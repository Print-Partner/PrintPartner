//! Node's addon replacement warnings over captured domain-pack documents.
use serde_json::{Value, json};
fn trim(s: &str) -> &str {
    s.trim_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}')
}
fn list(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| {
            if v.is_string() {
                Some(super::js_string(v))
            } else {
                ["base", "source", "name"]
                    .iter()
                    .find_map(|k| v.get(*k).filter(|v| !v.is_null()).map(super::js_string))
            }
        })
        .map(|s| trim(&s).to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}
fn prose_parts(line: &str) -> Vec<(String, Option<String>)> {
    let line = trim(line);
    let Ok(paths) = regress::Regex::with_flags(r"[\w./-]+\.stl", "i") else {
        return Vec::new();
    };
    let first = |s: &str| {
        paths
            .find(s)
            .map(|m| s[m.range()].to_owned())
            .unwrap_or_else(|| s.to_owned())
    };
    let arrow = [
        line.find("->").map(|i| (i, 2)),
        line.find('→').map(|i| (i, 3)),
    ]
    .into_iter()
    .flatten()
    .filter(|(i, n)| *i > 0 && *i + *n < line.len())
    .min_by_key(|(i, _)| *i);
    if let Some((i, n)) = arrow {
        let (from, to) = (&line[..i], &line[i + n..]);
        return vec![(first(trim(from)), Some(first(trim(to))))];
    }
    paths
        .find_iter(line)
        .take(3)
        .map(|m| (line[m.range()].to_owned(), None))
        .collect()
}
fn parts(row: &Value) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    for part in row["replaces_parts"].as_array().into_iter().flatten() {
        if let Some(line) = part.as_str() {
            out.extend(prose_parts(line));
            continue;
        }
        let from = part
            .get("from_slug_or_path")
            .filter(|v| !v.is_null())
            .or_else(|| part.get("from"))
            .filter(|v| !v.is_null())
            .map(super::js_string)
            .unwrap_or_default();
        if trim(&from).is_empty() {
            continue;
        }
        let to = part
            .get("to_slug_or_path")
            .filter(|v| !v.is_null())
            .or_else(|| part.get("to"))
            .filter(|v| !v.is_null())
            .map(super::js_string);
        out.push((trim(&from).into(), to));
    }
    if out.is_empty() {
        for line in list(&row["replaces"]) {
            out.extend(prose_parts(&line));
        }
    }
    out
}
fn unique(items: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out = Vec::new();
    for item in items {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}
struct Source {
    name: String,
    conflicts: Vec<String>,
    slots: Vec<String>,
    not_for: Vec<String>,
    parts: Vec<(String, Option<String>)>,
}
fn source_name(row: &Value) -> Option<&str> {
    row["source_name"]
        .as_str()
        .or_else(|| row["name"].as_str())
        .map(trim)
}
fn explain(name: &str, docs: &Value) -> Option<Source> {
    let rows = docs["sources"].as_array();
    let row = rows
        .into_iter()
        .flatten()
        .find(|r| source_name(r) == Some(name))
        .or_else(|| {
            rows.into_iter()
                .flatten()
                .find(|r| source_name(r).is_some_and(|s| s.to_lowercase() == name.to_lowercase()))
        });
    let resolved = row.and_then(source_name).unwrap_or(name);
    let mut conflicts = Vec::new();
    let mut slots = Vec::new();
    let category = docs["catalog"]["addon_categories"]
        .as_object()
        .and_then(|cats| {
            cats.iter()
                .filter(|(_, c)| c["rule"] == "pick_one")
                .rfind(|(_, c)| {
                    c["sources"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|s| s["name"] == resolved)
                })
        });
    if row.is_none() && category.is_none() {
        return None;
    }
    let empty = Value::Null;
    let row = row.unwrap_or(&empty);
    conflicts.extend(list(&row["conflicts_with"]));
    conflicts.extend(list(&row["conflicts"]));
    slots.extend(list(
        row.get("replaces_slots")
            .filter(|v| !v.is_null())
            .unwrap_or(&row["slots"]),
    ));
    if slots.is_empty() {
        let kind = row["kind"].as_str().unwrap_or_default().to_lowercase();
        for (needle, slot) in [
            ("probe", "probe"),
            ("toolhead", "toolhead"),
            ("extruder", "extruder"),
            ("electronics", "controller"),
            ("controller", "controller"),
        ] {
            if kind.contains(needle) {
                slots.push(slot.into());
                break;
            }
        }
    }
    if slots.is_empty() {
        let prose = list(&row["replaces"]).join(" ").to_lowercase();
        if prose.contains("probe")
            || prose.contains("zendstop")
            || prose.contains("z_endstop")
            || prose.contains("z-endstop")
            || prose.contains("z endstop")
        {
            slots.push("probe".into());
        }
        if prose.contains("inlet") {
            slots.push("power_inlet".into());
        }
    }
    if let Some((id, cat)) = category {
        conflicts.extend(
            cat["sources"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s["name"].as_str())
                .filter(|s| *s != resolved)
                .map(str::to_owned),
        );
        slots.push(
            docs["catalog"]["addon_categories"][id]["replaces_slot"]
                .as_str()
                .unwrap_or(id)
                .into(),
        );
    }
    let parts = parts(row);
    Some(Source {
        name: resolved.into(),
        conflicts: unique(conflicts),
        slots: unique(slots),
        not_for: list(&row["not_for"]),
        parts,
    })
}
pub fn check(name: &str, layers: &[String], docs: &Value) -> Value {
    let mut warnings = Vec::new();
    let mut excludes = Vec::new();
    let mut conflicts = Vec::new();
    if let Some(addon) = explain(name, docs) {
        let layers = unique(
            layers
                .iter()
                .map(|s| trim(s).to_owned())
                .filter(|s| !s.is_empty()),
        );
        for peer in &addon.conflicts {
            if layers
                .iter()
                .any(|l| l.to_lowercase() == peer.to_lowercase())
            {
                warnings.push(json!({"severity":"warning","code":"compat_conflict","message":format!("\"{}\" conflicts with \"{peer}\" (pick one).",addon.name),"sources":[addon.name,peer]}));
                conflicts.push(json!({"a":addon.name,"b":peer,"reason":"conflicts_with"}));
            }
        }
        for slot in &addon.slots {
            let holders: Vec<_> = layers
                .iter()
                .filter_map(|l| explain(l, docs))
                .filter(|s| {
                    s.slots.contains(slot) && s.name.to_lowercase() != addon.name.to_lowercase()
                })
                .map(|s| s.name)
                .collect();
            if !holders.is_empty() {
                let mut sources = vec![addon.name.clone()];
                sources.extend(holders.clone());
                warnings.push(json!({"severity":"warning","code":"compat_slot","message":format!("Slot \"{slot}\" already occupied by {}; adding {} may require removing them.",holders.join(", "),addon.name),"sources":sources,"slot":slot}));
                for holder in holders {
                    conflicts
                        .push(json!({"a":addon.name,"b":holder,"reason":format!("slot:{slot}")}));
                }
            }
        }
        for base in &addon.not_for {
            if let Some(hit) = layers.iter().find(|l| {
                l.to_lowercase() == base.to_lowercase()
                    || base.to_lowercase().contains(&l.to_lowercase())
            }) {
                warnings.push(json!({"severity":"warning","code":"compat_not_for","message":format!("\"{}\" is marked not_for {base} (plan has {hit}).",addon.name),"sources":[addon.name,hit]}));
            }
        }
        for (from, to) in addon.parts {
            let clean = from.replace('\\', "/");
            let hint = clean.rsplit('/').next().unwrap_or_default().to_lowercase();
            if !hint.is_empty() && !excludes.contains(&hint) {
                excludes.push(hint);
            }
            let message = if let Some(to) = to.filter(|s| !s.is_empty()) {
                format!("{} suggests replacing {from} → {to}", addon.name)
            } else {
                format!("{} suggests excluding/replacing {from}", addon.name)
            };
            warnings.push(json!({"severity":"info","code":"compat_replace_part","message":message,"sources":[addon.name]}));
        }
        for row in docs["merge_conflicts"]["conflicts"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let id = row["slug_or_path"]
                .as_str()
                .or_else(|| row["id"].as_str())
                .or_else(|| row["paths"][0].as_str())
                .unwrap_or("?");
            let sources = list(row.get("sources").unwrap_or(&row["stacks"]));
            let resolution = row["resolution"].as_str().unwrap_or_default();
            let hay = format!("{id} {} {resolution}", sources.join(" ")).to_lowercase();
            if hay.contains(&addon.name.to_lowercase()) && !layers.is_empty() {
                warnings.push(json!({"severity":"info","code":"merge_conflict_curated","message":format!("{id}: {}",resolution.chars().take(160).collect::<String>()),"sources":[addon.name],"conflict_id":id}));
            }
        }
    }
    json!({"warnings":warnings,"suggested_excludes":excludes,"conflicts":conflicts})
}
#[test]
fn addon_compatibility_preserves_node_aliases_and_category_order() {
    let docs = json!({"sources":[{"source_name":" A ","replaces_slots":null,"slots":["explicit"],"replaces_parts":[{"from":null}]},{"source_name":"Z","replaces":["zendstop"]}],"catalog":{"addon_categories":{"first":{"rule":"pick_one","sources":[{"name":"A"},{"name":"Early"}]},"toolhead":{"rule":"pick_one","replaces_slot":"custom_head","sources":[{"name":"A"},{"name":"Late"}]}}},"default_catalog":{"addon_categories":{"toolhead":{"replaces_slot":"shipped_head"}}}});
    let source = explain("A", &docs).unwrap();
    assert_eq!(source.name, "A");
    assert_eq!(source.slots, ["explicit", "custom_head"]);
    assert_eq!(source.conflicts, ["Late"]);
    assert!(source.parts.is_empty());
    assert_eq!(explain("Z", &docs).unwrap().slots, ["probe"]);
    assert_eq!(
        prose_parts("A → B -> C"),
        vec![("A".into(), Some("B -> C".into()))]
    );
    assert_eq!(
        prose_parts("A -> B → C"),
        vec![("A".into(), Some("B → C".into()))]
    );
}
