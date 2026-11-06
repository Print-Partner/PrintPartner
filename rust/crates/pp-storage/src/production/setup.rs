//! Node's stored Production setup (`production_setup:<Build id>`), read as
//! `parseStoredProductionSetup` does: the whole `productionSetupSchema`
//! (`@print-partner/contracts`) must hold, or the setup is ignored.

use super::layout::{GroupField, GroupKind, GroupRule};
use super::model::{MAX_JS_SAFE_INTEGER, trim_ecmascript};
use serde_json::{Map, Value};

/// The grouping rules of the stored setup for Build `profile`, or none when
/// it is absent, is not JSON, fails the schema or names another Build.
/// `assign_to_printer` rules are validated and left out: packing never reads
/// them.
pub fn stored_rules(raw: Option<&str>, profile: i64) -> Vec<GroupRule> {
    raw.filter(|raw| !raw.is_empty())
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|value| rules(value.as_object()?, profile))
        .unwrap_or_default()
}

/// Zod's `z.string().trim().min(1).max(max)`: the trimmed text, counted in
/// UTF-16 units as JavaScript's `length` does.
fn text(value: Option<&Value>, max: usize) -> Option<String> {
    let text = trim_ecmascript(value?.as_str()?);
    (1..=max)
        .contains(&text.encode_utf16().count())
        .then(|| text.to_owned())
}

fn nullable_text(value: Option<&Value>, max: usize) -> Option<Option<String>> {
    match value? {
        Value::Null => Some(None),
        other => text(Some(other), max).map(Some),
    }
}

fn array(value: Option<&Value>, max: usize) -> Option<&Vec<Value>> {
    value?.as_array().filter(|items| items.len() <= max)
}

fn rules(setup: &Map<String, Value>, profile: i64) -> Option<Vec<GroupRule>> {
    let id = setup.get("profile_id")?.as_f64()?;
    if setup.get("format")?.as_str()? != "production-setup-v1"
        || id.fract() != 0.0
        || !(1.0..=MAX_JS_SAFE_INTEGER as f64).contains(&id)
        || id as i64 != profile
    {
        return None;
    }
    nullable_text(setup.get("preferred_slicer_instance_id"), 200)?;
    match setup.get("updated_at")? {
        Value::Null => {}
        Value::String(at) if datetime(at) => {}
        _ => return None,
    }
    let selection = setup.get("selection")?.as_object()?;
    match selection.get("mode")?.as_str()? {
        "all_incomplete" => {}
        "custom" => {
            for token in array(selection.get("selected_unit_tokens"), 20_000)? {
                text(Some(token), 500)?;
            }
        }
        _ => return None,
    }
    if let Some(assignments) = setup.get("printer_assignments") {
        for assignment in array(Some(assignments), 20_000)? {
            text(assignment.get("token"), 500)?;
            text(assignment.get("printer_id"), 200)?;
        }
    }
    match setup.get("route") {
        None | Some(Value::Null) => {}
        Some(Value::String(route)) if ["plates", "stl", "external"].contains(&route.as_str()) => {}
        _ => return None,
    }
    let rules = array(setup.get("rules"), 200)?
        .iter()
        .map(rule)
        .collect::<Option<Vec<_>>>()?;
    Some(rules.into_iter().flatten().collect())
}

/// One `productionGroupingRuleSchema` rule; `Some(None)` for a valid
/// `assign_to_printer` rule.
fn rule(value: &Value) -> Option<Option<GroupRule>> {
    let id = text(value.get("id"), 100)?;
    let enabled = value.get("enabled")?.as_bool()?;
    let kind = value.get("kind")?.as_str()?;
    let field = match value.get("field")?.as_str()? {
        "material" if kind != "set_material" => GroupField::Material,
        "color" => GroupField::Color,
        "source_directory" => GroupField::SourceDirectory,
        "source_layer" => GroupField::SourceLayer,
        "role" => GroupField::Role,
        "part" => GroupField::ObjectName,
        _ => return None,
    };
    let value_text = || text(value.get("value"), 500);
    let (kind, rule_value, material_type) = match kind {
        "separate_by" => (GroupKind::SeparateBy, None, None),
        "keep_together" => (GroupKind::KeepTogether, Some(value_text()?), None),
        "assign_to_printer" => {
            value_text()?;
            text(value.get("printer_id"), 200)?;
            return Some(None);
        }
        "set_material" => (
            GroupKind::SetMaterial,
            Some(value_text()?),
            Some(text(value.get("material_type"), 100)?),
        ),
        _ => return None,
    };
    Some(Some(GroupRule {
        id,
        enabled,
        kind,
        field,
        value: rule_value,
        material_type,
    }))
}

/// Zod 4's `z.string().datetime()`: `YYYY-MM-DDTHH:MM:SS`, optional
/// fractional seconds, `Z`, and a real calendar date.
fn datetime(text: &str) -> bool {
    let bytes = text.as_bytes();
    let digits = |range: std::ops::Range<usize>| {
        bytes
            .get(range.clone())
            .is_some_and(|part| part.iter().all(u8::is_ascii_digit))
            .then(|| text[range].parse::<u32>().ok())
            .flatten()
    };
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        digits(0..4),
        digits(5..7),
        digits(8..10),
        digits(11..13),
        digits(14..16),
        digits(17..19),
    ) else {
        return false;
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    let fraction = &text[19.min(text.len())..];
    let fraction = fraction.strip_suffix('Z').unwrap_or("x");
    [bytes.get(4), bytes.get(7), bytes.get(13), bytes.get(16)]
        == [Some(&b'-'), Some(&b'-'), Some(&b':'), Some(&b':')]
        && bytes.get(10) == Some(&b'T')
        && (1..=days).contains(&day)
        && hour < 24
        && minute < 60
        && second < 60
        && (fraction.is_empty()
            || fraction.strip_prefix('.').is_some_and(|digits| {
                !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
            }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datetime_matches_zod() {
        for (text, valid) in [
            ("2026-01-01T00:00:00Z", true),
            ("2024-02-29T00:00:00Z", true),
            ("2000-02-29T00:00:00Z", true),
            ("0000-01-01T00:00:00Z", true),
            ("2026-01-01T00:00:00.1234567890123Z", true),
            ("2023-02-29T00:00:00Z", false),
            ("1900-02-29T00:00:00Z", false),
            ("2026-02-30T00:00:00Z", false),
            ("2026-01-01T24:00:00Z", false),
            ("2026-01-01T00:00:60Z", false),
            ("2026-01-01T00:00Z", false),
            ("2026-01-01t00:00:00z", false),
            ("2026-01-01T00:00:00.Z", false),
            ("2026-01-01T00:00:00+00:00", false),
            (" 2026-01-01T00:00:00Z", false),
        ] {
            assert_eq!(datetime(text), valid, "{text}");
        }
    }

    #[test]
    fn stored_rules_need_the_whole_setup_for_the_same_build() {
        let setup = serde_json::json!({
            "format": "production-setup-v1",
            "profile_id": 1,
            "preferred_slicer_instance_id": null,
            "selection": {"mode": "all_incomplete"},
            "rules": [
                {"id": " r1 ", "enabled": true, "kind": "separate_by", "field": "color"},
                {"id": "r2", "enabled": false, "kind": "assign_to_printer", "field": "role", "value": "x", "printer_id": "p"},
                {"id": "r3", "enabled": true, "kind": "set_material", "field": "part", "value": " a ", "material_type": "PETG"},
            ],
            "updated_at": "2026-01-01T00:00:00.000Z",
        });
        let rules = stored_rules(Some(&setup.to_string()), 1);
        assert_eq!(
            rules,
            vec![
                GroupRule {
                    id: "r1".into(),
                    enabled: true,
                    kind: GroupKind::SeparateBy,
                    field: GroupField::Color,
                    value: None,
                    material_type: None,
                },
                GroupRule {
                    id: "r3".into(),
                    enabled: true,
                    kind: GroupKind::SetMaterial,
                    field: GroupField::ObjectName,
                    value: Some("a".into()),
                    material_type: Some("PETG".into()),
                },
            ]
        );
        assert!(
            stored_rules(Some(&setup.to_string()), 2).is_empty(),
            "another Build"
        );
        for (key, bad) in [
            ("updated_at", serde_json::json!("2026-01-01")),
            ("route", serde_json::json!("plate")),
            ("printer_assignments", Value::Null),
            ("selection", serde_json::json!({"mode": "custom"})),
            (
                "rules",
                serde_json::json!([{"id": "x", "enabled": true, "kind": "set_material", "field": "material", "value": "a", "material_type": "b"}]),
            ),
        ] {
            let mut broken = setup.clone();
            broken[key] = bad;
            assert!(
                stored_rules(Some(&broken.to_string()), 1).is_empty(),
                "{key}"
            );
        }
        let mut missing = setup.clone();
        missing
            .as_object_mut()
            .unwrap()
            .remove("preferred_slicer_instance_id");
        assert!(stored_rules(Some(&missing.to_string()), 1).is_empty());
        for (id, kept) in [
            ("\u{1F600}".repeat(50), true),
            ("\u{1F600}".repeat(51), false),
        ] {
            let mut astral = setup.clone();
            astral["rules"][0]["id"] = Value::String(id);
            assert_eq!(
                stored_rules(Some(&astral.to_string()), 1).is_empty(),
                !kept,
                "a rule id is at most 100 UTF-16 units"
            );
        }
        assert!(stored_rules(Some("not json"), 1).is_empty());
        assert!(stored_rules(None, 1).is_empty());
    }
}
