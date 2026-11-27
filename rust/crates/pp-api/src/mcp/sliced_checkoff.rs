use super::checkoff::CheckoffDomain;
use base64::{Engine, engine::general_purpose::STANDARD};
use pp_services::{
    checkoff_3mf,
    mcp_domain::{ProposedAction, ToolInvocation},
};
use pp_storage::{
    checkoff_progress::{
        Basis, Body, Request, Response,
        sliced_import::{self, Import, ObservedObject, Refusal},
    },
    read_model::AcceptedRead,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::sync::atomic::AtomicBool;

pub(super) const PROPOSE: &str = "propose_import_3mf_checkoff";
const UNAVAILABLE: &str = "Sliced 3MF Checkoff is unavailable";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input<'a> {
    plan_id: i64,
    filename: Option<&'a str>,
    content_base64: Option<&'a str>,
    source_id: Option<i64>,
    path: Option<&'a str>,
    integration_id: Option<&'a str>,
    printer_id: Option<&'a str>,
    host_name: Option<&'a str>,
}

impl CheckoffDomain {
    pub(super) fn propose_sliced_checkoff(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolInvocation, String> {
        let input = Input::deserialize(
            serde::de::value::MapDeserializer::<_, serde_json::Error>::new(
                args.iter().map(|(key, value)| (key.as_str(), value)),
            ),
        )
        .map_err(|_| "Invalid sliced 3MF arguments")?;
        let id = input.plan_id;
        if !(1..=9_007_199_254_740_991).contains(&id) {
            return Err("plan_id must be a positive whole number".into());
        }
        let cancel = AtomicBool::new(false);
        let caller = self.caller()?;
        let accepted = self
            .accepted
            .read(caller.clone().into_credential(), &[id], &cancel, self.wait)
            .map_err(|_| UNAVAILABLE)?
            .builds
            .pop()
            .ok_or("Plan not found")?
            .accepted;
        let AcceptedRead::Ready { snapshot } = accepted else {
            return Err("This Build has no available Accepted Plan".into());
        };
        let bytes_limit = checkoff_3mf::MAX_BYTES;
        let (bytes, filename) = match (input.content_base64, input.source_id, input.path) {
            (Some(inline), None, None) => {
                let inline = inline.trim();
                if inline.len() > bytes_limit.div_ceil(3) * 4 {
                    return Err("The 3MF exceeds the 64 MiB limit".into());
                }
                let bytes = STANDARD
                    .decode(inline)
                    .map_err(|_| "content_base64 must be valid base64")?;
                let filename = input.filename.unwrap_or("imported.3mf");
                (
                    bytes,
                    filename
                        .rsplit(['/', '\\'])
                        .next()
                        .unwrap_or_default()
                        .to_owned(),
                )
            }
            (None, Some(source_id), Some(path)) if source_id > 0 => {
                let path = path.trim();
                let bytes = self
                    .sources
                    .read_model_package(
                        caller.into_credential(),
                        source_id,
                        path,
                        bytes_limit,
                        &cancel,
                        self.wait,
                    )
                    .map_err(|error| match error {
                        pp_storage::source_files::PackageError::Limit => {
                            "The 3MF exceeds the 64 MiB limit"
                        }
                        _ => "The 3MF file is unavailable in that Source",
                    })?;
                (
                    bytes,
                    path.rsplit('/').next().unwrap_or_default().to_owned(),
                )
            }
            _ => {
                return Err(
                    "Provide content_base64 or both source_id and path, not both inputs".into(),
                );
            }
        };
        let mut filename = label(Some(&filename), "imported.3mf")?;
        if !filename.to_ascii_lowercase().ends_with(".3mf") {
            filename.push_str(".3mf");
        }
        let objects: Vec<_> = checkoff_3mf::inspect(&bytes)
            .map_err(|error| match error {
                checkoff_3mf::Refusal::Package => "File is not a valid 3MF package",
                checkoff_3mf::Refusal::Model => "The 3MF model is malformed",
                checkoff_3mf::Refusal::Limit => "The 3MF exceeds import limits",
                checkoff_3mf::Refusal::Unsupported => {
                    "The 3MF build uses an unsupported mesh layout"
                }
                checkoff_3mf::Refusal::Empty => "The 3MF contains no printable mesh objects",
            })?
            .into_iter()
            .map(|object| ObservedObject {
                id: object.id,
                name: object.name,
                part_number: object.part_number,
            })
            .collect();
        let attribution =
            sliced_import::preview(&snapshot, &objects).map_err(|error| error.to_string())?;
        let import = Import {
            expected: Basis::from(snapshot.as_ref()),
            reviewed: sliced_import::coordinates(&attribution),
            objects,
            filename: filename.clone(),
            integration_id: label(input.integration_id, "mcp-3mf")?,
            printer_id: label(input.printer_id, "mcp-3mf")?,
            host_name: label(input.host_name, "Imported 3MF")?,
        };
        let Value::Object(params) = serde_json::to_value(&import).map_err(|_| UNAVAILABLE)? else {
            return Err(UNAVAILABLE.into());
        };
        let action = ProposedAction::new(
            PROPOSE,
            id,
            format!("Import Checkoff for {filename}"),
            "Record this 3MF as awaiting verification without changing progress.",
            params,
        );
        let mut content: Value =
            serde_json::from_str(&action.proposed_content()).map_err(|_| UNAVAILABLE)?;
        content["attribution"] = json!(attribution);
        content["object_count"] = json!(import.objects.len());
        Ok(ToolInvocation {
            content: content.to_string(),
            proposed_action: Some(action),
        })
    }

    pub(super) fn apply_sliced_checkoff(&self, action: &ProposedAction) -> Result<Value, String> {
        let import: Import = serde_json::from_value(Value::Object(action.params.clone()))
            .map_err(|_| UNAVAILABLE)?;
        if import.expected.profile_id != action.plan_id {
            return Err(UNAVAILABLE.into());
        }
        let filename = import.filename.clone();
        let count = import.objects.len();
        let result = self.progress.import_sliced_3mf(
            self.caller()?.into_credential(),
            action.id.clone(),
            import,
            &AtomicBool::new(false),
            self.wait,
        );
        match result {
            Ok(mut result) => {
                result["source_filename"] = json!(filename);
                result["object_count"] = json!(count);
                Ok(json!({"ok": true, "result": result}))
            }
            Err(Refusal::Writer(_)) => Err(UNAVAILABLE.into()),
            Err(error) => Ok(json!({"ok": false, "detail": error.to_string()})),
        }
    }

    pub(super) fn reconcile_sliced_checkoff(
        &self,
        action_id: &str,
    ) -> Result<Option<Value>, String> {
        if action_id.len() != 36
            || !action_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        {
            return Ok(None);
        }
        match self.run(Request::CheckoffLink {
            id: sliced_import::link_id(action_id),
        })? {
            Response {
                status: 200,
                body: Body::Link { link },
            } => Ok(Some(
                json!({"ok": true, "reconciled": true, "action_id": action_id, "result": {"link": link}}),
            )),
            Response { status: 404, .. } => Ok(None),
            _ => Err(UNAVAILABLE.into()),
        }
    }
}

fn label(value: Option<&str>, default: &str) -> Result<String, &'static str> {
    let value = value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default);
    if value.len() > checkoff_3mf::MAX_LABEL_BYTES || value.chars().any(char::is_control) {
        return Err("Invalid sliced 3MF label");
    }
    Ok(value.to_owned())
}
