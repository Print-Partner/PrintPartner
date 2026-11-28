use pp_services::kit::{EditableKitRecipe, KitBundlePart, source_refs::KitSourceRef};
use pp_storage::{
    build_graph::kit_recipe::{EditableKitRecipeCapture, KitPart, KitSource},
    manifest_text::JsText,
};
use serde_json::{Value, json};
use std::fmt;

#[derive(Debug)]
pub enum KitRecipeEncodingError {
    Manifest(serde_json::Error),
    PartManifest(&'static str),
}

impl fmt::Display for KitRecipeEncodingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(_) => {
                f.write_str("kit manifest cannot be represented by the bundle encoder")
            }
            Self::PartManifest(field) => write!(
                f,
                "part manifest field {field} cannot be represented by the bundle encoder"
            ),
        }
    }
}

impl std::error::Error for KitRecipeEncodingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Manifest(error) => Some(error),
            Self::PartManifest(_) => None,
        }
    }
}

pub fn project(
    capture: EditableKitRecipeCapture,
) -> Result<EditableKitRecipe, KitRecipeEncodingError> {
    let sources: Vec<Value> = capture.sources.into_iter().map(source).collect();
    let layers = capture
        .layers
        .into_iter()
        .map(|layer| {
            json!({
                "layer_order": layer.order,
                "layer_type": layer.kind,
                "project": layer.source_index.and_then(|index| sources.get(index)),
            })
        })
        .collect();
    let mut manifest = Vec::new();
    capture.manifest.write_json(&mut manifest);
    let kit_manifest =
        serde_json::from_slice(&manifest).map_err(KitRecipeEncodingError::Manifest)?;
    let working_parts = capture
        .parts
        .into_iter()
        .map(part)
        .collect::<Result<_, _>>()?;
    Ok(EditableKitRecipe {
        profile_name: capture.name,
        order_number: capture.order_number,
        layers,
        sources,
        kit_manifest,
        working_parts,
    })
}

fn source(source: KitSource) -> Value {
    KitSourceRef {
        name: source.name,
        url: source.url,
        branch: source.branch,
        tag: source.tag,
        source_kind: source.kind,
        source_type: source.source_type,
        role: source.role,
        category: source.category.unwrap_or_else(|| "unassigned".to_owned()),
        import_rules: source.import_rules,
        manifest_community_slug: source.manifest_community_slug,
        layer_type: None,
    }
    .to_export_record()
}

fn part(part: KitPart) -> Result<KitBundlePart, KitRecipeEncodingError> {
    let text = |value: Option<JsText>, field| {
        value
            .map(|text| {
                text.as_scalar()
                    .ok_or(KitRecipeEncodingError::PartManifest(field))
            })
            .transpose()
    };
    Ok(KitBundlePart {
        match_key: part.match_key,
        relative_path: part.relative_path,
        filename: part.filename,
        source_layer: part.source_layer,
        status: part.status,
        role: part.role,
        filament_color_id: part.filament_color_id,
        filament_custom_hex: part.filament_custom_hex,
        quantity_inferred: part.quantity_inferred,
        quantity_override: part.quantity_override,
        quantity_effective: part.quantity_effective,
        included: part.included,
        notes: part.notes,
        geometry_same: part.geometry_same,
        requirement: text(
            part.manifest.requirement.map(|value| value.0),
            "requirement",
        )?,
        option_group_id: text(
            part.manifest.option_group_id.map(|value| value.0),
            "option_group_id",
        )?,
        manifest_source: part.manifest.manifest_source,
    })
}
