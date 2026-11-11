use crate::read_model::views::js_trim;
use serde_json::{Value, json};
fn string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
        Value::Number(n) => ryu_js::Buffer::new()
            .format(n.as_f64().unwrap_or_default())
            .to_owned(),
        _ => value.to_string(),
    }
}
fn text(row: &Value, key: &str) -> String {
    js_trim(&string(&row[key])).into()
}
fn status(row: &Value) -> String {
    row.get("status")
        .filter(|v| !v.is_null())
        .map(string)
        .unwrap_or_else(|| "unverified".into())
}
fn valid_status(s: &str) -> bool {
    ["unverified", "satisfied", "incompatible", "user_waived"].contains(&s)
}
pub(super) fn update(brief: &mut Value, params: &Value) -> Result<Value, &'static str> {
    let rows = |key: &str| {
        params[key]
            .as_array()
            .or_else(|| brief[key].as_array())
            .cloned()
            .unwrap_or_default()
    };
    let requirements = requirements(rows("requirements"))?;
    let evidence = brief["evidence"].as_array().cloned().unwrap_or_default();
    let contributions = contributions(rows("contributions"), &evidence)?;
    let findings = findings(rows("compatibility_findings"), &evidence)?;
    let result = json!({"requirement_count":requirements.len(),"contribution_count":contributions.len(),"compatibility_finding_count":findings.len()});
    brief["requirements"] = json!(requirements);
    brief["contributions"] = json!(contributions);
    brief["compatibility_findings"] = json!(findings);
    if params.get("contributions").is_some()
        && let Some(fields) = brief.as_object_mut()
    {
        fields.remove("draft_id");
    }
    Ok(result)
}
fn requirements(rows: Vec<Value>) -> Result<Vec<Value>, &'static str> {
    let mut requirements = Vec::new();
    for row in rows {
        if !row.is_object() && !row.is_array() {
            return Err("Invalid requirement");
        }
        let (key, value, status) = (text(&row, "key"), text(&row, "value"), status(&row));
        if key.is_empty() || value.is_empty() || !valid_status(&status) {
            return Err("Invalid requirement");
        }
        let mut item = json!({"key":key,"value":value,"status":status});
        if row["detail"].is_string() {
            item["detail"] = row["detail"].clone();
        }
        requirements.push(item);
    }
    Ok(requirements)
}
fn contributions(rows: Vec<Value>, evidence: &[Value]) -> Result<Vec<Value>, &'static str> {
    let known = |id: &Value| evidence.iter().any(|e| &e["id"] == id);
    let mut contributions = Vec::new();
    for row in rows {
        if !row.is_object() && !row.is_array() {
            return Err("Invalid Source contribution");
        }
        let (id, evidence_id, slot) = (
            text(&row, "id"),
            text(&row, "evidence_id"),
            text(&row, "slot"),
        );
        if id.is_empty()
            || !known(&json!(evidence_id))
            || !slot.starts_with(|c: char| c.is_ascii_lowercase())
            || !slot
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            return Err("Source contribution identity, evidence, or slot is invalid");
        }
        let responsibility = string(&row["responsibility"]);
        if ![
            "printable_parts",
            "hardware_constraint",
            "informational_evidence",
        ]
        .contains(&responsibility.as_str())
        {
            return Err("Source contribution responsibility is invalid");
        }
        let confidence = string(&row["confidence"]);
        let status = row
            .get("status")
            .filter(|v| !v.is_null())
            .map(string)
            .unwrap_or_else(|| "confirmed".into());
        let evidence_text = text(&row, "evidence_text");
        if !["low", "medium", "high"].contains(&confidence.as_str())
            || !["proposed", "confirmed", "rejected"].contains(&status.as_str())
            || evidence_text.is_empty()
        {
            return Err("Source contribution confidence, status, or evidence is invalid");
        }
        let path_scopes: Vec<_> = row["path_scopes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(js_trim)
            .filter(|s| !s.is_empty())
            .collect();
        contributions.push(json!({"id":id,"evidence_id":evidence_id,"slot":slot,"responsibility":responsibility,"path_scopes":path_scopes,"confidence":confidence,"evidence_text":evidence_text,"status":status}));
    }
    Ok(contributions)
}
fn findings(rows: Vec<Value>, evidence: &[Value]) -> Result<Vec<Value>, &'static str> {
    let known = |id: &Value| evidence.iter().any(|e| &e["id"] == id);
    let mut findings = Vec::new();
    for row in rows {
        if !row.is_object() && !row.is_array() {
            return Err("Invalid compatibility finding");
        }
        let (id, subject, detail, status) = (
            text(&row, "id"),
            text(&row, "subject"),
            text(&row, "detail"),
            status(&row),
        );
        if id.is_empty() || subject.is_empty() || detail.is_empty() || !valid_status(&status) {
            return Err("Invalid compatibility finding");
        }
        let ids: Vec<_> = row["evidence_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|v| v.is_string())
            .cloned()
            .collect();
        if !ids.iter().all(known) {
            return Err("Compatibility finding references unknown evidence");
        }
        findings.push(
            json!({"id":id,"subject":subject,"detail":detail,"status":status,"evidence_ids":ids}),
        );
    }
    Ok(findings)
}
