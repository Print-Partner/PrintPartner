use super::AcceptedRead;
#[cfg(feature = "desktop_rebuild")]
pub(crate) use super::context::{
    AcceptedProgressFacts, BuildSummaryFacts, FreshnessFacts, StaleReasonFacts,
    UntrackedReasonFacts,
};
#[cfg(feature = "desktop_rebuild")]
use crate::manifest_text::{
    DraftPart, ManifestText, OptionGroupId, PartManifestMetadata, read_manifest_text,
};

#[cfg(feature = "desktop_rebuild")]
pub(crate) fn build_summary(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    repos: &std::path::Path,
    budget: &mut Budget,
) -> Result<Option<crate::build_graph::ProfileSummary>> {
    let accepted = super::accepted_read(tx, tenant, profile, repos, budget)?;
    if matches!(accepted, super::AcceptedRead::Missing) {
        return Ok(None);
    }
    let facts = super::context::capture_build_summary(tx, tenant, profile, &accepted, budget)?
        .ok_or_else(|| anyhow::anyhow!("Build context missing"))?;
    Ok(Some(crate::build_graph::projection::profile_summary(facts)))
}

use anyhow::{Result, anyhow};
use rusqlite::{Transaction, types::ValueRef};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
};

#[derive(Debug)]
pub(crate) struct Integrity {
    pub code: &'static str,
    pub message: String,
}
impl std::fmt::Display for Integrity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for Integrity {}
pub(super) fn check(ok: bool, code: &'static str, message: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(Integrity {
            code,
            message: message.into(),
        }
        .into())
    }
}
#[cfg(feature = "desktop_rebuild")]
pub(super) fn budget_check(ok: bool, resource: crate::build_graph::BudgetResource) -> Result<()> {
    anyhow::ensure!(
        ok,
        crate::build_graph::Failure::BudgetExhausted { resource }
    );
    Ok(())
}
#[derive(Default)]
pub(crate) struct Budget {
    rows: usize,
    bytes: usize,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub id: i64,
    pub name: String,
    pub order_number: Option<String>,
    pub special_request: Option<String>,
    pub archived_at: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub format: &'static str,
    pub profile: Profile,
    pub plan_version: i64,
    pub revision_id: i64,
    pub revision_number: i64,
    pub revision_digest: String,
    pub accepted_at: String,
    pub provenance: Provenance,
    pub required_unit_mapping_digest: String,
    pub parts: Vec<Part>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Provenance {
    Legacy,
    Tracked {
        #[serde(rename = "inputSetId")]
        input_set_id: i64,
        #[serde(rename = "inputSetDigest")]
        input_set_digest: String,
        inputs: Vec<Input>,
    },
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Input {
    pub input_id: i64,
    pub source_id: i64,
    pub source_layer: String,
    pub layer_order: i64,
    pub effective_naming_digest: String,
    pub tracking_kind: String,
    pub source_revision_id: Option<i64>,
    pub manifest_digest: Option<String>,
    pub snapshot_root: Option<String>,
    /// `repos/<locator>` before links resolve, which Node's artifact reads walk.
    #[serde(skip)]
    pub snapshot_path: Option<String>,
    pub source_synced_at: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Artifact {
    Tracked {
        #[serde(rename = "sourceId")]
        source_id: i64,
        #[serde(rename = "sourceRevisionId")]
        source_revision_id: i64,
        #[serde(rename = "snapshotRoot")]
        snapshot_root: String,
        /// `repos/<locator>` before links resolve.
        #[serde(skip)]
        snapshot_path: String,
        #[serde(rename = "relativePath")]
        relative_path: String,
        #[serde(rename = "expectedSha256")]
        expected_sha256: String,
    },
    Unavailable {
        reason: &'static str,
    },
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Unit {
    pub unit_index: i64,
    pub required: bool,
    pub token: String,
    pub object_name: String,
    pub completed: bool,
    pub assembled: bool,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Part {
    pub revision_part_id: i64,
    pub projection_part_id: i64,
    pub part_key: String,
    pub relative_path: String,
    pub filename: String,
    pub source_layer: String,
    pub status: String,
    pub role_inferred: String,
    pub role_override: Option<String>,
    pub effective_role: String,
    pub filament_color_id: Option<String>,
    pub filament_custom_hex: Option<String>,
    pub spoolman_spool_id: Option<String>,
    pub quantity_inferred: i64,
    pub quantity_override: Option<i64>,
    pub quantity_effective: i64,
    pub included: bool,
    pub notes: String,
    pub github_blob_url: Option<String>,
    pub geometry_same: Option<bool>,
    pub requirement: Option<String>,
    pub option_group_id: Option<String>,
    pub manifest_source: Option<String>,
    /// Lossless manifest metadata. The scalar fields above are `None` when
    /// the stored text is not valid Unicode; JSON bodies write this instead.
    #[cfg(feature = "desktop_rebuild")]
    #[serde(skip)]
    pub manifest: PartManifestMetadata,
    pub artifact: Artifact,
    pub units: Vec<Unit>,
}
#[derive(Clone)]
pub(super) struct Row(
    BTreeMap<String, Value>,
    &'static str,
    #[cfg(feature = "desktop_rebuild")] PartManifestMetadata,
);
impl Row {
    pub(super) fn same_field(&self, other: &Self, key: &str) -> bool {
        #[cfg(feature = "desktop_rebuild")]
        match key {
            "requirement" => return self.2.requirement == other.2.requirement,
            "option_group_id" => return self.2.option_group_id == other.2.option_group_id,
            _ => {}
        }
        self.v(key) == other.v(key)
    }

    pub(super) fn v(&self, k: &str) -> &Value {
        self.0.get(k).unwrap_or(&Value::Null)
    }
    pub(super) fn s(&self, k: &str) -> Result<&str> {
        self.v(k).as_str().ok_or_else(|| {
            anyhow!(Integrity {
                code: self.1,
                message: format!("Invalid stored text {k}")
            })
        })
    }
    pub(super) fn n(&self, k: &str) -> Result<i64> {
        self.v(k)
            .as_i64()
            .filter(|n| n.unsigned_abs() <= 9_007_199_254_740_991)
            .ok_or_else(|| {
                anyhow!(Integrity {
                    code: self.1,
                    message: format!("Invalid stored number {k}")
                })
            })
    }
    pub(super) fn os(&self, k: &str) -> Result<Option<String>> {
        if self.v(k).is_null() {
            Ok(None)
        } else {
            Ok(Some(self.s(k)?.into()))
        }
    }
    pub(super) fn on(&self, k: &str) -> Result<Option<i64>> {
        if self.v(k).is_null() {
            Ok(None)
        } else {
            Ok(Some(self.n(k)?))
        }
    }
    pub(super) fn b(&self, k: &str) -> Result<bool> {
        let n = self.n(k)?;
        check(
            n == 0 || n == 1,
            "projection",
            "Accepted Plan Part booleans or ownership are corrupt",
        )?;
        Ok(n == 1)
    }
    pub(super) fn ob(&self, k: &str) -> Result<Option<bool>> {
        if self.v(k).is_null() {
            Ok(None)
        } else {
            Ok(Some(self.b(k)?))
        }
    }
}
pub(super) fn rows(
    tx: &Transaction<'_>,
    table: &str,
    condition: &str,
    args: &[&dyn rusqlite::ToSql],
    code: &'static str,
    budget: &mut Budget,
) -> Result<Vec<Row>> {
    let mut metadata = tx.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = metadata
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let expression = columns
        .iter()
        .map(|c| {
            #[cfg(feature = "desktop_rebuild")]
            if matches!(c.as_str(), "requirement" | "option_group_id") {
                return format!("coalesce(length(cast(\"{c}\" AS blob)),0)");
            }
            format!(
                "CASE WHEN typeof(\"{c}\")='text' THEN length(cast(\"{c}\" AS blob)) ELSE 0 END"
            )
        })
        .collect::<Vec<_>>()
        .join("+");
    let (count,total,max):(i64,i64,i64)=tx.query_row(&format!("SELECT count(*),coalesce(sum({expression}),0),coalesce(max({expression}),0) FROM {table} WHERE {condition}"),args,|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    check(
        max <= 65536,
        code,
        "Accepted Plan stored row text is corrupt",
    )?;
    budget.rows = budget.rows.saturating_add(count as usize);
    budget.bytes = budget.bytes.saturating_add(total as usize);
    #[cfg(feature = "desktop_rebuild")]
    {
        use crate::build_graph::BudgetResource;
        budget_check(budget.rows <= 200_000, BudgetResource::GraphRows)?;
        budget_check(budget.bytes <= 64 * 1024 * 1024, BudgetResource::GraphBytes)?;
    }
    #[cfg(not(feature = "desktop_rebuild"))]
    check(
        budget.rows <= 200_000 && budget.bytes <= 64 * 1024 * 1024,
        code,
        "Accepted Plan batch read budget exceeded",
    )?;
    let mut stmt = tx.prepare(&format!("SELECT * FROM {table} WHERE {condition}"))?;
    let names = stmt
        .column_names()
        .iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>();
    let result = stmt
        .query_map(args, |r| {
            let mut values = BTreeMap::new();
            #[cfg(feature = "desktop_rebuild")]
            let mut manifest = PartManifestMetadata::default();
            for (i, name) in names.iter().enumerate() {
                #[cfg(feature = "desktop_rebuild")]
                if matches!(table, "parts" | "plan_revision_parts" | "plan_draft_parts") {
                    match name.as_str() {
                        "requirement" => {
                            manifest.requirement = read_manifest_text(r.get_ref(i)?)
                                .map_err(|error| {
                                    rusqlite::Error::ToSqlConversionFailure(Box::new(Integrity {
                                        code: "manifest_text",
                                        message: error.to_string(),
                                    }))
                                })?
                                .map(ManifestText);
                            values.insert(
                                name.clone(),
                                Value::from(
                                    manifest.requirement.as_ref().and_then(|v| v.0.as_scalar()),
                                ),
                            );
                            continue;
                        }
                        "option_group_id" => {
                            manifest.option_group_id = read_manifest_text(r.get_ref(i)?)
                                .map_err(|error| {
                                    rusqlite::Error::ToSqlConversionFailure(Box::new(Integrity {
                                        code: "manifest_text",
                                        message: error.to_string(),
                                    }))
                                })?
                                .map(OptionGroupId);
                            values.insert(
                                name.clone(),
                                Value::from(
                                    manifest
                                        .option_group_id
                                        .as_ref()
                                        .and_then(|v| v.0.as_scalar()),
                                ),
                            );
                            continue;
                        }
                        _ => {}
                    }
                }

                let value = match r.get_ref(i)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(v) => json!(v),
                    ValueRef::Real(v) => json!(v),
                    ValueRef::Text(v) => {
                        Value::String(String::from_utf8(v.to_vec()).map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                i,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?)
                    }
                    ValueRef::Blob(_) => {
                        return Err(rusqlite::Error::InvalidColumnType(
                            i,
                            name.clone(),
                            rusqlite::types::Type::Blob,
                        ));
                    }
                };
                values.insert(name.clone(), value);
            }
            #[cfg(feature = "desktop_rebuild")]
            {
                manifest.manifest_source = values
                    .get("manifest_source")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            Ok(Row(
                values,
                code,
                #[cfg(feature = "desktop_rebuild")]
                manifest,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| match error {
            rusqlite::Error::ToSqlConversionFailure(inner) => match inner.downcast::<Integrity>() {
                Ok(integrity) => anyhow!(*integrity),
                Err(inner) => anyhow!(rusqlite::Error::ToSqlConversionFailure(inner)),
            },
            error => anyhow!(error),
        })?;
    Ok(result)
}
pub(super) fn one(
    tx: &Transaction<'_>,
    table: &str,
    condition: &str,
    args: &[&dyn rusqlite::ToSql],
    code: &'static str,
    b: &mut Budget,
) -> Result<Option<Row>> {
    let mut r = rows(tx, table, condition, args, code, b)?;
    check(r.len() <= 1, code, "Duplicate accepted Plan identity")?;
    Ok(r.pop())
}
pub(super) fn required(row: Option<Row>, code: &'static str) -> Result<Row> {
    row.ok_or_else(|| {
        Integrity {
            code,
            message: format!("Accepted Plan {code} is missing"),
        }
        .into()
    })
}
pub(super) fn owned(r: &Row, tenant: &str, profile: Option<i64>, code: &'static str) -> Result<()> {
    check(
        r.s("tenant_id")? == tenant && profile.is_none_or(|p| r.n("profile_id").ok() == Some(p)),
        code,
        "Accepted Plan ownership is corrupt",
    )
}
pub(super) fn sha(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}
pub(super) fn digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(super) fn timestamp(s: &str) -> bool {
    let bytes = s.as_bytes();
    let extended = matches!(bytes.first(), Some(b'+' | b'-'));
    let prefix = if extended { 7 } else { 4 };
    if bytes.len() != prefix + 20 {
        return false;
    }
    let numeric = |start: usize, end: usize| -> Option<i64> {
        let text = s.get(start..end)?;
        if !text.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        text.parse().ok()
    };
    let Some(mut year) = numeric(usize::from(extended), prefix) else {
        return false;
    };
    if extended {
        if bytes[0] == b'-' {
            if year == 0 {
                return false;
            }
            year = -year;
        } else if year < 10_000 {
            return false;
        }
    }
    for (offset, expected) in [
        (0, b'-'),
        (3, b'-'),
        (6, b'T'),
        (9, b':'),
        (12, b':'),
        (15, b'.'),
        (19, b'Z'),
    ] {
        if bytes[prefix + offset] != expected {
            return false;
        }
    }
    let values = [(1, 3), (4, 6), (7, 9), (10, 12), (13, 15), (16, 19)]
        .map(|(a, b)| numeric(prefix + a, prefix + b));
    let [
        Some(month),
        Some(day),
        Some(hour),
        Some(minute),
        Some(second),
        Some(ms),
    ] = values
    else {
        return false;
    };
    if !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 59 {
        return false;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let date = (year, month, day, hour, minute, second, ms);
    (1..=days[month as usize - 1]).contains(&day)
        && ((-271821, 4, 20, 0, 0, 0, 0)..=(275760, 9, 13, 0, 0, 0, 0)).contains(&date)
}
pub(crate) fn safe_path(s: &str) -> bool {
    !s.is_empty()
        && !s.contains(['\\', '\0'])
        && !s.starts_with('/')
        && !(s.len() >= 2 && s.as_bytes()[0].is_ascii_alphabetic() && s.as_bytes()[1] == b':')
        && s.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
}
pub(super) fn object(fields: Vec<(&str, Value)>) -> String {
    format!(
        "{{{}}}",
        fields
            .into_iter()
            .map(|(k, v)| format!("{}:{}", json!(k), v))
            .collect::<Vec<_>>()
            .join(",")
    )
}
#[cfg(not(feature = "desktop_rebuild"))]
fn canonical_part(r: &Row) -> Result<String> {
    let fields = [
        "part_key",
        "relative_path",
        "filename",
        "source_layer",
        "status",
        "role_inferred",
        "role_override",
        "filament_color_id",
        "filament_custom_hex",
        "spoolman_spool_id",
        "quantity_inferred",
        "quantity_override",
        "quantity_effective",
        "included",
        "notes",
        "github_blob_url",
        "geometry_same",
        "requirement",
        "option_group_id",
        "manifest_source",
        "artifact_digest",
    ];
    let mut values = Vec::new();
    for k in fields {
        values.push((
            k,
            match k {
                "included" => json!(r.b(k)?),
                "geometry_same" => json!(r.ob(k)?),
                _ => r.v(k).clone(),
            },
        ));
    }
    Ok(object(values))
}
#[cfg(feature = "desktop_rebuild")]
fn canonical_part(r: &Row) -> Result<String> {
    let fields = [
        "part_key",
        "relative_path",
        "filename",
        "source_layer",
        "status",
        "role_inferred",
        "role_override",
        "filament_color_id",
        "filament_custom_hex",
        "spoolman_spool_id",
        "quantity_inferred",
        "quantity_override",
        "quantity_effective",
        "included",
        "notes",
        "github_blob_url",
        "geometry_same",
        "requirement",
        "option_group_id",
        "manifest_source",
        "artifact_digest",
    ];
    let mut scalar = serde_json::Map::new();
    for key in fields {
        if matches!(key, "requirement" | "option_group_id" | "manifest_source") {
            continue;
        }
        scalar.insert(
            key.into(),
            match key {
                "included" => json!(r.b(key)?),
                "geometry_same" => json!(r.ob(key)?),
                _ => r.v(key).clone(),
            },
        );
    }
    let part = DraftPart::new(Value::Object(scalar), r.2.clone());
    String::from_utf8(part.json_fields(&fields, false)).map_err(Into::into)
}
pub(crate) fn read(
    tx: &Transaction<'_>,
    tenant: &str,
    id: i64,
    repos: &Path,
    b: &mut Budget,
) -> Result<AcceptedRead> {
    let Some(p) = one(
        tx,
        "build_profiles",
        "id=?1 AND tenant_id=?2",
        &[&id, &tenant],
        "pointer",
        b,
    )?
    else {
        return Ok(AcceptedRead::Missing);
    };
    let profile = Profile {
        id,
        name: p.s("name")?.into(),
        order_number: p.os("order_number")?,
        special_request: p.os("special_request")?,
        archived_at: p.os("archived_at")?,
    };
    let version = p.n("accepted_plan_version")?;
    check(version >= 0, "pointer", "Accepted Plan pointer is corrupt")?;
    let accepted = one(
        tx,
        "plan_accepted_input_sets",
        "profile_id=?1",
        &[&id],
        "accepted_inputs",
        b,
    )?;
    let mut projection = rows(tx, "parts", "profile_id=?1", &[&id], "projection", b)?;
    let Some(revision_id) = p.on("accepted_plan_revision_id")? else {
        if version == 0 {
            let history: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM plan_revisions WHERE profile_id=?1)",
                [id],
                |r| r.get(0),
            )?;
            check(!history, "pointer", "Accepted Plan history has no pointer")?;
        }
        return Ok(
            if version > 0 || !projection.is_empty() || accepted.is_some() {
                AcceptedRead::CompatibilityDirty
            } else {
                AcceptedRead::Empty { profile }
            },
        );
    };
    check(
        version > 0 && revision_id > 0,
        "pointer",
        "Accepted Plan pointer has a zero version",
    )?;
    let rev = required(
        one(
            tx,
            "plan_revisions",
            "id=?1",
            &[&revision_id],
            "revision",
            b,
        )?,
        "revision",
    )?;
    owned(&rev, tenant, Some(id), "revision")?;
    check(
        rev.n("revision_number")? > 0
            && rev.s("digest_format")? == "plan-revision-parts-v1"
            && timestamp(rev.s("created_at")?)
            && timestamp(rev.s("accepted_at")?)
            && !rev.s("created_by")?.is_empty()
            && !rev.s("accepted_by")?.is_empty(),
        "revision",
        "Accepted Plan revision is corrupt",
    )?;
    if let Some(parent) = rev.on("parent_revision_id")? {
        let r = required(
            one(tx, "plan_revisions", "id=?1", &[&parent], "revision", b)?,
            "revision",
        )?;
        owned(&r, tenant, Some(id), "revision")?;
    }
    let mut revision_parts = rows(
        tx,
        "plan_revision_parts",
        "revision_id=?1",
        &[&revision_id],
        "revision",
        b,
    )?;
    revision_parts.sort_by_key(|r| r.n("id").unwrap_or(0));
    let mut canonical = Vec::new();
    for r in &revision_parts {
        owned(r, tenant, None, "revision")?;
        check(
            r.n("id")? > 0,
            "revision",
            "Accepted Plan revision Part ID is corrupt",
        )?;
        canonical.push(canonical_part(r)?);
    }
    canonical.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    check(
        sha(&format!(
            "{{\"format\":\"plan-revision-parts-v1\",\"parts\":[{}]}}",
            canonical.join(",")
        )) == rev.s("snapshot_digest")?,
        "revision_digest",
        "Accepted Plan revision digest is corrupt",
    )?;
    let kind = rev.s("provenance_kind")?;
    let mut inputs = Vec::new();
    let mut format1 = false;
    let provenance = if kind == "legacy" {
        check(
            rev.v("input_set_id").is_null() && accepted.is_none(),
            "accepted_inputs",
            "Legacy accepted Plan has input provenance",
        )?;
        Provenance::Legacy
    } else {
        check(
            kind == "tracked",
            "revision",
            "Accepted Plan revision provenance is corrupt",
        )?;
        let sid = rev.n("input_set_id")?;
        let a = required(accepted, "accepted_inputs")?;
        owned(&a, tenant, Some(id), "accepted_inputs")?;
        let set = required(
            one(
                tx,
                "plan_revision_input_sets",
                "id=?1",
                &[&sid],
                "accepted_inputs",
                b,
            )?,
            "accepted_inputs",
        )?;
        owned(&set, tenant, Some(id), "accepted_inputs")?;
        let fv = set.n("format_version")?;
        format1 = fv == 1;
        check(
            a.n("input_set_id")? == sid
                && a.s("accepted_at")? == rev.s("accepted_at")?
                && timestamp(set.s("recorded_at")?)
                && timestamp(set.s("published_at")?)
                && (fv == 1 || fv == 2),
            "accepted_inputs",
            "Accepted Plan input header is corrupt",
        )?;
        let ir = rows(
            tx,
            "plan_revision_inputs",
            "input_set_id=?1",
            &[&sid],
            "accepted_inputs",
            b,
        )?;
        check(
            set.n("expected_input_count")? == ir.len() as i64,
            "accepted_inputs",
            "Accepted Plan input set is incomplete",
        )?;
        let (mut sources, mut layers, mut orders) =
            (HashSet::new(), HashSet::new(), HashSet::new());
        let mut canonical_inputs = Vec::new();
        for r in ir {
            owned(&r, tenant, None, "accepted_inputs")?;
            let source = r.n("source_id")?;
            let layer = r.s("source_layer")?;
            let order = r.n("layer_order")?;
            let tracking = r.s("tracking_kind")?;
            check(
                r.n("id")? > 0
                    && source > 0
                    && sources.insert(source)
                    && !layer.is_empty()
                    && layers.insert(layer.to_owned())
                    && order >= 0,
                "accepted_inputs",
                "Accepted Plan input identity is corrupt",
            )?;
            let s = required(
                one(tx, "projects", "id=?1", &[&source], "source_revision", b)?,
                "source_revision",
            )?;
            owned(&s, tenant, None, "source_revision")?;
            if format1 {
                check(
                    layer == format!("legacy:{source}")
                        && order == 0
                        && tracking == "revision"
                        && r.v("effective_naming_digest").is_null(),
                    "accepted_inputs",
                    "Format-1 accepted Plan input set is corrupt",
                )?;
            } else {
                check(
                    orders.insert(order) && digest(r.s("effective_naming_digest")?),
                    "accepted_inputs",
                    "Accepted Plan input identity is corrupt",
                )?;
            }
            let (mut root, mut path, mut synced) = (None, None, None);
            if tracking == "revision" {
                let rid = r.n("source_revision_id")?;
                check(
                    rid > 0 && digest(r.s("manifest_digest")?),
                    "source_revision",
                    "Accepted Plan Source revision identity is corrupt",
                )?;
                let sr = required(
                    one(
                        tx,
                        "source_revisions",
                        "id=?1",
                        &[&rid],
                        "source_revision",
                        b,
                    )?,
                    "source_revision",
                )?;
                owned(&sr, tenant, None, "source_revision")?;
                let locator = sr.s("snapshot_locator")?;
                check(
                    sr.n("project_id")? == source
                        && sr.s("completeness")? == "complete"
                        && sr.v("manifest_digest") == r.v("manifest_digest")
                        && timestamp(sr.s("synced_at")?)
                        && safe_path(locator)
                        && locator.starts_with(&format!("{source}/")),
                    "source_revision",
                    "Accepted Plan Source revision is corrupt",
                )?;
                let workspace = repos.join(source.to_string());
                let source_meta = std::fs::symlink_metadata(&workspace);
                let resolved = repos.join(locator).canonicalize();
                check(
                    source_meta.is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
                        && resolved.as_ref().is_ok_and(|p| {
                            p.is_dir() && workspace.canonicalize().is_ok_and(|w| p.starts_with(w))
                        }),
                    "source_revision",
                    "Accepted Plan Source revision is corrupt",
                )?;
                let resolved = resolved.map_err(|_| Integrity {
                    code: "source_revision",
                    message: "Accepted Plan Source revision is corrupt".into(),
                })?;
                root = Some(resolved.to_string_lossy().into_owned());
                path = Some(repos.join(locator).to_string_lossy().into_owned());
                synced = sr.os("synced_at")?;
            } else {
                check(
                    tracking == "untracked"
                        && r.v("source_revision_id").is_null()
                        && r.v("manifest_digest").is_null(),
                    "source_revision",
                    "Accepted Plan untracked Source identity is corrupt",
                )?;
            }
            let fields = if format1 {
                vec![
                    ("source_revision_id", r.v("source_revision_id").clone()),
                    ("manifest_digest", r.v("manifest_digest").clone()),
                ]
            } else {
                [
                    "source_id",
                    "source_layer",
                    "layer_order",
                    "tracking_kind",
                    "source_revision_id",
                    "manifest_digest",
                    "effective_naming_digest",
                ]
                .iter()
                .map(|k| (*k, r.v(k).clone()))
                .collect()
            };
            canonical_inputs.push((
                if format1 {
                    r.n("source_revision_id")?
                } else {
                    source
                },
                object(fields),
            ));
            inputs.push(Input {
                input_id: r.n("id")?,
                source_id: source,
                source_layer: layer.into(),
                layer_order: order,
                effective_naming_digest: r.os("effective_naming_digest")?.unwrap_or_default(),
                tracking_kind: tracking.into(),
                source_revision_id: r.on("source_revision_id")?,
                manifest_digest: r.os("manifest_digest")?,
                snapshot_root: root,
                snapshot_path: path,
                source_synced_at: synced,
            });
        }
        canonical_inputs.sort_by_key(|r| r.0);
        let list = canonical_inputs
            .into_iter()
            .map(|r| r.1)
            .collect::<Vec<_>>()
            .join(",");
        check(
            sha(&if format1 {
                format!("[{list}]")
            } else {
                format!("{{\"version\":2,\"inputs\":[{list}]}}")
            }) == set.s("input_set_digest")?,
            "accepted_inputs",
            "Accepted Plan input digest is corrupt",
        )?;
        inputs.sort_by_key(|i| (i.layer_order, i.source_id));
        Provenance::Tracked {
            input_set_id: sid,
            input_set_digest: set.s("input_set_digest")?.into(),
            inputs: inputs.clone(),
        }
    };
    check(
        projection.len() == revision_parts.len(),
        "projection",
        "Accepted Plan Part booleans or ownership are corrupt",
    )?;
    let mut projections = HashMap::new();
    for p in projection.drain(..) {
        owned(&p, tenant, Some(id), "projection")?;
        p.b("included")?;
        p.ob("geometry_same")?;
        check(
            p.n("id")? > 0,
            "projection",
            "Accepted Plan Part identity is corrupt",
        )?;
        projections.insert(p.n("id")?, p);
    }
    let (mut ids, mut keys) = (HashSet::new(), HashSet::new());
    let mut parts = Vec::new();
    for r in revision_parts {
        let pid = r.n("projection_part_id")?;
        let key = r.s("part_key")?;
        let q = r.n("quantity_effective")?;
        let qi = r.n("quantity_inferred")?;
        let qo = r.on("quantity_override")?;
        check(
            pid > 0
                && ids.insert(pid)
                && !key.is_empty()
                && keys.insert(key.to_owned())
                && (1..=10000).contains(&qi)
                && qo.is_none_or(|n| (1..=10000).contains(&n))
                && q == qo.unwrap_or(qi),
            "projection",
            "Accepted Plan revision Part identity is corrupt",
        )?;
        let p = projections.get(&pid).ok_or_else(|| Integrity {
            code: "projection",
            message: "Accepted Plan projection differs from its revision".into(),
        })?;
        let role = r
            .os("role_override")?
            .unwrap_or(r.s("role_inferred")?.into());
        check(
            p.s("match_key")? == key && p.s("role")? == role && p.n("quantity_auto")? == qi,
            "projection",
            "Accepted Plan projection differs from its revision",
        )?;
        for k in [
            "relative_path",
            "filename",
            "source_layer",
            "status",
            "quantity_override",
            "quantity_effective",
            "included",
            "notes",
            "github_blob_url",
            "geometry_same",
            "requirement",
            "option_group_id",
            "manifest_source",
        ] {
            check(
                p.same_field(&r, k),
                "projection",
                "Accepted Plan projection differs from its revision",
            )?;
        }
        let artifact = if kind == "legacy" || format1 {
            Artifact::Unavailable { reason: "legacy" }
        } else {
            let input = inputs
                .iter()
                .find(|i| i.source_layer == r.s("source_layer").unwrap_or_default())
                .ok_or_else(|| Integrity {
                    code: "artifact_linkage",
                    message: "Accepted Plan Part has no pinned input".into(),
                })?;
            if input.tracking_kind == "untracked" {
                check(
                    r.v("artifact_digest").is_null(),
                    "artifact_linkage",
                    "Untracked accepted Plan Part claims an artifact digest",
                )?;
                Artifact::Unavailable {
                    reason: "untracked_source",
                }
            } else {
                check(
                    safe_path(r.s("relative_path")?) && digest(r.s("artifact_digest")?),
                    "artifact_linkage",
                    "Tracked accepted Plan Part artifact is corrupt",
                )?;
                Artifact::Tracked {
                    source_id: input.source_id,
                    source_revision_id: input.source_revision_id.ok_or_else(|| Integrity {
                        code: "artifact_linkage",
                        message: "Tracked accepted Plan Part has no source revision".into(),
                    })?,
                    snapshot_root: input.snapshot_root.clone().ok_or_else(|| Integrity {
                        code: "artifact_linkage",
                        message: "Tracked accepted Plan Part has no snapshot".into(),
                    })?,
                    snapshot_path: input.snapshot_path.clone().unwrap_or_default(),
                    relative_path: r.s("relative_path")?.into(),
                    expected_sha256: r.s("artifact_digest")?.into(),
                }
            }
        };
        parts.push(Part {
            revision_part_id: r.n("id")?,
            projection_part_id: pid,
            part_key: key.into(),
            relative_path: r.s("relative_path")?.into(),
            filename: r.s("filename")?.into(),
            source_layer: r.s("source_layer")?.into(),
            status: r.s("status")?.into(),
            role_inferred: r.s("role_inferred")?.into(),
            role_override: r.os("role_override")?,
            effective_role: role,
            filament_color_id: p.os("filament_color_id")?,
            filament_custom_hex: p.os("filament_custom_hex")?,
            spoolman_spool_id: p.os("spoolman_spool_id")?,
            quantity_inferred: qi,
            quantity_override: qo,
            quantity_effective: q,
            included: r.b("included")?,
            notes: r.s("notes")?.into(),
            github_blob_url: r.os("github_blob_url")?,
            geometry_same: r.ob("geometry_same")?,
            requirement: r.os("requirement")?,
            option_group_id: r.os("option_group_id")?,
            manifest_source: r.os("manifest_source")?,
            #[cfg(feature = "desktop_rebuild")]
            manifest: r.2.clone(),
            artifact,
            units: Vec::new(),
        });
    }
    let set = one(
        tx,
        "plan_revision_required_unit_sets",
        "revision_id=?1",
        &[&revision_id],
        "required_unit_map",
        b,
    )?;
    let mut mappings = rows(
        tx,
        "plan_revision_required_units",
        "revision_id=?1",
        &[&revision_id],
        "required_unit_map",
        b,
    )?;
    let created = rows(
        tx,
        "required_units",
        "created_in_revision_id=?1",
        &[&revision_id],
        "required_unit_map",
        b,
    )?;
    let Some(set) = set else {
        check(
            mappings.is_empty() && created.is_empty(),
            "required_unit_map",
            "Accepted Plan Required-unit set is partial",
        )?;
        return Ok(AcceptedRead::Uninitialized);
    };
    owned(&set, tenant, Some(id), "required_unit_map")?;
    let count = parts.iter().map(|p| p.quantity_effective).sum::<i64>();
    check(
        set.s("format")? == "required-unit-map-v1"
            && set.n("expected_unit_count")? == count
            && mappings.len() as i64 == count,
        "required_unit_map",
        "Accepted Plan Required-unit header or mappings are corrupt",
    )?;
    mappings.sort_by_key(|r| {
        (
            r.n("revision_part_id").unwrap_or(0),
            r.n("unit_index").unwrap_or(-1),
        )
    });
    let (mut tokens, mut names) = (HashSet::new(), HashSet::new());
    let mut canonical = Vec::new();
    for m in mappings {
        owned(&m, tenant, None, "required_unit_map")?;
        let part_id = m.n("revision_part_id")?;
        let index = m.n("unit_index")?;
        let token = m.s("required_unit_token")?;
        let p = parts
            .iter_mut()
            .find(|p| p.revision_part_id == part_id)
            .ok_or_else(|| Integrity {
                code: "required_unit_map",
                message: "Accepted Plan Required-unit ownership is corrupt".into(),
            })?;
        let u = required(
            one(
                tx,
                "required_units",
                "token=?1",
                &[&token],
                "required_unit_map",
                b,
            )?,
            "required_unit_map",
        )?;
        owned(&u, tenant, Some(id), "required_unit_map")?;
        let creation = u.n("created_in_revision_id")?;
        let cr = required(
            one(
                tx,
                "plan_revisions",
                "id=?1",
                &[&creation],
                "required_unit_map",
                b,
            )?,
            "required_unit_map",
        )?;
        owned(&cr, tenant, Some(id), "required_unit_map")?;
        let name = u.s("object_name")?;
        check(
            index == p.units.len() as i64
                && index < p.quantity_effective
                && tokens.insert(token.to_owned())
                && names.insert(name.to_lowercase()),
            "required_unit_map",
            "Accepted Plan Required-unit ownership is corrupt",
        )?;
        check(
            token.len() == 36
                && token.starts_with("ppu_")
                && token[4..]
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                && !name.is_empty()
                && name.len() <= 200
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_ .()+-".contains(&b))
                && name.ends_with(&format!("__{token}")),
            "required_unit_map",
            "Accepted Plan Required-unit syntax is corrupt",
        )?;
        canonical.push(object(vec![
            ("revision_part_id", json!(part_id)),
            ("unit_index", json!(index)),
            ("token", json!(token)),
            ("object_name", json!(name)),
        ]));
        p.units.push(Unit {
            unit_index: index,
            required: p.included,
            token: token.into(),
            object_name: name.into(),
            completed: false,
            assembled: false,
        });
    }
    for p in &parts {
        check(
            p.units.len() as i64 == p.quantity_effective,
            "required_unit_map",
            "Accepted Plan Required-unit coordinates are incomplete",
        )?;
    }
    for u in created {
        owned(&u, tenant, Some(id), "required_unit_map")?;
        check(
            tokens.contains(u.s("token")?),
            "required_unit_map",
            "Accepted Plan Required unit is orphaned",
        )?;
    }
    let mapping_digest = sha(&format!(
        "{{\"format\":\"required-unit-map-v1\",\"revision_id\":{revision_id},\"expected_unit_count\":{count},\"rows\":[{}]}}",
        canonical.join(",")
    ));
    check(
        mapping_digest == set.s("mapping_digest")?,
        "required_unit_map",
        "Accepted Plan Required-unit digest is corrupt",
    )?;
    if format1 {
        return Ok(AcceptedRead::Uninitialized);
    }
    let progress = rows(
        tx,
        "print_progress",
        "part_id IN (SELECT id FROM parts WHERE profile_id=?1)",
        &[&id],
        "progress",
        b,
    )?;
    let mut seen = HashSet::new();
    for r in progress {
        let part = r.n("part_id")?;
        let index = r.n("unit_index")?;
        let Some(p) = parts.iter_mut().find(|p| p.projection_part_id == part) else {
            continue;
        };
        if index < 0 || index >= p.quantity_effective {
            continue;
        }
        owned(&r, tenant, None, "progress")?;
        let completed = r.n("completed")?;
        let assembled = r.n("assembled")?;
        check(
            seen.insert((part, index))
                && (0..=1).contains(&completed)
                && (0..=1).contains(&assembled)
                && assembled <= completed,
            "progress",
            "Accepted Plan progress is corrupt",
        )?;
        p.units[index as usize].completed = completed == 1;
        p.units[index as usize].assembled = assembled == 1;
    }
    Ok(AcceptedRead::Ready {
        snapshot: Box::new(Snapshot {
            format: "accepted-plan-operational-v1",
            profile,
            plan_version: version,
            revision_id,
            revision_number: rev.n("revision_number")?,
            revision_digest: rev.s("snapshot_digest")?.into(),
            accepted_at: rev.s("accepted_at")?.into(),
            provenance,
            required_unit_mapping_digest: mapping_digest,
            parts,
        }),
    })
}

#[cfg(all(test, feature = "desktop_rebuild"))]
#[path = "review_tests.rs"]
mod review_tests;
