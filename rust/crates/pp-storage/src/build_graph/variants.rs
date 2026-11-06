//! `GET /plans/{id}/variant-dimensions` and `POST /plans/{id}/variant-selection`
//! (GRE-1221), Node's `services/variant-dimensions.ts` behind `desktop_rebuild`.
//!
//! Both read the base Source's checkout manifest outside any transaction, the
//! way the manifest builder does. A selection is checked against those
//! dimensions, then one writer transaction rechecks the Plan and its base
//! Source and stores the selection with the Source's import rules, as Node's
//! `savePlanVariantSelection` does.

use super::{BuildCommand, BuildGraphClient, BuildOutcome, build_exists, credential_tenant};
use crate::manifest_text::JsText;
use crate::working_drafts::observation::{
    CheckoutManifestUse, DimensionOption, Dimensions, DocumentRead, OwnedCheckoutReadRequest,
    PreparationBudget, ReadFailure, parse_variant_dimensions,
};
use crate::{catalog, read_model};
use anyhow::{Result, anyhow, bail};
use pp_domain::PositiveId;
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::{Map, Value, json};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

#[derive(Clone, Debug)]
pub enum VariantCommand {
    Read {
        build: PositiveId,
    },
    Select {
        build: PositiveId,
        request: Vec<u8>,
    },
    /// A checked selection for the writer; only this module builds one.
    Commit(Commit),
}

#[derive(Clone, Debug)]
pub struct Commit {
    build: i64,
    source: i64,
    selection: Vec<(String, String)>,
}

#[derive(Debug)]
pub enum VariantOutcome {
    /// The 200 JSON body.
    Body(Vec<u8>),
    MissingBuild,
    /// The request body is not JSON.
    Unreadable,
    /// Node's 400 `detail`.
    Refused(String),
    ObservationFailed {
        detail: String,
    },
}

const INVALID: &str = "Invalid variant selection";
const NO_BASE: &str = "No base source on plan";
const OTHER_SOURCE: &str = "Source must match the plan's base source";

fn refused(detail: impl Into<String>) -> Result<BuildOutcome> {
    Ok(BuildOutcome::Variants(VariantOutcome::Refused(
        detail.into(),
    )))
}

struct Captured {
    base: Option<i64>,
    path: Option<String>,
    stored: Option<String>,
}

fn base(tx: &Transaction<'_>, tenant: &str, build: i64) -> Result<Option<i64>> {
    Ok(tx
        .query_row(
            "SELECT project_id FROM profile_layers WHERE tenant_id=?1 AND profile_id=?2 AND layer_type='base' ORDER BY layer_order LIMIT 1",
            params![tenant, build],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten())
}

fn capture(tx: &Transaction<'_>, tenant: &str, build: i64) -> Result<Option<Captured>> {
    if !build_exists(tx, tenant, build)? {
        return Ok(None);
    }
    let base = base(tx, tenant, build)?;
    let path = match base {
        Some(source) => tx
            .query_row(
                "SELECT local_path FROM projects WHERE tenant_id=?1 AND id=?2",
                params![tenant, source],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten(),
        None => None,
    };
    let stored = tx
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
            params![tenant, format!("plan_variant_selection_{build}")],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    Ok(Some(Captured { base, path, stored }))
}

impl BuildGraphClient {
    pub(super) fn execute_variants(
        &self,
        credential: read_model::Credential,
        command: VariantCommand,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<BuildOutcome> {
        let (build, request) = match command {
            VariantCommand::Read { build } => (build.get() as i64, None),
            VariantCommand::Select { build, request } => (build.get() as i64, Some(request)),
            VariantCommand::Commit(_) => {
                let command = BuildCommand::Variants(command);
                return self.execute_with_reply(credential, command, None, cancelled, wait, false);
            }
        };
        let captured = {
            let mut checkout = self.settings.reader(wait)?;
            let tx = checkout
                .connection
                .as_mut()
                .ok_or_else(|| anyhow!("Reader checkout is closed"))?
                .transaction()?;
            let tenant = credential_tenant(&tx, &credential, self.policy)?;
            let captured = capture(&tx, &tenant, build)?;
            tx.commit()?;
            captured.map(|captured| (tenant, captured))
        };
        let Some((tenant, captured)) = captured else {
            return Ok(BuildOutcome::Variants(VariantOutcome::MissingBuild));
        };
        let mut budget = PreparationBudget::new(&cancelled, self.manifest_limits)?;
        let Some(request) = request else {
            let dimensions = match captured.base {
                Some(source) => {
                    match self.dimensions(&tenant, source, captured.path, &mut budget) {
                        Ok(dimensions) => dimensions,
                        Err(failure) => return observation_failed(failure),
                    }
                }
                None => Vec::new(),
            };
            let mut body = format!("{{\"profile_id\":{build},\"source_id\":").into_bytes();
            body.extend_from_slice(
                captured
                    .base
                    .map_or("null".into(), |id| id.to_string())
                    .as_bytes(),
            );
            body.extend_from_slice(b",\"dimensions\":");
            write_dimensions(&dimensions, &mut body);
            body.extend_from_slice(b",\"selection\":");
            body.extend_from_slice(stored_selection(captured.stored.as_deref()).as_bytes());
            body.push(b'}');
            return Ok(BuildOutcome::Variants(VariantOutcome::Body(body)));
        };
        let (selection, source) = match parse_request(&request) {
            Ok(parsed) => parsed,
            Err(outcome) => return Ok(BuildOutcome::Variants(outcome)),
        };
        let Some(base) = captured.base else {
            return refused(NO_BASE);
        };
        if source.is_some_and(|source| source != base) {
            return refused(OTHER_SOURCE);
        }
        let dimensions = match self.dimensions(&tenant, base, captured.path, &mut budget) {
            Ok(dimensions) => dimensions,
            Err(failure) => return observation_failed(failure),
        };
        for (dimension, value) in &selection {
            let key = JsText::scalar(dimension);
            let Some((_, options)) = dimensions.iter().find(|(name, _)| *name == key) else {
                return refused(format!("Unknown variant dimension: {dimension}"));
            };
            if !options.iter().any(|option| option.matches(value)) {
                return refused(format!(
                    "Unsupported option for variant dimension: {dimension}"
                ));
            }
        }
        let command = BuildCommand::Variants(VariantCommand::Commit(Commit {
            build,
            source: base,
            selection,
        }));
        self.execute_with_reply(credential, command, None, cancelled, wait, false)
    }

    /// Node's `getSourceVariantDimensions`: empty when the Source has no
    /// checkout, the manifest is missing or unreadable, or it does not load.
    fn dimensions(
        &self,
        tenant: &str,
        source: i64,
        path: Option<String>,
        budget: &mut PreparationBudget<'_>,
    ) -> std::result::Result<Dimensions, ReadFailure> {
        let Some(path) = path.filter(|path| !path.is_empty()) else {
            return Ok(Vec::new());
        };
        let reads = self
            .manifest_reads
            .as_deref()
            .ok_or(ReadFailure::Unavailable)?;
        let read = reads.read_checkout_manifest(
            &OwnedCheckoutReadRequest {
                tenant: tenant.into(),
                source_id: source,
                path,
            },
            CheckoutManifestUse::OptionGroups,
            budget,
        )?;
        let DocumentRead::Bytes(bytes) = read else {
            return Ok(Vec::new());
        };
        match parse_variant_dimensions(&bytes, budget) {
            Err(ReadFailure::InvalidDocument) => Ok(Vec::new()),
            other => other,
        }
    }
}

fn observation_failed(failure: ReadFailure) -> Result<BuildOutcome> {
    if matches!(failure, ReadFailure::Cancelled) {
        return Err(super::Failure::Cancelled.into());
    }
    Ok(BuildOutcome::Variants(VariantOutcome::ObservationFailed {
        detail: failure.to_string(),
    }))
}

fn write_dimensions(dimensions: &Dimensions, output: &mut Vec<u8>) {
    output.push(b'{');
    for (index, (name, options)) in dimensions.iter().enumerate() {
        if index > 0 {
            output.push(b',');
        }
        name.write_json(output);
        output.extend_from_slice(b":[");
        for (index, option) in options.iter().enumerate() {
            if index > 0 {
                output.push(b',');
            }
            DimensionOption::write_json(option, output);
        }
        output.push(b']');
    }
    output.push(b'}');
}

/// Object keys in JavaScript property order: array indexes ascending, then
/// the rest in insertion order.
fn js_order<T>(entries: &mut [(String, T)]) {
    entries.sort_by_key(|(key, _)| {
        JsText::scalar(key)
            .array_index()
            .map_or(u64::MAX, u64::from)
    });
}

/// `JSON.stringify(JSON.parse(raw))`, or `{}` for no setting, an empty one or
/// text that is not JSON, as Node's `getPlanVariantSelection` reads it.
fn stored_selection(raw: Option<&str>) -> String {
    let Some(value) = raw
        .filter(|raw| !raw.is_empty())
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
    else {
        return "{}".into();
    };
    let mut output = String::new();
    js_stringify(&value, &mut output);
    output
}

fn js_stringify(value: &Value, output: &mut String) {
    match value {
        Value::Number(number) => match number.as_f64().filter(|n| n.is_finite()) {
            Some(n) => output.push_str(ryu_js::Buffer::new().format(n)),
            None => output.push_str("null"),
        },
        Value::Array(items) => {
            output.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                js_stringify(item, output);
            }
            output.push(']');
        }
        Value::Object(fields) => {
            let mut fields: Vec<(String, &Value)> = fields
                .iter()
                .map(|(key, value)| (key.clone(), value))
                .collect();
            js_order(&mut fields);
            output.push('{');
            for (index, (key, value)) in fields.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output.push_str(&Value::String(key.clone()).to_string());
                output.push(':');
                js_stringify(value, output);
            }
            output.push('}');
        }
        other => output.push_str(&other.to_string()),
    }
}

type Request = (Vec<(String, String)>, Option<i64>);

/// Node's `variantSelectionBody` Zod schema over `request.body ?? {}`: a
/// `selection` record of strings (default `{}`) and an optional positive
/// safe-integer `source_id`.
fn parse_request(bytes: &[u8]) -> std::result::Result<Request, VariantOutcome> {
    const MAX_SAFE: u64 = 9_007_199_254_740_991;
    let invalid = || VariantOutcome::Refused(INVALID.into());
    if bytes.is_empty() {
        return Ok((Vec::new(), None));
    }
    let fields = match serde_json::from_slice::<Value>(bytes) {
        Err(_) => return Err(VariantOutcome::Unreadable),
        Ok(Value::Null) => return Ok((Vec::new(), None)),
        Ok(Value::Object(fields)) => fields,
        Ok(_) => return Err(invalid()),
    };
    let mut selection = match fields.get("selection") {
        None => Vec::new(),
        Some(Value::Object(entries)) => entries
            .iter()
            .map(|(key, value)| Some((key.clone(), value.as_str()?.to_owned())))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(invalid)?,
        Some(_) => return Err(invalid()),
    };
    js_order(&mut selection);
    let source = match fields.get("source_id") {
        None => None,
        Some(value) => {
            let id = match value.as_u64() {
                Some(id) => Some(id),
                None => value
                    .as_f64()
                    .filter(|id| id.fract() == 0.0 && *id >= 0.0 && *id <= MAX_SAFE as f64)
                    .map(|id| id as u64),
            };
            let id = id
                .filter(|id| (1..=MAX_SAFE).contains(id))
                .ok_or_else(invalid)?;
            Some(id as i64)
        }
    };
    Ok((selection, source))
}

/// The writer half of Node's `savePlanVariantSelection`.
pub(super) fn commit(
    tx: &Transaction<'_>,
    tenant: &str,
    command: &VariantCommand,
) -> Result<BuildOutcome> {
    let VariantCommand::Commit(commit) = command else {
        bail!("Variant command requires preparation");
    };
    if !build_exists(tx, tenant, commit.build)? {
        return refused("Profile not found");
    }
    if base(tx, tenant, commit.build)? != Some(commit.source) {
        return refused(OTHER_SOURCE);
    }
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE tenant_id=?1 AND id=?2)",
        params![tenant, commit.source],
        |row| row.get(0),
    )?;
    if !exists {
        return refused("Source not found");
    }
    let selection: Map<String, Value> = commit
        .selection
        .iter()
        .map(|(key, value)| (key.clone(), Value::String(value.clone())))
        .collect();
    tx.execute(
        "INSERT INTO app_settings(tenant_id,key,value) VALUES(?1,?2,?3) ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value",
        params![
            tenant,
            format!("plan_variant_selection_{}", commit.build),
            Value::Object(selection.clone()).to_string()
        ],
    )?;
    let rules = commit
        .selection
        .iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(_, value)| format!("{value}/"))
        .collect();
    let rules = catalog::save_import_rules(tx, tenant, commit.source, rules)?;
    let body = json!({
        "profile_id": commit.build,
        "source_id": commit.source,
        "rules": rules,
        "selection": selection,
    });
    Ok(BuildOutcome::Variants(VariantOutcome::Body(
        serde_json::to_vec(&body)?,
    )))
}
