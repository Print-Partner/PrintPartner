//! MCP `propose_create_build` and its `confirm_apply` on Rust storage
//! (GRE-1441 PR 1).
//!
//! Propose checks the arguments with [`NewBuild::parse`], runs storage's
//! checks in a read transaction and writes nothing. Confirm runs
//! `BuildGraphClient::create_with_brief`, which creates the Build, sets its
//! special request, stores its new planning brief and records the
//! idempotency key in one transaction. Both use the request's API key and
//! refuse while the Orders Module is off (provisional owner default).
//! Each `urls` entry is classified with [`pp_services::build_url`] and stored
//! as brief evidence. No Source is created.

use super::{
    checkoff::CheckoffDomain,
    write_result::{Code, Refusal, writer_failure},
};
use pp_services::{
    build_url::{self, UrlClass},
    mcp_domain::{ProposedAction, ToolInvocation},
};
use pp_storage::build_graph::{
    CreateBuildError, CreateUrl, CreateUrlKind, CreatedBuild, InvalidNewBuild, NewBuild,
};
use serde_json::{Map, Value, json};
use std::sync::atomic::AtomicBool;

pub(super) const PROPOSE: &str = "propose_create_build";

const NAME: &str = "name";
const REQUEST: &str = "request";
const URLS: &str = "urls";
const IDEMPOTENCY_KEY: &str = "idempotency_key";
const INVALID_KEY: Refusal = Refusal::new(
    Code::InvalidIdempotencyKey,
    "idempotency_key must be a string of at most 200 characters.",
);

impl CheckoffDomain {
    pub(super) fn create_build_propose(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolInvocation, String> {
        let refused = |refusal: Refusal| ToolInvocation {
            content: refusal.proposal(),
            proposed_action: None,
        };
        let new = match parse(args) {
            Ok(new) => new,
            Err(refusal) => return Ok(refused(refusal)),
        };
        let credential = self.caller()?.into_credential();
        if let Err(error) = self
            .builds
            .check_create_with_brief(credential, &new, self.wait)
        {
            return Ok(refused(storage_refusal(&error)));
        }
        let mut params = Map::new();
        params.insert(NAME.into(), new.name().into());
        params.insert(REQUEST.into(), new.request().into());
        if let Some(key) = new.idempotency_key() {
            params.insert(IDEMPOTENCY_KEY.into(), key.into());
        }
        if !new.urls().is_empty() {
            params.insert(
                URLS.into(),
                new.urls()
                    .iter()
                    .map(|url| Value::String(url.raw.clone()))
                    .collect(),
            );
        }
        let action = ProposedAction::new(
            PROPOSE,
            0,
            format!("Create Build {}", new.name()),
            "Create the Build with its planning brief and the customer request.",
            params,
        );
        Ok(ToolInvocation {
            content: action.proposed_content(),
            proposed_action: Some(action),
        })
    }

    pub(super) fn create_build_apply(&self, action: &ProposedAction) -> Result<Value, String> {
        let new = match parse(&action.params) {
            Ok(new) => new,
            Err(refusal) => return Ok(refusal.confirmation()),
        };
        let credential = self.caller()?.into_credential();
        Ok(
            match self
                .builds
                .create_with_brief(credential, new, &AtomicBool::new(false), self.wait)
            {
                Ok(CreatedBuild { id, name, replayed }) => json!({
                    "ok": true,
                    "result": { "plan_id": id.get(), "name": name, "idempotent_replay": replayed },
                }),
                Err(error) => storage_refusal(&error).confirmation(),
            },
        )
    }
}

/// The checked arguments, or the first refusal.
fn parse(args: &Map<String, Value>) -> Result<NewBuild, Refusal> {
    let text = |field: &str| match args.get(field) {
        Some(Value::String(text)) => Some(text.as_str()),
        _ => None,
    };
    let Some(name) = text(NAME) else {
        return Err(invalid(InvalidNewBuild::BlankName));
    };
    let Some(request) = text(REQUEST) else {
        return Err(invalid(InvalidNewBuild::BlankRequest));
    };
    let urls = read_urls(args)?;
    let key = match args.get(IDEMPOTENCY_KEY) {
        None | Some(Value::Null) => None,
        Some(Value::String(key)) => Some(key.as_str()),
        Some(_) => return Err(INVALID_KEY),
    };
    NewBuild::parse(name, request, key)
        .map(|new| new.with_urls(urls))
        .map_err(invalid)
}

/// `urls` absent, null, or a list of http(s) URLs. Anything else is refused
/// before a proposal exists.
fn read_urls(args: &Map<String, Value>) -> Result<Vec<CreateUrl>, Refusal> {
    let Some(value) = args.get(URLS) else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let Value::Array(urls) = value else {
        return Err(Refusal::new(
            Code::InvalidUrl,
            "urls must be a list of http or https URLs.",
        ));
    };
    urls.iter().map(one_url).collect()
}

fn one_url(value: &Value) -> Result<CreateUrl, Refusal> {
    let raw = value.as_str().ok_or_else(invalid_url)?;
    let classified = build_url::classify(raw).ok_or_else(invalid_url)?;
    let kind = match classified.class {
        UrlClass::Git { .. } => CreateUrlKind::Git,
        UrlClass::ModelPage => CreateUrlKind::ModelPage,
        UrlClass::Link => CreateUrlKind::Link,
    };
    Ok(CreateUrl {
        raw: raw.to_owned(),
        normalized: classified.normalized,
        kind,
    })
}

fn invalid_url() -> Refusal {
    Refusal::new(
        Code::InvalidUrl,
        "Each urls entry must be an http or https URL.",
    )
}

fn invalid(reason: InvalidNewBuild) -> Refusal {
    match reason {
        InvalidNewBuild::BlankName => {
            Refusal::new(Code::BlankName, "name must be a non-blank string.")
        }
        InvalidNewBuild::NameTooLong => Refusal::new(Code::NameTooLong, "name is too long."),
        InvalidNewBuild::BlankRequest => {
            Refusal::new(Code::BlankRequest, "request must be a non-blank string.")
        }
        InvalidNewBuild::IdempotencyKeyTooLong => INVALID_KEY,
    }
}

fn storage_refusal(error: &CreateBuildError) -> Refusal {
    match error {
        CreateBuildError::NameTaken => {
            Refusal::new(Code::NameTaken, "Another Build already has this name.")
        }
        CreateBuildError::OrdersOff => Refusal::new(
            Code::OrdersModuleOff,
            "The Orders Module is off. Turn it on in Settings to create a Build here.",
        ),
        CreateBuildError::ModuleSettingsInvalid => Refusal::new(
            Code::ModuleSettingsInvalid,
            "The stored Module settings are invalid. Repair them in Settings.",
        ),
        CreateBuildError::IdempotencyRecordInvalid => Refusal::new(
            Code::IdempotencyRecordInvalid,
            "The record for this idempotency_key is invalid. Use another key.",
        ),
        CreateBuildError::Writer(failure) => writer_failure(failure),
    }
}
