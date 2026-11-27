use super::model::KitManifest;
use crate::manifest_text::{JsText, ManifestText, OptionGroupId, VariantId};
use crate::working_drafts::observation::{BuilderGroup, BuilderGroups, BuilderVariant};
use std::collections::BTreeMap;

pub(crate) struct ManifestBuilder {
    pub profile_id: i64,
    pub sources: Vec<ManifestSource>,
    pub resolved_selections: KitManifest,
    pub merged_option_groups: BuilderGroups,
    pub variant_sources: VariantSources,
}

pub(crate) type VariantSources = BTreeMap<OptionGroupId, BTreeMap<VariantId, Vec<VariantSource>>>;

pub(crate) struct VariantSource {
    pub source_id: i64,
    pub source_name: String,
}

pub(crate) struct ManifestSource {
    pub source_id: i64,
    pub layer_type: String,
    pub name: String,
    pub role: String,
    pub url: String,
    pub loaded_yaml: Option<String>,
    pub option_groups: BuilderGroups,
    pub scanned_parts: Vec<String>,
}

impl ManifestBuilder {
    pub fn write_json(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(b"{\"profile_id\":");
        output.extend_from_slice(self.profile_id.to_string().as_bytes());
        output.extend_from_slice(b",\"sources\":[");
        for (index, source) in self.sources.iter().enumerate() {
            if index > 0 {
                output.push(b',');
            }
            source.write_json(output);
        }
        output.extend_from_slice(b"],\"resolved_selections\":");
        self.resolved_selections.write_selections_json(output);
        output.extend_from_slice(b",\"merged_option_groups\":");
        self.write_groups(output);
        output.push(b'}');
    }

    pub fn write_option_groups(&self, stored: &KitManifest, output: &mut Vec<u8>) {
        output.extend_from_slice(b"{\"plan_id\":");
        output.extend_from_slice(self.profile_id.to_string().as_bytes());
        output.extend_from_slice(b",\"selections\":");
        stored.write_selections_json(output);
        output.extend_from_slice(b",\"option_groups\":");
        self.write_groups(output);
        output.extend_from_slice(b",\"sources\":[");
        for (index, source) in self.sources.iter().enumerate() {
            if index > 0 {
                output.push(b',');
            }
            let summary = serde_json::json!({
                "source_id": source.source_id, "name": source.name, "layer_type": source.layer_type,
                "exists": source.loaded_yaml.is_some(), "scanned_part_count": source.scanned_parts.len(),
            });
            output.extend_from_slice(summary.to_string().as_bytes());
        }
        output.extend_from_slice(b"]}");
    }

    fn write_groups(&self, output: &mut Vec<u8>) {
        output.push(b'{');
        for (index, (id, group)) in self.merged_option_groups.iter().enumerate() {
            if index > 0 {
                output.push(b',');
            }
            write_open_group(id, group, output);
            write_bounds(group, output);
            output.extend_from_slice(b",\"variants\":[");
            let sources = self.variant_sources.get(id);
            for (index, variant) in group.variants.iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                write_open_variant(variant, output);
                let sources = sources
                    .and_then(|sources| sources.get(&variant.id))
                    .map_or(&[][..], Vec::as_slice);
                if let Some(first) = sources.first() {
                    output.push(b',');
                    write_variant_source_fields(first, output);
                }
                if sources.len() > 1 {
                    output.extend_from_slice(b",\"sources\":[");
                    for (index, source) in sources.iter().enumerate() {
                        if index > 0 {
                            output.push(b',');
                        }
                        output.push(b'{');
                        write_variant_source_fields(source, output);
                        output.push(b'}');
                    }
                    output.push(b']');
                }
                output.push(b'}');
            }
            output.extend_from_slice(b"]}");
        }
        output.push(b'}');
    }
}

impl ManifestSource {
    fn write_json(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(b"{\"source_id\":");
        output.extend_from_slice(self.source_id.to_string().as_bytes());
        output.extend_from_slice(b",\"layer_type\":");
        string(&self.layer_type, output);
        output.extend_from_slice(b",\"name\":");
        string(&self.name, output);
        output.extend_from_slice(b",\"role\":");
        string(&self.role, output);
        output.extend_from_slice(b",\"url\":");
        string(&self.url, output);
        output.extend_from_slice(if self.loaded_yaml.is_some() {
            b",\"exists\":true"
        } else {
            b",\"exists\":false"
        });
        output.extend_from_slice(b",\"path\":\"print-partner.manifest.yaml\",\"yaml\":");
        match &self.loaded_yaml {
            Some(yaml) => string(yaml, output),
            None => string(
                &format!(
                    "format: print-partner-manifest-v2\nversion: 2\nproject: {}\n",
                    self.name
                ),
                output,
            ),
        }
        output.extend_from_slice(
            b",\"document\":{\"format\":\"print-partner-manifest-v2\",\"version\":2,\"project\":",
        );
        string(&self.name, output);
        output.extend_from_slice(b",\"option_groups\":{");
        for (index, (id, group)) in self.option_groups.iter().enumerate() {
            if index > 0 {
                output.push(b',');
            }
            write_open_group(id, group, output);
            output.extend_from_slice(b",\"variants\":[");
            for (index, variant) in group.variants.iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                write_open_variant(variant, output);
                output.push(b'}');
            }
            output.push(b']');
            write_bounds(group, output);
            output.push(b'}');
        }
        output.extend_from_slice(b"}},\"scanned_parts\":[");
        for (index, part) in self.scanned_parts.iter().enumerate() {
            if index > 0 {
                output.push(b',');
            }
            output.extend_from_slice(b"{\"match\":");
            string(part, output);
            output.extend_from_slice(b",\"relative_path\":");
            string(part, output);
            output.push(b'}');
        }
        output.extend_from_slice(b"]}");
    }
}

fn write_open_group(id: &OptionGroupId, group: &BuilderGroup, output: &mut Vec<u8>) {
    id.0.write_json(output);
    output.extend_from_slice(b":{\"rule\":");
    string(&group.rule, output);
    output.extend_from_slice(b",\"label\":");
    label(group.label.as_ref(), output);
    output.extend_from_slice(b",\"parts\":");
    texts(&group.parts, output);
}

fn write_open_variant(variant: &BuilderVariant, output: &mut Vec<u8>) {
    output.extend_from_slice(b"{\"id\":");
    variant.id.0.write_json(output);
    output.extend_from_slice(b",\"label\":");
    label(variant.label.as_ref(), output);
    output.extend_from_slice(b",\"parts\":");
    texts(&variant.parts, output);
    output.extend_from_slice(b",\"excludes\":");
    texts(&variant.excludes, output);
}

fn write_variant_source_fields(source: &VariantSource, output: &mut Vec<u8>) {
    output.extend_from_slice(b"\"source_id\":");
    output.extend_from_slice(source.source_id.to_string().as_bytes());
    output.extend_from_slice(b",\"source_name\":");
    string(&source.source_name, output);
}

fn write_bounds(group: &BuilderGroup, output: &mut Vec<u8>) {
    for (key, bound) in [
        (&b",\"min\":"[..], group.min),
        (&b",\"max\":"[..], group.max),
    ] {
        output.extend_from_slice(key);
        match bound {
            Some(bound) => output.extend_from_slice(bound.to_string().as_bytes()),
            None => output.extend_from_slice(b"null"),
        }
    }
}

fn string(value: &str, output: &mut Vec<u8>) {
    JsText::scalar(value).write_json(output);
}

fn label(value: Option<&ManifestText>, output: &mut Vec<u8>) {
    match value {
        Some(text) => text.0.write_json(output),
        None => output.extend_from_slice(b"null"),
    }
}

fn texts(values: &[ManifestText], output: &mut Vec<u8>) {
    output.push(b'[');
    for (index, text) in values.iter().enumerate() {
        if index > 0 {
            output.push(b',');
        }
        text.0.write_json(output);
    }
    output.push(b']');
}
