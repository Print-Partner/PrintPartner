//! MCP `propose_import_build_inputs` and its `confirm_apply` on Rust storage
//! (GRE-1300 parts 2 and 3a): Library Sources by `source_id`, and model pages
//! and other links by `url`. A git repository URL is refused until part 3b.
//!
//! Propose checks `plan_id` and every input, classifying each `url` with
//! `pp_services::build_url` (no network call), then proves the API key and
//! runs `BuildGraphClient::check_import_inputs`, which finds the brief, each
//! Source and each `derived_from_evidence_id`. It writes nothing. Confirm
//! checks the stored params again and runs `BuildGraphClient::import_inputs`,
//! which puts one evidence item per input in the brief and clears its
//! selected draft in one transaction. It creates and changes no Source.

use super::{
    checkoff::CheckoffDomain,
    source_roles::{BRIEF_INVALID, INVALID_PLAN, NO_BRIEF, brief_proposal, plan},
    write_result::{Code, Refusal, writer_failure},
};
use pp_services::{
    build_url::{self, UrlClass},
    mcp_domain::{ProposedAction, ToolInvocation},
};
use pp_storage::{
    build_graph::{ImportInput, ImportInputsError, Imported, Page, Target},
    read_model::planning::EvidenceKind,
};
use serde_json::{Map, Value, json};
use std::sync::atomic::AtomicBool;

pub(super) const PROPOSE: &str = "propose_import_build_inputs";

const INPUTS: &str = "inputs";
const INVALID_INPUTS: Refusal = Refusal::new(
    Code::InvalidInputs,
    "inputs must be a non-empty list of { source_id or url, kind?, title?, extract?, filenames?, derived_from_evidence_id? }.",
);
/// Confirm adds the unknown id as `source_id`.
const SOURCE_NOT_FOUND: Refusal = Refusal::new(
    Code::SourceNotFound,
    "A source_id names no Library Source. Upload files into a Source first, then pass its id.",
);
/// Confirm adds the unknown id as `evidence_id`.
const DERIVED_NOT_FOUND: Refusal = Refusal::new(
    Code::DerivedEvidenceNotFound,
    "A derived_from_evidence_id names no evidence in this Build's planning brief.",
);

impl CheckoffDomain {
    pub(super) fn import_inputs_propose(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolInvocation, String> {
        let Some(build) = plan(args) else {
            return Ok(INVALID_PLAN.invocation());
        };
        let inputs = match inputs(args) {
            Ok(inputs) => inputs,
            Err(refusal) => return Ok(refusal.invocation()),
        };
        let credential = self.caller()?.into_credential();
        if let Err(error) = self
            .builds
            .check_import_inputs(credential, build, inputs, self.wait)
        {
            return Ok(storage_refusal(&error).invocation());
        }
        brief_proposal(
            PROPOSE,
            build,
            (INPUTS, args[INPUTS].clone()),
            "Attach evidence to Build",
            "Attach Library Sources and pages to the Build's planning brief and clear its selected draft. No Source changes.",
        )
    }

    pub(super) fn import_inputs_apply(&self, action: &ProposedAction) -> Result<Value, String> {
        let Some(build) = plan(&action.params) else {
            return Ok(INVALID_PLAN.confirmation());
        };
        let inputs = match inputs(&action.params) {
            Ok(inputs) => inputs,
            Err(refusal) => return Ok(refusal.confirmation()),
        };
        let credential = self.caller()?.into_credential();
        let cancelled = AtomicBool::new(false);
        Ok(
            match self
                .builds
                .import_inputs(credential, build, inputs, &cancelled, self.wait)
            {
                Ok(imported) => json!({
                    "ok": true,
                    "result": {
                        "evidence": imported.iter().map(item).collect::<Vec<_>>(),
                        "created_source_ids": [],
                    },
                }),
                Err(ImportInputsError::SourceNotFound(id)) => {
                    let mut refused = SOURCE_NOT_FOUND.confirmation();
                    refused["source_id"] = id.into();
                    refused
                }
                Err(ImportInputsError::DerivedEvidenceNotFound(id)) => {
                    let mut refused = DERIVED_NOT_FOUND.confirmation();
                    refused["evidence_id"] = id.into();
                    refused
                }
                Err(error) => storage_refusal(&error).confirmation(),
            },
        )
    }
}

/// The checked inputs, or the first refusal.
fn inputs(args: &Map<String, Value>) -> Result<Vec<ImportInput>, Refusal> {
    let rows = args
        .get(INPUTS)
        .and_then(Value::as_array)
        .filter(|rows| !rows.is_empty())
        .ok_or(INVALID_INPUTS)?;
    rows.iter().map(input).collect()
}

fn input(row: &Value) -> Result<ImportInput, Refusal> {
    let one_of = Refusal::new(
        Code::InvalidInput,
        "Each inputs item must be an object with either a source_id or a url.",
    );
    let row = row.as_object().ok_or(one_of)?;
    let given = |field: &str| row.get(field).filter(|value| !value.is_null());
    let target = match (given("source_id"), given("url")) {
        (Some(_), None) => source(row)?,
        (None, Some(url)) => page(url)?,
        _ => return Err(one_of),
    };
    let kind = match row.get("kind") {
        None | Some(Value::Null) => None,
        Some(kind) => Some(kind.as_str().and_then(EvidenceKind::from_wire).ok_or(
            Refusal::new(
                Code::InvalidEvidenceKind,
                "kind must be canonical_design, vendor_overlay, mod, component, model_source or informational_evidence.",
            ),
        )?),
    };
    let text = |field: &str| row.get(field).and_then(Value::as_str).map(str::to_owned);
    Ok(ImportInput {
        target,
        kind,
        title: text("title"),
        extract: text("extract"),
        derived_from_evidence_id: text("derived_from_evidence_id"),
    })
}

fn source(row: &Map<String, Value>) -> Result<Target, Refusal> {
    let id = super::positive_id(&Value::Object(row.clone()), "source_id").ok_or(Refusal::new(
        Code::InvalidInput,
        "source_id must name a Library Source by its positive number.",
    ))?;
    let filenames = row
        .get("filenames")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Ok(Target::Source { id, filenames })
}

/// A model page or link, classified as Node classifies it.
fn page(url: &Value) -> Result<Target, Refusal> {
    let url = url.as_str().unwrap_or_default();
    let classified = build_url::classify(url).ok_or(Refusal::new(
        Code::InvalidUrl,
        "url must be an http or https URL.",
    ))?;
    let page = match classified.class {
        UrlClass::ModelPage => Page::Model,
        UrlClass::Link => Page::Link,
        UrlClass::Git { .. } => {
            return Err(Refusal::new(
                Code::UrlInputsNotSupported,
                "Git repository URLs are not supported yet. Add the repository as a Library Source, then attach it by source_id.",
            ));
        }
    };
    Ok(Target::Url {
        url: url.to_owned(),
        normalized: classified.normalized,
        page,
    })
}

fn item(imported: &Imported) -> Value {
    json!({
        "id": imported.evidence_id,
        "kind": imported.kind.as_str(),
        "source_id": imported.source_id,
        "url": imported.url,
        "upload_required": imported.upload_required,
    })
}

fn storage_refusal(error: &ImportInputsError) -> Refusal {
    match error {
        ImportInputsError::NoBrief => NO_BRIEF,
        ImportInputsError::BriefInvalid => BRIEF_INVALID,
        ImportInputsError::SourceNotFound(_) => SOURCE_NOT_FOUND,
        ImportInputsError::DerivedEvidenceNotFound(_) => DERIVED_NOT_FOUND,
        ImportInputsError::Writer(failure) => writer_failure(failure),
    }
}
