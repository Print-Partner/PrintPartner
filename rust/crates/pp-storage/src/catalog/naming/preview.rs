use super::*;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamingOverride {
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub roles: Option<Vec<RoleOverride>>,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub quantity: Option<QuantityOverride>,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub slug: Option<SlugOverride>,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub folder_rules: Option<Vec<FolderRule>>,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub export_role_order: Option<Vec<RoleId>>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleOverride {
    pub id: RoleId,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub label: Option<String>,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub markers: Option<Vec<String>>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuantityOverride {
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub regex: Option<String>,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub default: Option<u64>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlugOverride {
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub strip_markers: Option<bool>,
    #[serde(
        default,
        deserialize_with = "optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub strip_quantity: Option<bool>,
}
#[derive(Debug, Serialize)]
pub struct NamingPreview {
    pub role: RoleId,
    pub quantity: Option<serde_json::Number>,
    pub part_slug: String,
    pub filename: String,
}
fn merge(mut base: NamingProfile, overrides: NamingOverride) -> Result<NamingProfile> {
    if let Some(roles) = overrides.roles {
        ensure!(
            !roles.is_empty(),
            CatalogFailure::Input("Empty roles override".into())
        );
        let mut seen = Vec::new();
        for role in roles {
            ensure!(
                !seen.contains(&role.id),
                CatalogFailure::Input("Duplicate role override".into())
            );
            seen.push(role.id);
            let position = base.roles.iter().position(|r| r.id == role.id);
            let target = if let Some(i) = position {
                &mut base.roles[i]
            } else {
                let label = match role.id {
                    RoleId::Primary => "primary",
                    RoleId::Accent => "accent",
                    RoleId::Clear => "clear",
                    RoleId::Opaque => "opaque",
                }
                .to_owned();
                base.roles.push(Role {
                    id: role.id,
                    label,
                    markers: vec![],
                });
                let index = base.roles.len() - 1;
                &mut base.roles[index]
            };
            if let Some(label) = role.label {
                target.label = label
            }
            if let Some(markers) = role.markers {
                target.markers = markers
            }
        }
    }
    if let Some(q) = overrides.quantity {
        if let Some(r) = q.regex {
            base.quantity.regex = r
        }
        if let Some(d) = q.default {
            base.quantity.default = d
        }
    }
    if let Some(s) = overrides.slug {
        if let Some(v) = s.strip_markers {
            base.slug.strip_markers = v
        }
        if let Some(v) = s.strip_quantity {
            base.slug.strip_quantity = v
        }
    }
    if let Some(r) = overrides.folder_rules {
        base.folder_rules = r
    }
    if let Some(r) = overrides.export_role_order {
        base.export_role_order = r
    }
    base.validate_input()
}
fn escape_regex(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if ".*+?^${}()|[]\\".contains(ch) {
            out.push('\\')
        }
        out.push(ch)
    }
    out
}
fn replace_first(text: &str, pattern: &str, replacement: &str) -> String {
    if let Ok(regex) = regress::Regex::with_flags(pattern, "i")
        && let Some(m) = regex.find(text)
    {
        let mut result = text.to_owned();
        result.replace_range(m.range(), replacement);
        return result;
    }
    text.to_owned()
}
fn quantity_strip(pattern: &str) -> Option<regress::Regex> {
    let mut pattern = pattern.replace("(?:", "(");
    let named = regress::Regex::new(r"\(\?P<\w+>").ok()?;
    while let Some(m) = named.find(&pattern) {
        pattern.replace_range(m.range(), "(");
    }
    pattern = replace_first(&pattern, r"\([^?][^)]*\)", "[0-9]+");
    pattern = pattern.replacen(r"\.stl$", "$", 1);
    pattern = replace_first(&pattern, r"\.stl$", "$");
    regress::Regex::with_flags(&pattern, "i").ok()
}
/// The file name without a `.stl` or `.3mf` extension, in any case.
fn model_stem(filename: &str) -> Option<&str> {
    let split = filename.len().checked_sub(4)?;
    let extension = filename.get(split..)?;
    (extension.eq_ignore_ascii_case(".stl") || extension.eq_ignore_ascii_case(".3mf"))
        .then(|| &filename[..split])
}
/// Removes every `marker` and one separator next to it, keeping one separator
/// between the words left on both sides. Matches Node's `parsePartSlug`.
fn strip_marker(stem: &str, marker: &str) -> String {
    let pattern = format!("([ _-]?){}([ _-]?)", escape_regex(marker));
    let Ok(regex) = regress::Regex::with_flags(&pattern, "i") else {
        return stem.to_owned();
    };
    let mut out = String::with_capacity(stem.len());
    let mut last = 0;
    for m in regex.find_iter(stem) {
        let range = m.range();
        out.push_str(&stem[last..range.start]);
        if range.start > 0 && range.end < stem.len() {
            let side = |group: usize| m.group(group).map(|r| &stem[r]).unwrap_or("");
            let before = side(1);
            out.push_str(if before.is_empty() { side(2) } else { before });
        }
        last = range.end;
    }
    out.push_str(&stem[last..]);
    out
}
fn parse_integer(text: &str) -> f64 {
    let text = trim(text);
    let mut end = 0;
    for (i, b) in text.bytes().enumerate() {
        if b.is_ascii_digit() || (i == 0 && matches!(b, b'+' | b'-')) {
            end = i + 1
        } else {
            break;
        }
    }
    text[..end].parse::<f64>().unwrap_or(f64::NAN)
}
pub(in crate::catalog) fn preview(
    tx: &Transaction<'_>,
    tenant: &str,
    relative_path: String,
    overrides: Option<NamingOverride>,
) -> Result<NamingPreview> {
    let global = global(tx, tenant)?;
    let profile = match overrides {
        Some(p) => merge(global, p)?,
        None => global,
    };
    let path = relative_path.replace('\\', "/");
    let lowercase_path = path.to_lowercase();
    let filename = path.rsplit('/').next().unwrap_or("").to_owned();
    let mut markers = profile
        .roles
        .iter()
        .flat_map(|r| {
            r.markers
                .iter()
                .filter(|m| !trim(m).is_empty())
                .map(move |m| (m, r.id))
        })
        .collect::<Vec<_>>();
    markers.sort_by_key(|(marker, _)| std::cmp::Reverse(marker.encode_utf16().count()));
    let markers = markers
        .into_iter()
        .map(|(marker, role)| (marker.to_lowercase(), role))
        .collect::<Vec<_>>();
    let role = profile
        .folder_rules
        .iter()
        .find(|r| lowercase_path.contains(&r.path_contains.to_lowercase()))
        .map(|r| r.role_id)
        .or_else(|| {
            lowercase_path.split('/').find_map(|segment| {
                markers
                    .iter()
                    .find(|(m, _)| segment.contains(m))
                    .map(|(_, role)| *role)
            })
        })
        .unwrap_or(RoleId::Primary);
    let quantity_re = regress::Regex::with_flags(&profile.quantity.regex, "i")
        .map_err(|_| CatalogFailure::Input("Invalid quantity regex".into()))?;
    // Quantity regexes end in `\.stl$`; a `.3mf` part is read as the `.stl` of the same name.
    let quantity_name = match model_stem(&filename) {
        Some(stem) => format!("{stem}.stl"),
        None => filename.clone(),
    };
    let captured = quantity_re
        .find(&quantity_name)
        .and_then(|m| m.captures.first().cloned().flatten())
        .map(|r| &quantity_name[r])
        .filter(|s| !s.is_empty());
    let quantity = captured
        .map(parse_integer)
        .unwrap_or(profile.quantity.default as f64);
    // JSON.stringify writes a whole number without a fraction (`2`, not `2.0`).
    let quantity = match quantity.max(1.0) {
        _ if quantity.is_nan() => None,
        whole if whole.fract() == 0.0 && whole < 9.0e18 => {
            Some(serde_json::Number::from(whole as i64))
        }
        other => serde_json::Number::from_f64(other),
    };
    let name = model_stem(&filename).unwrap_or(&filename);
    let mut slug = name.to_owned();
    if profile.slug.strip_markers {
        for marker in profile
            .roles
            .iter()
            .flat_map(|r| &r.markers)
            .filter(|m| !trim(m).is_empty())
        {
            slug = strip_marker(&slug, marker);
        }
    }
    if profile.slug.strip_quantity
        && let Some(regex) = quantity_strip(&profile.quantity.regex)
        && let Some(m) = regex.find(&slug)
    {
        slug.replace_range(m.range(), "");
    }
    if slug.is_empty() {
        slug = name.to_owned()
    }
    Ok(NamingPreview {
        role,
        quantity,
        part_slug: slug,
        filename,
    })
}

fn optional_non_null<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}
