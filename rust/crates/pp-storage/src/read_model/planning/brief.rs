//! The stored Build planning brief (`build_planning.v1.<id>`), and what
//! Node's `services/build-planning.ts` derives from it for
//! `GET /plans/:id/build-planning`: hydration against the tenant's Sources,
//! the planning phase and readiness.

use super::super::views::{folder_compare, js_trim};
use crate::working_drafts::source_reads::SourceReads;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

const MAX_SAFE: i64 = 9_007_199_254_740_991;

macro_rules! string_enum {
    ($name:ident { $($variant:ident = $text:literal),+ $(,)? }) => {
        #[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        pub enum $name { $(#[serde(rename = $text)] $variant),+ }
        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }
        }
    };
}

string_enum!(RequirementStatus {
    Unverified = "unverified",
    Satisfied = "satisfied",
    Incompatible = "incompatible",
    UserWaived = "user_waived",
});
string_enum!(EvidenceKind {
    CanonicalDesign = "canonical_design",
    VendorOverlay = "vendor_overlay",
    Mod = "mod",
    Component = "component",
    ModelSource = "model_source",
    InformationalEvidence = "informational_evidence",
});
string_enum!(EvidenceInputKind {
    Url = "url",
    ModelPage = "model_page",
    Upload = "upload",
});
string_enum!(SourceRole {
    StructuralBase = "structural_base",
    Overlay = "overlay",
    Addon = "addon",
    Evidence = "evidence",
});
string_enum!(SyncStatus {
    Pending = "pending",
    Synced = "synced",
    Failed = "failed",
});
string_enum!(ArtifactFormat {
    Stl = "stl",
    ThreeMf = "3mf",
    Zip = "zip",
});
string_enum!(ContributionResponsibility {
    PrintableParts = "printable_parts",
    HardwareConstraint = "hardware_constraint",
    InformationalEvidence = "informational_evidence",
});
string_enum!(Confidence {
    Low = "low",
    Medium = "medium",
    High = "high",
});
string_enum!(ContributionStatus {
    Proposed = "proposed",
    Confirmed = "confirmed",
    Rejected = "rejected",
});
string_enum!(DifferenceKind {
    Added = "added",
    Removed = "removed",
    Changed = "changed",
    Renamed = "renamed",
    Contradictory = "contradictory",
});
string_enum!(DifferenceResolution {
    ChooseSourceA = "choose_source_a",
    ChooseSourceB = "choose_source_b",
    IncludeBoth = "include_both",
    NotApplicable = "not_applicable",
    Custom = "custom",
});
string_enum!(FilamentInventoryKind {
    Catalog = "catalog",
    Spoolman = "spoolman",
    Custom = "custom",
    Substitute = "substitute",
});
string_enum!(ChecklistCategory {
    TestFit = "test_fit",
    Wiring = "wiring",
    Safety = "safety",
    PrePrint = "pre_print",
    Other = "other",
});

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuildRequirement {
    pub key: String,
    pub value: String,
    pub status: RequirementStatus,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub detail: Option<String>,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceArtifact {
    pub path: String,
    pub format: ArtifactFormat,
    pub printable: bool,
    pub byte_size: i64,
    pub sha256: String,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuildEvidence {
    pub id: String,
    pub url: String,
    pub normalized_url: String,
    pub kind: EvidenceKind,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<EvidenceInputKind>")
    )]
    pub input_kind: Option<EvidenceInputKind>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<Vec<String>>")
    )]
    pub filenames: Option<Vec<String>>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<Vec<SourceArtifact>>")
    )]
    pub artifacts: Option<Vec<SourceArtifact>>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<bool>")
    )]
    pub upload_required: Option<bool>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub derived_from_evidence_id: Option<String>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<i64>")
    )]
    pub source_id: Option<i64>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<SourceRole>")
    )]
    pub source_role: Option<SourceRole>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub title: Option<String>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub extract: Option<String>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub retrieved_at: Option<String>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub content_hash: Option<String>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<SyncStatus>")
    )]
    pub sync_status: Option<SyncStatus>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub pinned_revision: Option<String>,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceContribution {
    pub id: String,
    pub evidence_id: String,
    pub slot: String,
    pub responsibility: ContributionResponsibility,
    pub path_scopes: Vec<String>,
    pub confidence: Confidence,
    pub evidence_text: String,
    pub status: ContributionStatus,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompatibilityFinding {
    pub id: String,
    pub subject: String,
    pub status: RequirementStatus,
    pub detail: String,
    pub evidence_ids: Vec<String>,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuildDifference {
    pub id: String,
    pub group_id: String,
    pub family: String,
    pub kind: DifferenceKind,
    pub source_a: String,
    pub source_b: String,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub path_a: Option<String>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub path_b: Option<String>,
    pub detail: String,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DifferenceGroupResolution {
    pub resolution: DifferenceResolution,
    pub rationale: String,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub custom_resolution: Option<String>,
    pub resolved_at: String,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoleFilamentAssignment {
    pub role: String,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub requested_brand: Option<String>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub requested_name: Option<String>,
    pub inventory_kind: FilamentInventoryKind,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub inventory_id: Option<String>,
    pub color_hex: String,
    pub substitution_confirmed: bool,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuildChecklistItem {
    pub id: String,
    pub title: String,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub detail: Option<String>,
    pub category: ChecklistCategory,
    pub required: bool,
    pub completed: bool,
}

/// The stored brief, version 1. Keys Node's validator does not read are
/// dropped. An optional field Node omits is served as `null`, and a stored
/// `null` reads as absent where Node refuses the brief; Node never writes one.
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuildPlanningBrief {
    pub version: u8,
    pub build_id: i64,
    pub special_request: String,
    pub requirements: Vec<BuildRequirement>,
    pub evidence: Vec<BuildEvidence>,
    pub contributions: Vec<SourceContribution>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<Vec<CompatibilityFinding>>")
    )]
    pub compatibility_findings: Option<Vec<CompatibilityFinding>>,
    pub differences: Vec<BuildDifference>,
    pub resolutions: BTreeMap<String, DifferenceGroupResolution>,
    pub role_filaments: Vec<RoleFilamentAssignment>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<Vec<BuildChecklistItem>>")
    )]
    pub checklist_items: Option<Vec<BuildChecklistItem>>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<Vec<i64>>")
    )]
    pub managed_source_ids: Option<Vec<i64>>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<BTreeMap<String, String>>")
    )]
    pub draft_source_revisions: Option<BTreeMap<String, String>>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<i64>")
    )]
    pub draft_id: Option<i64>,
    pub draft_review_blockers: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReadinessBlocker {
    pub code: String,
    pub detail: String,
}

#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BuildPlanningReadiness {
    pub ready: bool,
    pub blockers: Vec<ReadinessBlocker>,
}

/// Where the brief's Plan draft stands.
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BuildPlanningPhase {
    Preparing,
    Draft {
        draft_id: i64,
    },
    Applied {
        draft_id: i64,
        #[cfg_attr(
            feature = "codegen",
            schemars(with = "pp_contracts::RequiredNullable<i64>")
        )]
        revision_id: Option<i64>,
    },
    Abandoned {
        draft_id: i64,
    },
    MissingDraft {
        draft_id: i64,
    },
}

/// The hydrated brief and what Node derives from it.
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BuildPlanning {
    pub brief: BuildPlanningBrief,
    pub planning_phase: BuildPlanningPhase,
    pub readiness: BuildPlanningReadiness,
    pub grouped_difference_count: usize,
    pub difference_count: usize,
}

/// `GET /plans/{id}/build-planning` body: `planning` is `null` when the Build
/// has no brief.
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BuildPlanningResponse {
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<BuildPlanning>")
    )]
    pub planning: Option<BuildPlanning>,
}

fn safe(value: i64) -> bool {
    (-MAX_SAFE..=MAX_SAFE).contains(&value)
}

fn lower_hex(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Node's `Date.parse` for the stored stamps, limited to RFC 3339, RFC 2822
/// and `YYYY-MM-DD`. Node writes `toISOString()`.
fn date(text: &str) -> bool {
    use time::format_description::well_known::{Rfc2822, Rfc3339};
    let calendar = || {
        let bytes = text.as_bytes();
        let number = |range: std::ops::Range<usize>| text.get(range)?.parse::<u16>().ok();
        let (Some(year), Some(month), Some(day)) = (number(0..4), number(5..7), number(8..10))
        else {
            return false;
        };
        bytes.len() == 10
            && bytes[4] == b'-'
            && bytes[7] == b'-'
            && time::Month::try_from(month as u8).is_ok_and(|month| {
                time::Date::from_calendar_date(i32::from(year), month, day as u8).is_ok()
            })
    };
    time::OffsetDateTime::parse(text, &Rfc3339).is_ok()
        || time::OffsetDateTime::parse(text, &Rfc2822).is_ok()
        || calendar()
}

impl BuildPlanningBrief {
    /// Node's `readBuildPlanningBrief`: parse, validate as
    /// `isBuildPlanningBrief` does, and require the brief to name `build`.
    pub(crate) fn parse(raw: &str, build: i64) -> Result<Self> {
        let value: serde_json::Value =
            serde_json::from_str(raw).context("Invalid Build planning brief data")?;
        if value.is_object() && value.get("version") != Some(&serde_json::Value::from(1)) {
            bail!("Unsupported Build planning brief version");
        }
        let brief: Self =
            serde_json::from_value(value).context("Invalid Build planning brief data")?;
        let evidence_valid = brief.evidence.iter().all(|item| {
            !item.id.is_empty()
                && item.source_id.is_none_or(|id| id > 0 && safe(id))
                && item
                    .artifacts
                    .iter()
                    .flatten()
                    .all(|artifact| safe(artifact.byte_size) && lower_hex(&artifact.sha256, 64))
        });
        let filaments_valid = brief.role_filaments.iter().all(|filament| {
            let hex = filament.color_hex.to_ascii_lowercase();
            hex.strip_prefix('#')
                .is_some_and(|digits| lower_hex(digits, 6))
        });
        ensure!(
            brief.build_id > 0
                && safe(brief.build_id)
                && evidence_valid
                && filaments_valid
                && brief
                    .managed_source_ids
                    .iter()
                    .flatten()
                    .all(|id| safe(*id))
                && brief.draft_id.is_none_or(|id| id > 0 && safe(id))
                && date(&brief.created_at)
                && date(&brief.updated_at),
            "Invalid Build planning brief data"
        );
        ensure!(
            brief.build_id == build,
            "Build planning brief belongs to another Build"
        );
        Ok(brief)
    }
}

/// One tenant Source as Node's `listSources` returns it, with `local_path`
/// resolved under the filesystem policy.
pub(crate) struct PlanningSource {
    pub name: String,
    pub root: Option<PathBuf>,
    pub last_synced_at: Option<String>,
    pub last_commit_sha: Option<String>,
}

impl PlanningSource {
    pub(crate) fn resolve(
        reads: &SourceReads,
        id: i64,
        name: String,
        local_path: Option<&str>,
        synced: (Option<String>, Option<String>),
    ) -> Self {
        Self {
            name,
            root: reads.node_root(id, local_path),
            last_synced_at: synced.0,
            last_commit_sha: synced.1,
        }
    }
}

fn truthy(text: &Option<String>) -> bool {
    text.as_deref().is_some_and(|text| !text.is_empty())
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn difference_id(parts: &[&str]) -> String {
    sha256_hex(parts.join("\0").as_bytes())[..24].to_owned()
}

/// `text.slice(0, units)` in UTF-16 code units, without splitting a pair.
fn js_prefix(text: &str, units: usize) -> &str {
    let mut used = 0;
    for (index, c) in text.char_indices() {
        used += c.len_utf16();
        if used > units {
            return &text[..index];
        }
    }
    text
}

/// Node's `sourceFiles`: every regular file under `root` (symlinks are
/// neither followed nor listed), in libuv's sorted `readdir` order with a
/// LIFO directory stack, mapped to its SHA-256.
fn source_files(root: &Path) -> Result<Vec<(String, String)>> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let mut entries = fs::read_dir(&directory)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let name = entry.file_name();
            if name == ".git" || name == ".printpartner-source-snapshot.json" {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                let path = entry.path();
                let relative = path
                    .strip_prefix(root)?
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                files.push((relative, sha256_hex(&fs::read(&path)?)));
            }
        }
    }
    Ok(files)
}

/// Node's `differenceFamily`: the directory, or the name of a top-level file.
fn family(path: &str) -> String {
    path.rsplit_once('/')
        .map_or(path, |(directory, _)| directory)
        .to_owned()
}

/// Node's `compareSourceTrees`.
fn compare_source_trees(a: (&str, &Path), b: (&str, &Path)) -> Result<Vec<BuildDifference>> {
    let ((name_a, root_a), (name_b, root_b)) = (a, b);
    let files_a = source_files(root_a)?;
    let files_b = source_files(root_b)?;
    let index_a: HashMap<&str, &str> = files_a
        .iter()
        .map(|(p, h)| (p.as_str(), h.as_str()))
        .collect();
    let index_b: HashMap<&str, &str> = files_b
        .iter()
        .map(|(p, h)| (p.as_str(), h.as_str()))
        .collect();
    let difference = |kind, id, path_a: Option<&str>, path_b: Option<&str>, detail| {
        let group = family(path_a.or(path_b).unwrap_or_default());
        BuildDifference {
            id,
            group_id: group.clone(),
            family: group,
            kind,
            source_a: name_a.to_owned(),
            source_b: name_b.to_owned(),
            path_a: path_a.map(str::to_owned),
            path_b: path_b.map(str::to_owned),
            detail,
        }
    };
    let mut differences = Vec::new();
    let mut removed = Vec::new();
    for (path, hash_a) in &files_a {
        match index_b.get(path.as_str()) {
            None => removed.push((path.as_str(), hash_a.as_str())),
            Some(hash_b) if hash_b != hash_a => differences.push(difference(
                DifferenceKind::Changed,
                difference_id(&["changed", name_a, name_b, path, hash_a, hash_b]),
                Some(path),
                Some(path),
                format!("Content differs at {path}"),
            )),
            Some(_) => {}
        }
    }
    let mut added: Vec<(&str, &str)> = files_b
        .iter()
        .filter(|(path, _)| !index_a.contains_key(path.as_str()))
        .map(|(p, h)| (p.as_str(), h.as_str()))
        .collect();
    let mut kept = Vec::new();
    for (path_a, hash_a) in removed {
        let Some(position) = added.iter().position(|(_, hash_b)| *hash_b == hash_a) else {
            kept.push((path_a, hash_a));
            continue;
        };
        let (path_b, _) = added.remove(position);
        differences.push(difference(
            DifferenceKind::Renamed,
            difference_id(&["renamed", name_a, name_b, path_a, path_b, hash_a]),
            Some(path_a),
            Some(path_b),
            format!("{path_a} was renamed to {path_b}"),
        ));
    }
    for (path, hash) in kept {
        differences.push(difference(
            DifferenceKind::Removed,
            difference_id(&["removed", name_a, name_b, path, hash]),
            Some(path),
            None,
            format!("{path} is absent from {name_b}"),
        ));
    }
    for (path, hash) in added {
        differences.push(difference(
            DifferenceKind::Added,
            difference_id(&["added", name_a, name_b, path, hash]),
            None,
            Some(path),
            format!("{path} exists only in {name_b}"),
        ));
    }
    differences.sort_by(|left, right| {
        folder_compare(&left.group_id, &right.group_id)
            .then_with(|| folder_compare(&left.id, &right.id))
    });
    Ok(differences)
}

/// Each group's sorted difference ids.
fn group_signatures(differences: &[BuildDifference]) -> HashMap<&str, Vec<&str>> {
    let mut groups: HashMap<&str, Vec<&str>> = HashMap::new();
    for difference in differences {
        groups
            .entry(&difference.group_id)
            .or_default()
            .push(&difference.id);
    }
    for ids in groups.values_mut() {
        ids.sort_unstable();
    }
    groups
}

fn identity(differences: &[BuildDifference]) -> Vec<(&str, &str)> {
    let mut pairs: Vec<_> = differences
        .iter()
        .map(|d| (d.group_id.as_str(), d.id.as_str()))
        .collect();
    pairs.sort_unstable();
    pairs
}

/// The Source an evidence item names and its resolved root, when it has one.
fn rooted<'a>(
    sources: &'a HashMap<i64, PlanningSource>,
    evidence: &BuildEvidence,
) -> Option<(&'a str, &'a Path)> {
    let source = sources.get(&evidence.source_id?)?;
    Some((source.name.as_str(), source.root.as_deref()?))
}

/// The Source-tree differences between the synced structural base and each
/// synced overlay, or the stored non-contradictory ones before sync.
fn tree_differences(
    brief: &BuildPlanningBrief,
    evidence: &[BuildEvidence],
    sources: &HashMap<i64, PlanningSource>,
) -> Result<Vec<BuildDifference>> {
    let synced = |item: &&BuildEvidence| item.sync_status == Some(SyncStatus::Synced);
    let structural = evidence
        .iter()
        .find(|item| item.source_role == Some(SourceRole::StructuralBase));
    let mut differences: Vec<BuildDifference> = brief
        .differences
        .iter()
        .filter(|d| d.kind != DifferenceKind::Contradictory)
        .cloned()
        .collect();
    if let Some(structural) = structural.filter(synced) {
        differences = Vec::new();
        if let Some(a) = rooted(sources, structural) {
            let overlays = evidence
                .iter()
                .filter(|item| item.source_role == Some(SourceRole::Overlay))
                .filter(synced);
            for overlay in overlays {
                let Some(b) = rooted(sources, overlay) else {
                    continue;
                };
                for difference in compare_source_trees(a, b)? {
                    differences.push(BuildDifference {
                        id: difference_id(&[
                            &difference.id,
                            structural.pinned_revision.as_deref().unwrap_or_default(),
                            overlay.pinned_revision.as_deref().unwrap_or_default(),
                        ]),
                        group_id: format!(
                            "{}:{}:{}",
                            structural.id, overlay.id, difference.group_id
                        ),
                        ..difference
                    });
                }
            }
        }
    }
    Ok(differences)
}

/// Contradictory documentation claims: evidence with the same title and
/// different extracts.
fn claim_differences(evidence: &[BuildEvidence], differences: &mut Vec<BuildDifference>) {
    let mut claims: Vec<(String, Vec<&BuildEvidence>)> = Vec::new();
    for item in evidence {
        let subject = item
            .title
            .as_deref()
            .map(|title| js_trim(title).to_lowercase())
            .unwrap_or_default();
        if subject.is_empty()
            || item
                .extract
                .as_deref()
                .is_none_or(|e| js_trim(e).is_empty())
        {
            continue;
        }
        match claims.iter_mut().find(|(known, _)| *known == subject) {
            Some((_, items)) => items.push(item),
            None => claims.push((subject, vec![item])),
        }
    }
    for (subject, items) in &claims {
        for (index, left) in items.iter().enumerate() {
            for right in &items[index + 1..] {
                let left_text = js_trim(left.extract.as_deref().unwrap_or_default());
                let right_text = js_trim(right.extract.as_deref().unwrap_or_default());
                if left_text == right_text {
                    continue;
                }
                differences.push(BuildDifference {
                    id: difference_id(&[
                        "contradictory",
                        &left.id,
                        &right.id,
                        subject,
                        left_text,
                        right_text,
                    ]),
                    group_id: format!("claims:{}", &sha256_hex(subject.as_bytes())[..12]),
                    family: "documentation_claims".into(),
                    kind: DifferenceKind::Contradictory,
                    source_a: left.url.clone(),
                    source_b: right.url.clone(),
                    path_a: None,
                    path_b: None,
                    detail: format!(
                        "Conflicting claims about {}: {} <> {}",
                        left.title.as_deref().unwrap_or(subject),
                        js_prefix(left_text, 160),
                        js_prefix(right_text, 160)
                    ),
                });
            }
        }
    }
}

/// Node's `hydrateBuildPlanningBrief`. Reads the synced structural and
/// overlay Source trees; writes nothing.
pub(crate) fn hydrate(
    brief: BuildPlanningBrief,
    sources: &HashMap<i64, PlanningSource>,
) -> Result<BuildPlanningBrief> {
    let evidence: Vec<BuildEvidence> = brief
        .evidence
        .iter()
        .map(|item| {
            let Some(source) = item.source_id.and_then(|id| sources.get(&id)) else {
                return item.clone();
            };
            let synced = source.root.is_some()
                && truthy(&source.last_synced_at)
                && truthy(&source.last_commit_sha);
            BuildEvidence {
                sync_status: Some(if synced {
                    SyncStatus::Synced
                } else {
                    SyncStatus::Pending
                }),
                pinned_revision: synced.then(|| source.last_commit_sha.clone()).flatten(),
                ..item.clone()
            }
        })
        .collect();
    let mut differences = tree_differences(&brief, &evidence, sources)?;
    claim_differences(&evidence, &mut differences);
    let previous = group_signatures(&brief.differences);
    let current = group_signatures(&differences);
    let resolutions: BTreeMap<String, DifferenceGroupResolution> = brief
        .resolutions
        .iter()
        .filter(|(group, _)| {
            current
                .get(group.as_str())
                .is_some_and(|ids| previous.get(group.as_str()) == Some(ids))
        })
        .map(|(group, resolution)| (group.clone(), resolution.clone()))
        .collect();
    let same_differences = identity(&brief.differences) == identity(&differences);
    let mut revisions = BTreeMap::new();
    for item in &evidence {
        if let (Some(id), Some(revision)) = (item.source_id, item.pinned_revision.as_ref()) {
            revisions.insert(id.to_string(), revision.clone());
        }
    }
    let same_sources = brief
        .draft_source_revisions
        .as_ref()
        .is_none_or(|stored| *stored == revisions);
    let kept = same_differences && same_sources && resolutions.len() == brief.resolutions.len();
    Ok(BuildPlanningBrief {
        draft_id: brief.draft_id.filter(|_| kept),
        evidence,
        differences,
        resolutions,
        ..brief
    })
}

/// Readiness blockers from each evidence item's upload, sync and provenance.
fn evidence_blockers(brief: &BuildPlanningBrief, block: &mut impl FnMut(&str, String)) {
    for evidence in &brief.evidence {
        let url = &evidence.normalized_url;
        let uploaded = brief.evidence.iter().any(|candidate| {
            candidate.input_kind == Some(EvidenceInputKind::Upload)
                && candidate.derived_from_evidence_id.as_deref() == Some(evidence.id.as_str())
        });
        if evidence.input_kind == Some(EvidenceInputKind::ModelPage)
            && evidence.upload_required == Some(true)
            && !uploaded
        {
            block(
                "model_files_missing",
                format!("{url} needs an attached file upload"),
            );
        }
        if let Some(status @ (SyncStatus::Pending | SyncStatus::Failed)) = evidence.sync_status {
            block("source_sync", format!("{url}: {}", status.as_str()));
        }
        let pinned = evidence
            .pinned_revision
            .as_deref()
            .filter(|p| !p.is_empty());
        if evidence.sync_status == Some(SyncStatus::Synced) && pinned.is_none() {
            block("source_provenance", format!("{url} has no pinned revision"));
        }
        let built = |source: i64| {
            brief
                .draft_source_revisions
                .as_ref()
                .and_then(|revisions| revisions.get(&source.to_string()))
                .map(String::as_str)
        };
        if let (Some(draft), Some(source), Some(pinned)) =
            (brief.draft_id, evidence.source_id, pinned)
            && evidence.source_role != Some(SourceRole::Evidence)
            && built(source) != Some(pinned)
        {
            block(
                "draft_source_changed",
                format!("{url} changed after draft {draft} was built"),
            );
        }
    }
}

/// Node's `deriveBuildPlanningReadiness`.
pub(crate) fn readiness(brief: &BuildPlanningBrief) -> BuildPlanningReadiness {
    let mut blockers = Vec::new();
    let mut block = |code: &str, detail: String| {
        blockers.push(ReadinessBlocker {
            code: code.to_owned(),
            detail,
        });
    };
    let printable: Vec<&BuildEvidence> = brief
        .evidence
        .iter()
        .filter(|e| e.source_id.is_some() && e.kind != EvidenceKind::InformationalEvidence)
        .collect();
    if printable.iter().any(|e| e.source_role.is_none()) {
        block(
            "source_role",
            "Every printable Source needs a confirmed Build role".into(),
        );
    }
    let structural = printable
        .iter()
        .filter(|e| e.source_role == Some(SourceRole::StructuralBase))
        .count();
    if !printable.is_empty() && structural != 1 {
        block(
            "source_roles",
            "Build planning requires exactly one structural base Source".into(),
        );
    }
    evidence_blockers(brief, &mut block);
    let open = [
        RequirementStatus::Unverified,
        RequirementStatus::Incompatible,
    ];
    for requirement in &brief.requirements {
        if open.contains(&requirement.status) {
            block(
                &format!("requirement_{}", requirement.status.as_str()),
                format!("{}: {}", requirement.key, requirement.value),
            );
        }
    }
    for contribution in &brief.contributions {
        if contribution.status == ContributionStatus::Proposed {
            block(
                "unconfirmed_contribution",
                format!("{}: {}", contribution.slot, contribution.id),
            );
        }
    }
    for finding in brief.compatibility_findings.iter().flatten() {
        if open.contains(&finding.status) {
            block(
                &format!("compatibility_{}", finding.status.as_str()),
                format!("{}: {}", finding.subject, finding.detail),
            );
        }
    }
    let mut seen = HashSet::new();
    for difference in &brief.differences {
        if seen.insert(difference.group_id.as_str())
            && !brief.resolutions.contains_key(&difference.group_id)
        {
            block("open_difference", difference.group_id.clone());
        }
    }
    for assignment in &brief.role_filaments {
        if assignment.inventory_kind == FilamentInventoryKind::Substitute
            && !assignment.substitution_confirmed
        {
            block("unconfirmed_filament_substitute", assignment.role.clone());
        }
    }
    for item in brief.checklist_items.iter().flatten() {
        if item.required && !item.completed {
            block("checklist_incomplete", item.title.clone());
        }
    }
    for detail in &brief.draft_review_blockers {
        block("draft_review", detail.clone());
    }
    if brief.draft_id.is_none() {
        block("draft_missing", "Rebuild and review a plan draft".into());
    }
    BuildPlanningReadiness {
        ready: blockers.is_empty(),
        blockers,
    }
}

/// Node's `resolvedSourcePathExclusions` blockers: a custom resolution, or a
/// resolved printable path whose Source the brief's evidence does not name.
pub(crate) fn source_choice_blockers(
    brief: &BuildPlanningBrief,
    sources: &HashMap<i64, PlanningSource>,
) -> Vec<String> {
    let named: HashSet<&str> = brief
        .evidence
        .iter()
        .filter_map(|item| sources.get(&item.source_id?))
        .map(|source| source.name.as_str())
        .collect();
    let mut groups: Vec<(&str, Vec<&BuildDifference>)> = Vec::new();
    for difference in &brief.differences {
        match groups.iter_mut().find(|(id, _)| *id == difference.group_id) {
            Some((_, members)) => members.push(difference),
            None => groups.push((&difference.group_id, vec![difference])),
        }
    }
    let mut blockers = Vec::new();
    for (group, differences) in groups {
        let Some(resolution) = brief.resolutions.get(group).map(|r| r.resolution) else {
            continue;
        };
        let (keep_a, keep_b) = match resolution {
            DifferenceResolution::IncludeBoth => continue,
            DifferenceResolution::Custom => {
                blockers.push(format!(
                    "{group}: custom resolution requires explicit draft review"
                ));
                continue;
            }
            DifferenceResolution::ChooseSourceA => (true, false),
            DifferenceResolution::ChooseSourceB => (false, true),
            DifferenceResolution::NotApplicable => (false, false),
        };
        for difference in differences {
            let sides = [
                (keep_a, &difference.source_a, &difference.path_a),
                (keep_b, &difference.source_b, &difference.path_b),
            ];
            for (kept, source, path) in sides {
                let Some(path) = path.as_deref().filter(|_| !kept) else {
                    continue;
                };
                if path.to_lowercase().ends_with(".stl") && !named.contains(source.as_str()) {
                    blockers.push(format!(
                        "{group}: resolved printable path is missing from the draft: {source}/{path}"
                    ));
                }
            }
        }
    }
    blockers
}

/// Node's `deriveBuildPlanningPhase`, from the stored draft row the hydrated
/// brief names (`state`, `consumed_revision_id`), or `None` when it is gone.
pub(crate) fn phase(
    draft_id: Option<i64>,
    draft: Option<&(String, Option<i64>)>,
) -> Result<BuildPlanningPhase> {
    let Some(draft_id) = draft_id else {
        return Ok(BuildPlanningPhase::Preparing);
    };
    Ok(
        match draft.map(|(state, revision)| (state.as_str(), *revision)) {
            None => BuildPlanningPhase::MissingDraft { draft_id },
            Some(("open", _)) => BuildPlanningPhase::Draft { draft_id },
            Some(("consumed", revision_id)) => BuildPlanningPhase::Applied {
                draft_id,
                revision_id,
            },
            Some(("abandoned", _)) => BuildPlanningPhase::Abandoned { draft_id },
            Some((state, _)) => bail!("Unknown Plan draft state {state}"),
        },
    )
}
