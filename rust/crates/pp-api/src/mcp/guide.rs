use super::{
    pending_actions::Decision,
    web_discovery,
    write_result::{self, Code, Refusal},
    *,
};
use pp_services::{guide, mcp_domain::ProposedAction};
use pp_storage::{
    pending_actions::StagedBy,
    source_notes::{Draft, Source},
};
use serde::Deserialize;

pub(super) const URL: &str = "ingest_guide_url";
pub(super) const TEXT: &str = "ingest_guide_text";
pub(super) const IMPORT: &str = "import_guide_notes";

pub(super) fn describe(catalog: &mut Value) {
    let Some(tools) = catalog["tools"].as_array_mut() else {
        return;
    };
    for tool in tools {
        let (description, properties) = match tool["name"].as_str() {
            Some(URL) => (
                "Fetch one public guide URL as untrusted evidence. No embedded links are followed and nothing is saved.",
                json!({"url": {"type": "string", "maxLength": guide::MAX_URL_BYTES}}),
            ),
            Some(TEXT) => (
                "Extract untrusted evidence from pasted text or Markdown. Nothing is saved and no network request is made.",
                json!({"text": {"type": "string", "maxLength": guide::MAX_GUIDE_BYTES}}),
            ),
            Some(IMPORT) => (
                "Propose saving untrusted guide evidence as a Source note. Ask for the exact Source name shown in the Library and pass the guide URL when known. Requires Apply.",
                json!({
                    "source_name": {"type": "string"},
                    "title": {"type": "string", "description": "Defaults to Guide: <Source>", "maxLength": pp_storage::source_notes::MAX_TITLE_BYTES},
                    "body_markdown": {"type": "string", "maxLength": pp_storage::source_notes::MAX_BODY_BYTES},
                    "guide_url": {"type": "string", "description": "Optional origin of the evidence; saved with the note and never fetched.", "maxLength": guide::MAX_URL_BYTES},
                }),
            ),
            _ => continue,
        };
        tool["description"] = json!(description);
        tool["inputSchema"]["properties"] = properties;
    }
}

pub(super) async fn ingest(
    tool: &str,
    args: &Value,
    web: Option<&dyn web_discovery::Discovery>,
) -> ToolResult {
    let page = if tool == URL {
        let Some(web) = web else {
            return ingest_error("disabled", "Guide URL intake is disabled.");
        };
        let Some(url) = args["url"]
            .as_str()
            .and_then(|url| guide::web_reference(url.trim()))
        else {
            return ingest_error(
                "invalid_input",
                "Pass a public HTTP(S) guide URL within the length limit.",
            );
        };
        match web.fetch(url.as_str()).await {
            Ok(page) if !page.truncated => Some(page),
            Ok(_) => return text_too_large(),
            Err(error) => return error.result(),
        }
    } else {
        None
    };
    let raw = page
        .as_ref()
        .map(|page| page.text.as_str())
        .or_else(|| args["text"].as_str())
        .unwrap_or_default();
    let text = match guide::validate_text(raw, guide::MAX_GUIDE_BYTES) {
        Ok(text) => text,
        Err(guide::InvalidText::TooLarge) => return text_too_large(),
        Err(guide::InvalidText::EmptyOrBinary) => {
            return ingest_error(
                "invalid_input",
                "Pass nonempty guide text without binary control characters.",
            );
        }
    };
    let links = page
        .as_ref()
        .map(|page| page.links.as_slice())
        .unwrap_or_default();
    let extract = guide::extract(text, links);
    let end = text.floor_char_boundary(12_000);
    let mut body = json!({
        "ok": true, "banner": guide::BANNER, "extract_method": "heuristic",
        "untrusted_text": &text[..end], "truncated": end < text.len(), "extract": extract,
    });
    if let Some(page) = page {
        body["url"] = json!(page.url);
    }
    ToolResult::ok(body.to_string())
}

fn ingest_error(code: &str, detail: &str) -> ToolResult {
    ToolResult::error(json!({"ok": false, "code": code, "error": detail}))
}

fn text_too_large() -> ToolResult {
    ingest_error("input_too_large", "Guide text exceeds 48,000 bytes.")
}

#[derive(Deserialize)]
struct ImportArgs {
    source_name: String,
    title: Option<String>,
    body_markdown: String,
    guide_url: Option<String>,
}

#[derive(Deserialize)]
struct ConfirmArgs {
    source: Source,
    title: String,
    body_markdown: String,
}

const INVALID_NOTE: Refusal = Refusal::new(
    Code::InvalidProposal,
    "Pass a Source, a nonempty note body of at most 20,000 bytes, and an optional title of at most 300 bytes.",
);
const MISSING_SOURCE: Refusal = Refusal::new(
    Code::SourceNotFound,
    "Source not found or changed. Ask for its current name as shown in the Library.",
);

fn refuse(refusal: Refusal) -> ToolResult {
    ToolResult::error(json!({"error": refusal.detail, "code": refusal.code.as_str()}))
}

impl ProductMcp {
    pub(super) async fn propose_guide_note(
        &self,
        args: Value,
        presented: PresentedCredential,
        staged_by: StagedBy,
    ) -> ToolResult {
        let Ok(args) = serde_json::from_value::<ImportArgs>(args) else {
            return refuse(INVALID_NOTE);
        };
        let name = args.source_name.trim();
        if name.is_empty() {
            return refuse(INVALID_NOTE);
        }
        let default_title = format!("Guide: {name}");
        let title = args
            .title
            .as_deref()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or(&default_title);
        let Some(draft) = Draft::new(title, &args.body_markdown) else {
            return refuse(INVALID_NOTE);
        };
        let draft = match args.guide_url {
            Some(raw_url) => {
                let Some(url) = guide::web_reference(raw_url.trim()) else {
                    return refuse(Refusal::new(
                        Code::InvalidUrl,
                        "Pass an HTTP(S) guide URL without credentials.",
                    ));
                };
                let body = format!("{}\n\nGuide URL: {url}", draft.body_markdown());
                let Some(draft) = Draft::new(draft.title(), &body) else {
                    return refuse(INVALID_NOTE);
                };
                draft
            }
            None => draft,
        };
        let client = self.reads.accepted.source_notes();
        let credential = presented.clone().into_credential();
        let wait = self.reads.wait;
        let name = name.to_owned();
        let source =
            match tokio::task::spawn_blocking(move || client.find_source(credential, &name, wait))
                .await
            {
                Ok(Ok(Some(source))) => source,
                Ok(Ok(None)) => return refuse(MISSING_SOURCE),
                Ok(Err(error)) => return refuse(write_result::writer_failure(&error)),
                Err(_) => {
                    return refuse(Refusal::new(
                        Code::Unavailable,
                        "Source lookup is unavailable.",
                    ));
                }
            };
        let Value::Object(params) = json!({"source": source, "title": draft.title(), "body_markdown": draft.body_markdown()})
        else {
            return refuse(INVALID_NOTE);
        };
        let action = ProposedAction::new(
            IMPORT,
            0,
            format!(
                "Save note “{}” on Source “{}”",
                draft.title(),
                source.name()
            ),
            "Save untrusted guide evidence, not assistant policy.",
            params,
        );
        let result = ToolResult::ok(action.proposed_content());
        self.stage_proposal(action, presented, staged_by, result)
            .await
    }

    pub(super) async fn confirm_guide_note(
        &self,
        action: ProposedAction,
        presented: PresentedCredential,
    ) -> Decision {
        let Ok(params) = serde_json::from_value::<ConfirmArgs>(Value::Object(action.params)) else {
            return Decision::confirmed(INVALID_NOTE.confirmation());
        };
        let Some(draft) = Draft::new(&params.title, &params.body_markdown) else {
            return Decision::confirmed(INVALID_NOTE.confirmation());
        };
        let client = self.reads.accepted.source_notes();
        let wait = self.reads.wait;
        let (id, name, title) = (
            params.source.id(),
            params.source.name().to_owned(),
            draft.title().to_owned(),
        );
        let body = match tokio::task::spawn_blocking(move || {
            client.import(presented.into_credential(), params.source, draft, wait)
        })
        .await
        {
            Ok(Ok(Some(changed))) => json!({"ok": true, "result": {
                "source_id": id, "source_name": name, "title": title, "upserted": changed,
            }}),
            Ok(Ok(None)) => MISSING_SOURCE.confirmation(),
            Ok(Err(error)) => {
                let refusal = write_result::writer_failure(&error);
                if refusal.code == Code::OutcomeUnknown {
                    return unknown();
                }
                refusal.confirmation()
            }
            Err(_) => return unknown(),
        };
        Decision::confirmed(body)
    }
}

fn unknown() -> Decision {
    Decision::unknown("The result is unknown. Check the Source note before proposing again.")
}
