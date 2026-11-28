use crate::{
    WriterFailure,
    build_graph::{self, PositiveId},
    manifest_options::KitManifest,
    manifest_text::{ManifestText, OptionGroupId, PartManifestMetadata, read_manifest_text},
    read_model::{Credential, ReadClient},
};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, de::IgnoredAny};
use std::{
    collections::HashMap,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

const MAX_PARTS: usize = 200_000;

#[derive(Debug, PartialEq)]
pub struct EditableKitRecipeCapture {
    pub name: String,
    pub order_number: Option<String>,
    pub layers: Vec<KitLayer>,
    pub sources: Vec<KitSource>,
    pub manifest: KitManifest,
    pub parts: Vec<KitPart>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct KitLayer {
    pub order: i64,
    pub kind: String,
    pub source_index: Option<usize>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct KitSource {
    pub name: String,
    pub url: String,
    pub branch: String,
    pub tag: Option<String>,
    pub kind: String,
    pub source_type: String,
    pub role: String,
    pub category: Option<String>,
    pub import_rules: Vec<String>,
    pub manifest_community_slug: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct KitPart {
    pub match_key: String,
    pub relative_path: String,
    pub filename: String,
    pub source_layer: String,
    pub status: String,
    pub role: String,
    pub filament_color_id: Option<String>,
    pub filament_custom_hex: Option<String>,
    pub quantity_inferred: i64,
    pub quantity_override: Option<i64>,
    pub quantity_effective: i64,
    pub included: bool,
    pub notes: String,
    pub geometry_same: Option<bool>,
    pub manifest: PartManifestMetadata,
}

#[derive(Debug, thiserror::Error)]
pub enum KitRecipeFailure {
    #[error("Plan not found")]
    MissingPlan,
    #[error("Kit recipe exceeds the part limit")]
    TooManyParts,
    #[error("Kit recipe exceeds the layer limit")]
    TooManyLayers,
    #[error(transparent)]
    Storage(#[from] WriterFailure),
}

impl ReadClient {
    /// Capture the saved recipe, not an open draft or Checkoff progress.
    pub fn editable_kit_recipe(
        &self,
        credential: Credential,
        plan: PositiveId,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<EditableKitRecipeCapture, KitRecipeFailure> {
        self.storage
            .read("kit_recipe.read", cancelled, wait, |tx| {
                let tenant = crate::write::authorize(tx, &credential, self.policy)?;
                capture(tx, tenant.id(), plan.get() as i64, cancelled)
            })?
    }
}

fn capture(
    tx: &Transaction<'_>,
    tenant: &str,
    plan: i64,
    cancelled: &AtomicBool,
) -> Result<EditableKitRecipeCapture, KitRecipeFailure> {
    let (name, order_number) = tx
        .query_row(
            "SELECT name,order_number FROM build_profiles WHERE tenant_id=?1 AND id=?2",
            params![tenant, plan],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(WriterFailure::from)?
        .ok_or(KitRecipeFailure::MissingPlan)?;
    let layers =
        build_graph::layers(tx, tenant, plan, cancelled).map_err(|error| match error
            .downcast_ref::<build_graph::Failure>(
        ) {
            Some(build_graph::Failure::Cancelled) => WriterFailure::Cancelled.into(),
            Some(build_graph::Failure::TooManyLayers) => KitRecipeFailure::TooManyLayers,
            _ => WriterFailure::from(error).into(),
        })?;
    let mut sources = Vec::new();
    let mut indexes = HashMap::new();
    let mut kit_layers = Vec::with_capacity(layers.len());
    for layer in layers {
        check_cancelled(cancelled)?;
        let source_index = match layer.project_id {
            Some(id) if layer.project_name.is_some() => {
                if let Some(&index) = indexes.get(&id) {
                    Some(index)
                } else {
                    let source = source(tx, tenant, id)?;
                    let index = sources.len();
                    sources.push(source);
                    indexes.insert(id, index);
                    Some(index)
                }
            }
            _ => None,
        };
        kit_layers.push(KitLayer {
            order: layer.layer_order,
            kind: layer.layer_type,
            source_index,
        });
    }
    let manifest =
        build_graph::manifest_options::read_kit(tx, tenant, plan).map_err(WriterFailure::from)?;
    let parts = parts(tx, tenant, plan, cancelled)?;
    check_cancelled(cancelled)?;
    Ok(EditableKitRecipeCapture {
        name,
        order_number,
        layers: kit_layers,
        sources,
        manifest,
        parts,
    })
}

fn check_cancelled(cancelled: &AtomicBool) -> Result<(), WriterFailure> {
    if cancelled.load(Ordering::Acquire) {
        return Err(WriterFailure::Cancelled);
    }
    Ok(())
}

#[derive(Default, Deserialize)]
#[serde(untagged)]
enum Category {
    #[default]
    #[serde(skip)]
    Absent,
    Name(String),
    Unassigned(IgnoredAny),
}

#[derive(Default, Deserialize)]
struct SourceMetadata {
    #[serde(default)]
    category: Category,
}

fn source(tx: &Transaction<'_>, tenant: &str, id: i64) -> Result<KitSource, WriterFailure> {
    let (mut source, metadata, rules) = tx.query_row(
        "SELECT name,url,branch,tag,source_kind,source_type,role,metadata_json,
                imported_paths,manifest_community_slug
         FROM projects WHERE tenant_id=?1 AND id=?2",
        params![tenant, id],
        |row| {
            Ok((
                KitSource {
                    name: row.get(0)?,
                    url: portable_url(row.get::<_, String>(1)?),
                    branch: row.get(2)?,
                    tag: row.get(3)?,
                    kind: row.get(4)?,
                    source_type: row.get(5)?,
                    role: row.get(6)?,
                    category: None,
                    import_rules: Vec::new(),
                    manifest_community_slug: row.get(9)?,
                },
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
            ))
        },
    )?;
    let metadata: SourceMetadata = metadata
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    source.category = match metadata.category {
        Category::Name(name) if !name.trim().is_empty() => Some(name.trim().to_owned()),
        Category::Absent if source.role == "base" => Some("Printer kits".into()),
        Category::Absent if source.role == "addon" => Some("Mods".into()),
        _ => None,
    };
    source.import_rules = crate::catalog::rules(rules).0;
    Ok(source)
}

fn portable_url(raw: String) -> String {
    match url::Url::parse(&raw) {
        Ok(url)
            if matches!(url.scheme(), "http" | "https" | "git" | "ssh")
                && url.has_host()
                && (url.username().is_empty()
                    || url.scheme() == "ssh" && url.username() == "git")
                && url.password().is_none() =>
        {
            raw
        }
        _ => String::new(),
    }
}

fn parts(
    tx: &Transaction<'_>,
    tenant: &str,
    plan: i64,
    cancelled: &AtomicBool,
) -> Result<Vec<KitPart>, KitRecipeFailure> {
    let mut query = tx
        .prepare(
            "SELECT match_key,relative_path,filename,source_layer,status,role,
                    filament_color_id,filament_custom_hex,quantity_auto,quantity_override,
                    quantity_effective,included,notes,geometry_same,requirement,option_group_id,
                    manifest_source
             FROM parts WHERE tenant_id=?1 AND profile_id=?2 ORDER BY filename,id LIMIT ?3",
        )
        .map_err(WriterFailure::from)?;
    let mut rows = query
        .query(params![tenant, plan, MAX_PARTS as i64 + 1])
        .map_err(WriterFailure::from)?;
    let mut parts = Vec::new();
    while let Some(row) = rows.next().map_err(WriterFailure::from)? {
        check_cancelled(cancelled)?;
        if parts.len() == MAX_PARTS {
            return Err(KitRecipeFailure::TooManyParts);
        }
        parts.push(part(row)?);
    }
    Ok(parts)
}

fn part(row: &rusqlite::Row<'_>) -> Result<KitPart, WriterFailure> {
    Ok(KitPart {
        match_key: row.get(0)?,
        relative_path: row.get(1)?,
        filename: row.get(2)?,
        source_layer: row.get(3)?,
        status: row.get(4)?,
        role: row.get(5)?,
        filament_color_id: row.get(6)?,
        filament_custom_hex: row.get(7)?,
        quantity_inferred: row.get(8)?,
        quantity_override: row.get(9)?,
        quantity_effective: row.get(10)?,
        included: row.get(11)?,
        notes: row.get(12)?,
        geometry_same: row.get(13)?,
        manifest: PartManifestMetadata {
            requirement: read_manifest_text(row.get_ref(14)?)
                .map_err(WriterFailure::from)?
                .map(ManifestText),
            option_group_id: read_manifest_text(row.get_ref(15)?)
                .map_err(WriterFailure::from)?
                .map(OptionGroupId),
            manifest_source: row.get(16)?,
        },
    })
}
