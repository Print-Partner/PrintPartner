//! MCP `propose_create_build` and its `confirm_apply` on Rust storage
//! (GRE-1441 PR 1).
//!
//! Propose checks the arguments with [`NewBuild::parse`], runs storage's
//! checks in a read transaction and writes nothing. Confirm runs
//! `BuildGraphClient::create_with_brief`, which creates the Build, sets its
//! special request, stores its new planning brief and records the
//! idempotency key in one transaction. Both use the request's API key and
//! refuse while the Orders Module is off (provisional owner default).
//! `urls` must be absent or empty until PR 2.

use super::{
    checkoff::CheckoffDomain,
    write_result::{Code, Refusal, writer_failure},
};
use pp_services::mcp_domain::{ProposedAction, ToolInvocation};
use pp_storage::build_graph::{CreateBuildError, CreatedBuild, InvalidNewBuild, NewBuild};
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
    match args.get(URLS) {
        None | Some(Value::Null) => {}
        Some(Value::Array(urls)) if urls.is_empty() => {}
        Some(_) => {
            return Err(Refusal::new(
                Code::UrlsNotSupported,
                "urls are not supported yet. Create the Build without them, then add Sources.",
            ));
        }
    }
    let key = match args.get(IDEMPOTENCY_KEY) {
        None | Some(Value::Null) => None,
        Some(Value::String(key)) => Some(key.as_str()),
        Some(_) => return Err(INVALID_KEY),
    };
    NewBuild::parse(name, request, key).map_err(invalid)
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
