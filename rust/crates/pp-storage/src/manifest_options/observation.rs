use super::ManifestInputDetail;
use super::builder::{ManifestBuilder, ManifestSource, VariantSource, VariantSources};
use super::json::{self, JsonError, JsonLimit, JsonValue};
use super::model::KitManifest;
use crate::manifest_text::{JsText, ManifestText, OptionGroupId, VariantId};
use crate::working_drafts::observation::{
    BuilderGlob, BuilderGroup, BuilderGroups, BuilderVariant, CatalogPurpose, CheckoutManifestUse,
    CommunitySlug, DocumentRead, DraftReads, InventoryPath, OwnedCatalogReadRequest,
    OwnedCheckoutReadRequest, OwnedStlReadRequest, PreparationBudget, ReadFailure, ReadResult,
    Resource, StlPurpose, infer_builder_siblings, merge_builder_groups, parse_builder_hints,
    parse_builder_manifest,
};
use rusqlite::{OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CapturedManifestGraph {
    tenant: String,
    build: i64,
    config_modified_at: Option<String>,
    stored_kit: Option<String>,
    sources: Vec<CapturedSource>,
}

impl CapturedManifestGraph {
    pub(crate) fn stored_selections(&self) -> KitManifest {
        self.stored_kit
            .as_deref()
            .and_then(|raw| KitManifest::from_stored(raw).ok())
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, PartialEq)]
struct CapturedSource {
    layer_id: i64,
    layer_order: i64,
    layer_type: String,
    source_id: i64,
    name: String,
    role: String,
    url: String,
    local_path: Option<String>,
    imported_paths: Option<String>,
    community_slug: Option<String>,
    current_revision_id: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SourceStamp([u8; 32]);

pub(crate) struct ObservedBuilder {
    pub builder: ManifestBuilder,
    pub stamps: Vec<(i64, SourceStamp)>,
    /// Sources whose stored community slug was refused; observation read no
    /// community manifest for them.
    pub invalid_community_slugs: Vec<i64>,
}

type SelectionDefault = (OptionGroupId, Vec<VariantId>, bool);

struct SourceObservation {
    row: ManifestSource,
    groups: BuilderGroups,
    defaults: Vec<SelectionDefault>,
    stamp: SourceStamp,
    invalid_community_slug: bool,
}

pub(crate) fn capture(
    tx: &Transaction<'_>,
    tenant: &str,
    build: i64,
) -> rusqlite::Result<Option<CapturedManifestGraph>> {
    let Some(config_modified_at) = tx
        .query_row(
            "SELECT config_modified_at FROM build_profiles WHERE tenant_id=?1 AND id=?2",
            params![tenant, build],
            |row| row.get(0),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let stored_kit = tx
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
            params![tenant, format!("kit_manifest_{build}")],
            |row| row.get(0),
        )
        .optional()?;
    let mut statement = tx.prepare(
        "SELECT layer.id,layer.layer_order,layer.layer_type,source.id,source.name,coalesce(source.role,'unassigned'),coalesce(source.url,''),source.local_path,source.imported_paths,source.manifest_community_slug,source.current_source_revision_id FROM profile_layers layer JOIN projects source ON source.tenant_id=layer.tenant_id AND source.id=layer.project_id WHERE layer.tenant_id=?1 AND layer.profile_id=?2 ORDER BY layer.layer_order,layer.id",
    )?;
    let sources = statement
        .query_map(params![tenant, build], |row| {
            Ok(CapturedSource {
                layer_id: row.get(0)?,
                layer_order: row.get(1)?,
                layer_type: row.get(2)?,
                source_id: row.get(3)?,
                name: row.get(4)?,
                role: row.get(5)?,
                url: row.get(6)?,
                local_path: row.get(7)?,
                imported_paths: row.get(8)?,
                community_slug: row.get(9)?,
                current_revision_id: row.get(10)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Some(CapturedManifestGraph {
        tenant: tenant.into(),
        build,
        config_modified_at,
        stored_kit,
        sources,
    }))
}

pub(crate) fn basis_matches(
    tx: &Transaction<'_>,
    captured: &CapturedManifestGraph,
) -> rusqlite::Result<bool> {
    Ok(capture(tx, &captured.tenant, captured.build)?.as_ref() == Some(captured))
}

pub(crate) fn observe(
    captured: &CapturedManifestGraph,
    reads: Option<&dyn DraftReads>,
    budget: &mut PreparationBudget<'_>,
) -> ReadResult<ObservedBuilder> {
    budget.check()?;
    let mut resolved_selections = match captured.stored_kit.as_deref() {
        Some(raw) => stored_kit(raw, budget)?,
        None => KitManifest::default(),
    };
    let mut sources = Vec::new();
    let mut merged_option_groups = BuilderGroups::new();
    let mut variant_sources = VariantSources::new();
    let mut stamps = Vec::new();
    let mut invalid_community_slugs = Vec::new();
    for source in &captured.sources {
        let Some(path) = source.local_path.as_deref().filter(|path| !path.is_empty()) else {
            continue;
        };
        let reads = reads.ok_or(ReadFailure::Unavailable)?;
        let Some(observed) = observe_source(&captured.tenant, source, path, reads, budget)? else {
            continue;
        };
        for (group, value) in &observed.groups {
            for variant in &value.variants {
                if !any_pattern_matches(&variant.parts, &observed.row.scanned_parts, budget)? {
                    continue;
                }
                let entries = variant_sources
                    .entry(group.clone())
                    .or_default()
                    .entry(variant.id.clone())
                    .or_default();
                if !entries
                    .iter()
                    .any(|entry| entry.source_id == source.source_id)
                {
                    entries.push(VariantSource {
                        source_id: source.source_id,
                        source_name: source.name.clone(),
                    });
                }
            }
        }
        for (group, values, scalar) in observed.defaults {
            resolved_selections.insert_default_selection(group, values, scalar);
        }
        merge_builder_groups(&mut merged_option_groups, observed.groups);
        if observed.invalid_community_slug {
            invalid_community_slugs.push(source.source_id);
        }
        sources.push(observed.row);
        stamps.push((source.source_id, observed.stamp));
    }
    budget.check()?;
    Ok(ObservedBuilder {
        builder: ManifestBuilder {
            profile_id: captured.build,
            sources,
            resolved_selections,
            merged_option_groups,
            variant_sources,
        },
        stamps,
        invalid_community_slugs,
    })
}

fn observe_source(
    tenant: &str,
    source: &CapturedSource,
    path: &str,
    reads: &dyn DraftReads,
    budget: &mut PreparationBudget<'_>,
) -> ReadResult<Option<SourceObservation>> {
    budget.check()?;
    let mut stamp = Sha256::new();
    stamp_text(&mut stamp, &source.source_id.to_string());
    stamp_text(&mut stamp, path);
    let manifest = reads.read_checkout_manifest(
        &OwnedCheckoutReadRequest {
            tenant: tenant.into(),
            source_id: source.source_id,
            path: path.into(),
        },
        CheckoutManifestUse::OptionGroups,
        budget,
    )?;
    stamp_document(&mut stamp, &manifest);
    let (loaded_yaml, declared, mut defaults) = match load_manifest(manifest, budget)? {
        Some((yaml, groups, defaults)) => (Some(yaml), groups, defaults),
        None => (None, BuilderGroups::new(), Vec::new()),
    };
    let mut root = reads.resolve_stl_root(
        &OwnedStlReadRequest {
            tenant: tenant.into(),
            source_id: source.source_id,
            path: Some(path.into()),
            purpose: StlPurpose::CheckoutOptions,
        },
        budget,
    )?;
    if root.logical_resolved_path().is_none() {
        return Ok(None);
    }
    let inventory = root.scan_stls(budget)?;
    let scanned_parts = scanned_paths(
        inventory.entries(),
        source.imported_paths.as_deref(),
        budget,
    )?;
    for part in &scanned_parts {
        stamp_text(&mut stamp, part);
    }
    let groups = if declared.is_empty() {
        inferred_groups(&scanned_parts, reads, budget, &mut stamp)?
    } else {
        declared.clone()
    };
    let community_slug = source
        .community_slug
        .as_deref()
        .filter(|slug| !slug.is_empty())
        .map(CommunitySlug::parse);
    if let Some(Some(slug)) = &community_slug {
        let community = reads.read_catalog_document(
            &OwnedCatalogReadRequest {
                purpose: CatalogPurpose::Community(slug.clone()),
            },
            budget,
        )?;
        stamp_document(&mut stamp, &community);
        if let Some((_, _, community_defaults)) = load_manifest(community, budget)? {
            defaults.extend(community_defaults);
        }
    }
    Ok(Some(SourceObservation {
        row: ManifestSource {
            source_id: source.source_id,
            layer_type: source.layer_type.clone(),
            name: source.name.clone(),
            role: source.role.clone(),
            url: source.url.clone(),
            loaded_yaml,
            option_groups: declared,
            scanned_parts,
        },
        groups,
        defaults,
        stamp: SourceStamp(stamp.finalize().into()),
        invalid_community_slug: matches!(community_slug, Some(None)),
    }))
}

fn load_manifest(
    read: DocumentRead,
    budget: &mut PreparationBudget<'_>,
) -> ReadResult<Option<(String, BuilderGroups, Vec<SelectionDefault>)>> {
    let DocumentRead::Bytes(bytes) = read else {
        return Ok(None);
    };
    let manifest = match parse_builder_manifest(&bytes, budget) {
        Ok(manifest) => manifest,
        Err(ReadFailure::InvalidDocument) => return Ok(None),
        Err(error) => return Err(error),
    };
    let defaults = manifest
        .selections
        .into_iter()
        .map(|(group, values)| {
            let scalar = manifest
                .selection_shapes
                .iter()
                .any(|(candidate, scalar)| *candidate == group && *scalar);
            (group, values, scalar)
        })
        .collect();
    Ok(Some((
        String::from_utf8_lossy(&bytes).into_owned(),
        manifest.groups,
        defaults,
    )))
}

fn inferred_groups(
    paths: &[String],
    reads: &dyn DraftReads,
    budget: &mut PreparationBudget<'_>,
    stamp: &mut Sha256,
) -> ReadResult<BuilderGroups> {
    let mut hints = Vec::new();
    for purpose in [
        CatalogPurpose::ShippedHintsFirst,
        CatalogPurpose::ShippedHintsSecond,
    ] {
        let read = reads.read_catalog_document(&OwnedCatalogReadRequest { purpose }, budget)?;
        stamp_document(stamp, &read);
        if let DocumentRead::Bytes(bytes) = read {
            match parse_builder_hints(&bytes, budget) {
                Ok(shipped) => {
                    hints = shipped;
                    break;
                }
                Err(ReadFailure::InvalidDocument) => {}
                Err(error) => return Err(error),
            }
        }
    }
    let custom = reads.read_catalog_document(
        &OwnedCatalogReadRequest {
            purpose: CatalogPurpose::CustomHints,
        },
        budget,
    )?;
    stamp_document(stamp, &custom);
    match custom {
        DocumentRead::Missing => {}
        DocumentRead::Skipped(error) => return Err(ReadFailure::Io(error)),
        DocumentRead::Bytes(bytes) => hints.extend(parse_builder_hints(&bytes, budget)?),
    }
    let mut groups = BuilderGroups::new();
    for (pattern, group, variant, label) in hints {
        if !any_pattern_matches(std::slice::from_ref(&pattern), paths, budget)? {
            continue;
        }
        let group_label = label.clone().unwrap_or_else(|| group.0.identifier_label());
        let variant_label = label.unwrap_or_else(|| variant.0.identifier_label());
        merge_builder_groups(
            &mut groups,
            vec![(
                group,
                BuilderGroup {
                    rule: "pick_one".into(),
                    label: Some(group_label),
                    parts: Vec::new(),
                    variants: vec![BuilderVariant {
                        id: variant,
                        label: Some(variant_label),
                        parts: vec![pattern],
                        excludes: Vec::new(),
                    }],
                    min: None,
                    max: None,
                },
            )],
        );
    }
    merge_builder_groups(&mut groups, infer_builder_siblings(paths, budget)?);
    Ok(groups)
}

fn any_pattern_matches(
    patterns: &[ManifestText],
    paths: &[String],
    budget: &PreparationBudget<'_>,
) -> ReadResult<bool> {
    budget.check()?;
    for pattern in patterns {
        let glob = BuilderGlob::new(pattern);
        for path in paths {
            budget.check()?;
            if glob.matches(path) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn scanned_paths(
    entries: &[InventoryPath],
    imported_paths: Option<&str>,
    budget: &mut PreparationBudget<'_>,
) -> ReadResult<Vec<String>> {
    let rules = imported_paths
        .map(|raw| import_rules(raw, budget))
        .transpose()?;
    let mut paths = Vec::new();
    for entry in entries {
        budget.check()?;
        let path = entry.physical_relative_path.replace('\\', "/");
        if rules.as_ref().is_none_or(|rules| imported(&path, rules)) {
            paths.push((path.to_lowercase(), path));
        }
    }
    paths.sort_by(|left, right| crate::read_model::views::folder_compare(&left.0, &right.0));
    Ok(paths.into_iter().map(|(_, path)| path).collect())
}

fn import_rules(raw: &str, budget: &mut PreparationBudget<'_>) -> ReadResult<Vec<String>> {
    budget.document(raw.len(), raw.len())?;
    let parsed = match parse_json(&js_trim(raw), budget)? {
        Some(parsed) => parsed,
        None => return Ok(Vec::new()),
    };
    let JsonValue::Array(values) = parsed else {
        return Ok(Vec::new());
    };
    values
        .into_iter()
        .filter_map(|value| match value {
            JsonValue::Text(text) if !text.is_ecmascript_blank() => text.as_scalar(),
            _ => None,
        })
        .map(|rule| {
            budget.rule()?;
            let rule = relative_path(&rule);
            Ok(
                if rule.is_empty() || rule.ends_with('/') || rule.to_lowercase().ends_with(".stl") {
                    rule
                } else {
                    format!("{rule}/")
                },
            )
        })
        .collect()
}

/// Parses a stored kit as Node `loadKitManifest` does, charging its bytes and
/// nodes to `budget`. Malformed kits read as empty.
pub(crate) fn stored_kit(raw: &str, budget: &mut PreparationBudget<'_>) -> ReadResult<KitManifest> {
    budget.document(raw.len(), raw.len())?;
    Ok(parse_json(raw, budget)?
        .map(KitManifest::from_stored_or_empty)
        .unwrap_or_default())
}

enum BudgetedJsonError {
    Json(JsonError),
    Budget(ReadFailure),
}

impl From<JsonError> for BudgetedJsonError {
    fn from(error: JsonError) -> Self {
        Self::Json(error)
    }
}

fn parse_json(raw: &str, budget: &mut PreparationBudget<'_>) -> ReadResult<Option<JsonValue>> {
    match json::parse_with_node(raw.as_bytes(), |depth| {
        budget.node(depth).map_err(BudgetedJsonError::Budget)
    }) {
        Ok(parsed) => Ok(Some(parsed)),
        Err(BudgetedJsonError::Json(JsonError::Syntax)) => Ok(None),
        Err(BudgetedJsonError::Json(JsonError::Limit(limit))) => Err(limit_exceeded(limit)),
        Err(BudgetedJsonError::Budget(error)) => Err(error),
    }
}

fn limit_exceeded(limit: JsonLimit) -> ReadFailure {
    ReadFailure::LimitExceeded(match limit {
        JsonLimit::Bytes => Resource::DocumentBytes,
        JsonLimit::Depth => Resource::Depth,
        JsonLimit::Nodes => Resource::Nodes,
    })
}

fn imported(path: &str, rules: &[String]) -> bool {
    let path = relative_path(path);
    rules.iter().any(|rule| match rule.strip_suffix('/') {
        Some(prefix) => {
            path == prefix
                || path
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('/'))
        }
        None => path == *rule,
    })
}

fn relative_path(path: &str) -> String {
    js_trim(&path.replace('\\', "/"))
        .trim_start_matches('/')
        .to_owned()
}

fn js_trim(value: &str) -> String {
    String::from_utf16_lossy(JsText::scalar(value).trim_ecmascript().units())
}

fn stamp_document(stamp: &mut Sha256, document: &DocumentRead) {
    match document {
        DocumentRead::Missing => stamp.update([0]),
        DocumentRead::Skipped(error) => {
            stamp.update([1]);
            stamp_text(stamp, &format!("{error:?}"));
        }
        DocumentRead::Bytes(bytes) => {
            stamp.update([2]);
            stamp.update((bytes.len() as u64).to_le_bytes());
            stamp.update(bytes);
        }
    }
}

fn stamp_text(stamp: &mut Sha256, value: &str) {
    stamp.update((value.len() as u64).to_le_bytes());
    stamp.update(value.as_bytes());
}

pub(crate) fn validate_known_selections(
    kit: &KitManifest,
    groups: &BuilderGroups,
) -> Result<(), ManifestInputDetail> {
    for (group_id, selection) in kit.selection_projection() {
        let Some((_, group)) = groups.iter().find(|(id, _)| *id == group_id) else {
            continue;
        };
        let pick_one = group.rule == "pick_one";
        if pick_one && selection.is_empty() {
            return Err(ManifestInputDetail::group(
                &group_id.0,
                " must contain at least one variant id",
            ));
        }
        let maximum = if pick_one {
            Some(group.max.unwrap_or(1).min(1))
        } else {
            group.max
        };
        if let Some(maximum) = maximum
            && selection.len() as u64 > maximum
        {
            let unit = if maximum == 1 {
                "variant id"
            } else {
                "variant ids"
            };
            return Err(ManifestInputDetail::group(
                &group_id.0,
                &format!(" must contain no more than {maximum} {unit}"),
            ));
        }
    }
    Ok(())
}
