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
use crate::source_scan::node_json::{js_members, js_string};
use crate::working_drafts::observation::{
    CheckoutManifestUse, DimensionOption, Dimensions, DocumentRead, OwnedCheckoutReadRequest,
    PreparationBudget, ReadFailure, parse_variant_dimensions,
};
use crate::{catalog, read_model};
use anyhow::{Result, anyhow, bail};
use pp_domain::PositiveId;
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::value::RawValue;
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
    selection: Vec<(JsText, JsText)>,
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
            let Some((_, options)) = dimensions.iter().find(|(name, _)| name == dimension) else {
                let dimension = String::from_utf16_lossy(dimension.units());
                return refused(format!("Unknown variant dimension: {dimension}"));
            };
            if !options.iter().any(|option| option.matches(value)) {
                let dimension = String::from_utf16_lossy(dimension.units());
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

/// `JSON.stringify(JSON.parse(raw))`, or `{}` for no setting, an empty one or
/// text that is not JSON, as Node's `getPlanVariantSelection` reads it. Lone
/// surrogate escapes survive, as in JavaScript.
fn stored_selection(raw: Option<&str>) -> String {
    raw.filter(|raw| !raw.is_empty())
        .and_then(crate::source_scan::node_json::reserialize)
        .unwrap_or_else(|| "{}".into())
}

type Request = (Vec<(JsText, JsText)>, Option<i64>);

/// Node's `variantSelectionBody` Zod schema over `request.body ?? {}`: a
/// `selection` record of strings (default `{}`) and an optional positive
/// safe-integer `source_id`. Keys and values stay UTF-16, lone surrogates
/// included, in JavaScript property order.
fn parse_request(bytes: &[u8]) -> std::result::Result<Request, VariantOutcome> {
    let invalid = || VariantOutcome::Refused(INVALID.into());
    if bytes.is_empty() {
        return Ok((Vec::new(), None));
    }
    let Some(body) = std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| serde_json::from_str::<Box<RawValue>>(text).ok())
    else {
        return Err(VariantOutcome::Unreadable);
    };
    if body.get() == "null" {
        return Ok((Vec::new(), None));
    }
    let fields = js_members(body.get()).ok_or_else(invalid)?;
    let field = |name: &str| {
        let name = JsText::scalar(name);
        fields
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.get())
    };
    let selection = match field("selection") {
        None => Vec::new(),
        Some(entries) => js_members(entries)
            .ok_or_else(invalid)?
            .into_iter()
            .map(|(key, value)| Some((key, js_string(value.get())?)))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(invalid)?,
    };
    let source = match field("source_id") {
        None => None,
        Some(value) => Some(
            serde_json::from_str::<PositiveId>(value)
                .map_err(|_| invalid())?
                .get() as i64,
        ),
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
    let mut selection = vec![b'{'];
    for (index, (key, value)) in commit.selection.iter().enumerate() {
        if index > 0 {
            selection.push(b',');
        }
        key.write_json(&mut selection);
        selection.push(b':');
        value.write_json(&mut selection);
    }
    selection.push(b'}');
    let selection = String::from_utf8(selection)?;
    tx.execute(
        "INSERT INTO app_settings(tenant_id,key,value) VALUES(?1,?2,?3) ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value",
        params![
            tenant,
            format!("plan_variant_selection_{}", commit.build),
            selection
        ],
    )?;
    let rules = commit
        .selection
        .iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(_, value)| format!("{}/", String::from_utf16_lossy(value.units())))
        .collect();
    let rules = catalog::save_import_rules(tx, tenant, commit.source, rules)?;
    let body = format!(
        "{{\"profile_id\":{},\"source_id\":{},\"rules\":{},\"selection\":{selection}}}",
        commit.build,
        commit.source,
        serde_json::to_string(&rules)?
    );
    Ok(BuildOutcome::Variants(VariantOutcome::Body(
        body.into_bytes(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lone_surrogates_survive_the_request_and_match_their_option() {
        let body = br#"{"source_id":2.0,"selection":{"b":"\udc00x","10":"\ud800","b":"\ud800"}}"#;
        let Ok((selection, source)) = parse_request(body) else {
            panic!("selection refused");
        };
        assert_eq!(source, Some(2));
        let units: Vec<_> = selection
            .iter()
            .map(|(key, value)| (key.units().to_vec(), value.units().to_vec()))
            .collect();
        assert_eq!(
            units,
            [(vec![0x31, 0x30], vec![0xd800]), (vec![0x62], vec![0xd800])]
        );
        let option = DimensionOption::Text(JsText::from_units(vec![0xd800]));
        assert!(option.matches(&selection[0].1));
        assert!(!option.matches(&JsText::scalar("\u{fffd}")));
    }

    #[test]
    fn adjacent_lone_surrogates_each_survive() {
        let body = br#"{"selection":{"\ud800\ud801":"\udc00\ud800x\udbff"}}"#;
        let Ok((selection, _)) = parse_request(body) else {
            panic!("selection refused");
        };
        assert_eq!(selection[0].0.units(), [0xd800, 0xd801]);
        assert_eq!(selection[0].1.units(), [0xdc00, 0xd800, 0x78, 0xdbff]);
        assert_eq!(
            stored_selection(Some(r#"{"\ud800\ud801":"\udc00\udbff"}"#)),
            r#"{"\ud800\ud801":"\udc00\udbff"}"#
        );
    }

    #[test]
    fn stored_selection_keeps_lone_surrogate_escapes() {
        assert_eq!(
            stored_selection(Some(r#"{"b":"\uD800","2":1.0}"#)),
            r#"{"2":1,"b":"\ud800"}"#
        );
        assert_eq!(stored_selection(Some("not json")), "{}");
        assert_eq!(stored_selection(Some("")), "{}");
    }
}
