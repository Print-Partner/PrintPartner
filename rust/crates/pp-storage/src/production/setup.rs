//! Node's Production setup (`production_setup:<Build id>`), parsed as Zod parses
//! `productionSetupSchema` and `productionSetupCommandSchema`
//! (`@print-partner/contracts`). A stored setup must hold the whole schema or it
//! is ignored; a command that fails answers Zod's issues.

use super::layout::{GroupField, GroupKind, GroupRule};
use super::model::{MAX_JS_SAFE_INTEGER, trim_ecmascript};
use serde_json::{Map, Value, json};

const FORMAT: &str = "production-setup-v1";
const ROUTES: [&str; 3] = ["plates", "stl", "external"];
const GROUP_FIELDS: [&str; 6] = [
    "material",
    "color",
    "source_directory",
    "source_layer",
    "role",
    "part",
];
const RULE_KINDS: [&str; 4] = [
    "separate_by",
    "keep_together",
    "assign_to_printer",
    "set_material",
];
const COMMANDS: [&str; 5] = [
    "set_preferred_slicer_instance",
    "set_selection",
    "replace_printer_assignments",
    "set_route",
    "replace_rules",
];

/// The issues of one Zod parse, `{ path, message }`, in the order Zod adds them.
#[derive(Default)]
struct Zod {
    path: Vec<Value>,
    issues: Vec<Value>,
}

impl Zod {
    fn fail<T>(&mut self, message: String) -> Option<T> {
        self.issues
            .push(json!({"path": self.path, "message": message}));
        None
    }

    fn at<T>(
        &mut self,
        key: impl Into<Value>,
        parse: impl FnOnce(&mut Self) -> Option<T>,
    ) -> Option<T> {
        self.path.push(key.into());
        let parsed = parse(self);
        self.path.pop();
        parsed
    }

    fn field<T>(
        &mut self,
        object: &Map<String, Value>,
        key: &str,
        parse: impl FnOnce(&mut Self, Option<&Value>) -> Option<T>,
    ) -> Option<T> {
        self.at(key, |zod| parse(zod, object.get(key)))
    }

    fn expected<T>(&mut self, kind: &str, value: Option<&Value>) -> Option<T> {
        let received = match value {
            None => "undefined",
            Some(Value::Null) => "null",
            Some(Value::Bool(_)) => "boolean",
            Some(Value::Number(_)) => "number",
            Some(Value::String(_)) => "string",
            Some(Value::Array(_)) => "array",
            Some(Value::Object(_)) => "object",
        };
        self.fail(format!(
            "Invalid input: expected {kind}, received {received}"
        ))
    }

    fn object<'a>(&mut self, value: Option<&'a Value>) -> Option<&'a Map<String, Value>> {
        match value {
            Some(Value::Object(object)) => Some(object),
            other => self.expected("object", other),
        }
    }

    /// `z.string().trim().min(1).max(max)`, counted in UTF-16 units.
    fn text(&mut self, value: Option<&Value>, max: usize) -> Option<Value> {
        let Some(Value::String(text)) = value else {
            return self.expected("string", value);
        };
        let text = trim_ecmascript(text);
        let units = text.encode_utf16().count();
        if units == 0 {
            return self.fail("Too small: expected string to have >=1 characters".into());
        }
        if units > max {
            return self.fail(format!(
                "Too big: expected string to have <={max} characters"
            ));
        }
        Some(text.into())
    }

    fn nullable_text(&mut self, value: Option<&Value>, max: usize) -> Option<Value> {
        match value {
            Some(Value::Null) => Some(Value::Null),
            other => self.text(other, max),
        }
    }

    fn boolean(&mut self, value: Option<&Value>) -> Option<Value> {
        match value {
            Some(Value::Bool(flag)) => Some((*flag).into()),
            other => self.expected("boolean", other),
        }
    }

    /// `z.enum(options)`.
    fn option(&mut self, value: Option<&Value>, options: &[&str]) -> Option<Value> {
        match value {
            Some(Value::String(text)) if options.contains(&text.as_str()) => {
                Some(text.as_str().into())
            }
            _ => {
                let quoted: Vec<String> = options
                    .iter()
                    .map(|option| format!("\"{option}\""))
                    .collect();
                self.fail(format!(
                    "Invalid option: expected one of {}",
                    quoted.join("|")
                ))
            }
        }
    }

    /// `z.array(item).max(max)`: a longer array fails before its items are read.
    fn array(
        &mut self,
        value: Option<&Value>,
        max: usize,
        mut item: impl FnMut(&mut Self, &Value) -> Option<Value>,
    ) -> Option<Value> {
        let Some(Value::Array(items)) = value else {
            return self.expected("array", value);
        };
        if items.len() > max {
            return self.fail(format!("Too big: expected array to have <={max} items"));
        }
        let parsed: Vec<Option<Value>> = items
            .iter()
            .enumerate()
            .map(|(index, value)| self.at(index, |zod| item(zod, value)))
            .collect();
        parsed
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .map(Value::Array)
    }

    /// The tag of a `z.discriminatedUnion(key, ...)` value.
    fn tag<'a>(
        &mut self,
        object: &'a Map<String, Value>,
        key: &str,
        options: &[&str],
    ) -> Option<&'a str> {
        match object.get(key) {
            Some(Value::String(tag)) if options.contains(&tag.as_str()) => Some(tag),
            _ => {
                let quoted: Vec<String> =
                    options.iter().map(|option| format!("'{option}'")).collect();
                self.at(key, |zod| {
                    zod.fail(format!(
                        "Invalid discriminator value. Expected {}",
                        quoted.join(" | ")
                    ))
                })
            }
        }
    }

    /// `.strict()`: one issue naming every key the shape does not list.
    fn strict(&mut self, object: &Map<String, Value>, shape: &[&str]) {
        let unknown: Vec<String> = object
            .keys()
            .filter(|key| !shape.contains(&key.as_str()))
            .map(|key| format!("\"{key}\""))
            .collect();
        match unknown.len() {
            0 => {}
            1 => {
                self.fail::<()>(format!("Unrecognized key: {}", unknown[0]));
            }
            _ => {
                self.fail::<()>(format!("Unrecognized keys: {}", unknown.join(", ")));
            }
        }
    }

    fn selection(&mut self, value: Option<&Value>) -> Option<Value> {
        let object = self.object(value)?;
        match self.tag(object, "mode", &["all_incomplete", "custom"])? {
            "custom" => {
                let tokens = self.field(object, "selected_unit_tokens", |zod, value| {
                    zod.array(value, 20_000, |zod, token| zod.text(Some(token), 500))
                })?;
                Some(json!({"mode": "custom", "selected_unit_tokens": tokens}))
            }
            _ => Some(json!({"mode": "all_incomplete"})),
        }
    }

    fn assignments(&mut self, value: Option<&Value>) -> Option<Value> {
        self.array(value, 20_000, |zod, assignment| {
            let object = zod.object(Some(assignment))?;
            let token = zod.field(object, "token", |zod, value| zod.text(value, 500));
            let printer = zod.field(object, "printer_id", |zod, value| zod.text(value, 200));
            Some(json!({"token": token?, "printer_id": printer?}))
        })
    }

    fn rules(&mut self, value: Option<&Value>) -> Option<Value> {
        self.array(value, 200, |zod, rule| {
            let object = zod.object(Some(rule))?;
            let kind = zod.tag(object, "kind", &RULE_KINDS)?;
            let id = zod.field(object, "id", |zod, value| zod.text(value, 100));
            let enabled = zod.field(object, "enabled", Self::boolean);
            let fields: &[&str] = if kind == "set_material" {
                &GROUP_FIELDS[1..]
            } else {
                &GROUP_FIELDS
            };
            let field = zod.field(object, "field", |zod, value| zod.option(value, fields));
            let extra: &[(&str, usize)] = match kind {
                "keep_together" => &[("value", 500)],
                "assign_to_printer" => &[("value", 500), ("printer_id", 200)],
                "set_material" => &[("value", 500), ("material_type", 100)],
                _ => &[],
            };
            let extra: Vec<(&str, Option<Value>)> = extra
                .iter()
                .map(|(key, max)| {
                    (
                        *key,
                        zod.field(object, key, |zod, value| zod.text(value, *max)),
                    )
                })
                .collect();
            let mut parsed = Map::new();
            parsed.insert("id".into(), id?);
            parsed.insert("enabled".into(), enabled?);
            parsed.insert("kind".into(), kind.into());
            parsed.insert("field".into(), field?);
            for (key, value) in extra {
                parsed.insert(key.into(), value?);
            }
            Some(Value::Object(parsed))
        })
    }
}

/// Node's `defaultProductionSetup`.
pub fn default_setup(profile: i64) -> Value {
    json!({
        "format": FORMAT,
        "profile_id": profile,
        "preferred_slicer_instance_id": null,
        "selection": {"mode": "all_incomplete"},
        "printer_assignments": [],
        "route": null,
        "rules": [],
        "updated_at": null,
    })
}

/// Node's `parseStoredProductionSetup`: the stored setup as Zod outputs it, or
/// none when it is absent, is not JSON, fails the schema or names another Build.
pub fn stored_setup(raw: Option<&str>, profile: i64) -> Option<Value> {
    let value: Value = serde_json::from_str(raw.filter(|raw| !raw.is_empty())?).ok()?;
    let object = value.as_object()?;
    let mut zod = Zod::default();
    let preferred = zod.field(object, "preferred_slicer_instance_id", |zod, value| {
        zod.nullable_text(value, 200)
    });
    let selection = zod.field(object, "selection", Zod::selection);
    let assignments = match object.get("printer_assignments") {
        None => Some(json!([])),
        Some(value) => zod.assignments(Some(value)),
    };
    let route = match object.get("route") {
        None | Some(Value::Null) => Some(Value::Null),
        Some(value) => zod.option(Some(value), &ROUTES),
    };
    let rules = zod.field(object, "rules", Zod::rules);
    let id = object.get("profile_id")?.as_f64()?;
    let updated_at = match object.get("updated_at")? {
        Value::Null => Value::Null,
        Value::String(at) if datetime(at) => at.as_str().into(),
        _ => return None,
    };
    if object.get("format")?.as_str()? != FORMAT
        || id.fract() != 0.0
        || !(1.0..=MAX_JS_SAFE_INTEGER as f64).contains(&id)
        || id as i64 != profile
    {
        return None;
    }
    Some(json!({
        "preferred_slicer_instance_id": preferred?,
        "selection": selection?,
        "printer_assignments": assignments?,
        "route": route?,
        "rules": rules?,
        "format": FORMAT,
        "profile_id": profile,
        "updated_at": updated_at,
    }))
}

/// A `productionSetupCommandSchema` command: the setup field it replaces and its
/// parsed value.
#[derive(Debug, PartialEq)]
pub struct SetupCommand {
    pub field: &'static str,
    pub value: Value,
}

/// The command a PATCH body names, or Zod's issues.
pub fn setup_command(body: &Value) -> Result<SetupCommand, Vec<Value>> {
    let mut zod = Zod::default();
    let parsed = (|| {
        let object = zod.object(Some(body))?;
        let field = match zod.tag(object, "kind", &COMMANDS)? {
            "set_preferred_slicer_instance" => "preferred_slicer_instance_id",
            "set_selection" => "selection",
            "replace_printer_assignments" => "printer_assignments",
            "set_route" => "route",
            _ => "rules",
        };
        let value = zod.field(object, field, |zod, value| match field {
            "preferred_slicer_instance_id" => zod.nullable_text(value, 200),
            "selection" => zod.selection(value),
            "printer_assignments" => zod.assignments(value),
            "route" if value == Some(&Value::Null) => Some(Value::Null),
            "route" => zod.option(value, &ROUTES),
            _ => zod.rules(value),
        });
        zod.strict(object, &["kind", field]);
        Some(SetupCommand {
            field,
            value: value?,
        })
    })();
    match parsed {
        Some(command) if zod.issues.is_empty() => Ok(command),
        _ => Err(zod.issues),
    }
}

/// `updateProductionSetup`: `setup` with the command applied, stamped for
/// Build `profile` at `updated_at`, in the schema's key order.
pub fn apply(setup: &Value, command: SetupCommand, profile: i64, updated_at: &str) -> Value {
    let mut next = Map::new();
    for key in [
        "preferred_slicer_instance_id",
        "selection",
        "printer_assignments",
        "route",
        "rules",
    ] {
        let value = if key == command.field {
            command.value.clone()
        } else {
            setup[key].clone()
        };
        next.insert(key.into(), value);
    }
    next.insert("format".into(), FORMAT.into());
    next.insert("profile_id".into(), profile.into());
    next.insert("updated_at".into(), updated_at.into());
    Value::Object(next)
}

/// The grouping rules of the stored setup for Build `profile`, or none when it
/// is ignored. `assign_to_printer` rules are left out: packing never reads them.
pub fn stored_rules(raw: Option<&str>, profile: i64) -> Vec<GroupRule> {
    let Some(setup) = stored_setup(raw, profile) else {
        return Vec::new();
    };
    setup["rules"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(group_rule)
        .collect()
}

/// One parsed rule as packing reads it.
fn group_rule(rule: &Value) -> Option<GroupRule> {
    let text = |key: &str| rule[key].as_str().map(str::to_owned);
    let kind = match rule["kind"].as_str()? {
        "separate_by" => GroupKind::SeparateBy,
        "keep_together" => GroupKind::KeepTogether,
        "set_material" => GroupKind::SetMaterial,
        _ => return None,
    };
    let field = match rule["field"].as_str()? {
        "material" => GroupField::Material,
        "color" => GroupField::Color,
        "source_directory" => GroupField::SourceDirectory,
        "source_layer" => GroupField::SourceLayer,
        "role" => GroupField::Role,
        _ => GroupField::ObjectName,
    };
    Some(GroupRule {
        id: text("id")?,
        enabled: rule["enabled"].as_bool()?,
        kind,
        field,
        value: text("value"),
        material_type: text("material_type"),
    })
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
