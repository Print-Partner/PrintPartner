//! Node's `POST /plans/{id}/decisions` (GRE-1243): `appendPlanDecision`
//! through `repo.createPlanDecision`, an insert into `plan_decisions` that
//! never touches publication or any other table.
//!
//! Node reads the body without a schema. `kind` defaults to `user_note` and
//! must be one of four kinds; `actor` is `assistant` or else `user`;
//! `params` and `result` are stored as `JSON.stringify` writes them. The
//! other four fields reach the SQLite driver as they are, so this module binds
//! them the way better-sqlite3 binds Drizzle's eleven insert parameters: a
//! number is a double that the TEXT column turns into text, an array is spread
//! into the parameter list, an object is a named-parameter bag that binds
//! nothing, and anything else is refused with the driver's message.

use super::BuildOutcome;
use crate::manifest_text::JsText;
use crate::source_scan::node_json::{js_members, js_string, reserialize};
use anyhow::Result;
use pp_domain::PositiveId;
use rusqlite::{ErrorCode, Transaction, params_from_iter, types::Value};
use serde::Serialize;
use serde_json::value::RawValue;

#[derive(Debug)]
pub enum DecisionOutcome {
    /// The created decision as Node's `mapPlanDecision` returns it.
    Created(Vec<u8>),
    /// Node's 400 `detail`.
    Refused(String),
    /// The body is not JSON.
    Unreadable,
}

const KINDS: [&str; 4] = ["applied_action", "dismissed_action", "user_note", "choice"];
const UNBINDABLE: &str = "SQLite3 can only bind numbers, strings, bigints, buffers, and null";
const PARAMETERS: usize = 11;

/// Node's `PlanDecision`, with `params` and `result` as JSON text.
#[derive(Serialize)]
struct Created {
    id: i64,
    plan_id: i64,
    created_at: String,
    actor: String,
    kind: String,
    action_type: Option<String>,
    params: Box<RawValue>,
    label: String,
    summary: String,
    rationale: Option<String>,
    result: Option<Box<RawValue>>,
}

/// `parseJsonObject`: the stored text when it holds a JSON object, else `{}`,
/// as `JSON.stringify` writes it back.
fn object(stored: &str) -> rusqlite::Result<Box<RawValue>> {
    let text = reserialize(stored)
        .filter(|text| text.starts_with('{'))
        .unwrap_or_else(|| "{}".into());
    RawValue::from_string(text)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
}

/// One body field's value as the driver receives it.
enum Field<'a> {
    Bound(Value),
    Spread(Vec<&'a RawValue>),
    Named,
    Unbindable,
}

fn field(raw: &RawValue) -> Field<'_> {
    let text = raw.get();
    match text.as_bytes().first() {
        Some(b'n') => Field::Bound(Value::Null),
        Some(b'"') => js_string(text).map_or(Field::Unbindable, |text| {
            Field::Bound(Value::Text(String::from_utf16_lossy(text.units())))
        }),
        Some(b'-' | b'0'..=b'9') => text.parse().map_or(Field::Unbindable, |number| {
            Field::Bound(Value::Real(number))
        }),
        Some(b'[') => serde_json::from_str(text).map_or(Field::Unbindable, Field::Spread),
        Some(b'{') => Field::Named,
        _ => Field::Unbindable,
    }
}

pub(super) fn append(
    tx: &Transaction<'_>,
    tenant: &str,
    build: PositiveId,
    body: &[u8],
) -> Result<BuildOutcome> {
    let build = build.get() as i64;
    if !super::build_exists(tx, tenant, build)? {
        return Ok(BuildOutcome::MissingBuild);
    }
    let refused = |detail: &str| {
        Ok(BuildOutcome::Decision(DecisionOutcome::Refused(
            detail.into(),
        )))
    };
    let members = if body.is_empty() {
        Vec::new()
    } else {
        let Some(raw) = std::str::from_utf8(body)
            .ok()
            .and_then(|text| serde_json::from_str::<Box<RawValue>>(text).ok())
        else {
            return Ok(BuildOutcome::Decision(DecisionOutcome::Unreadable));
        };
        js_members(raw.get()).unwrap_or_default()
    };
    // `body.<name> ?? fallback`: a missing or null member falls back.
    let member = |name: &str| {
        members
            .iter()
            .find(|(key, _)| *key == JsText::scalar(name))
            .map(|(_, value)| value.as_ref())
            .filter(|value| value.get() != "null")
    };
    let text = |name: &str| member(name).and_then(|value| js_string(value.get()));
    let kind = match member("kind") {
        None => "user_note".to_owned(),
        Some(_) => match text("kind").and_then(|kind| kind.as_scalar()) {
            Some(kind) if KINDS.contains(&kind.as_str()) => kind,
            _ => return refused("Invalid kind"),
        },
    };
    let actor = if text("actor") == Some(JsText::scalar("assistant")) {
        "assistant"
    } else {
        "user"
    };
    let Some(params) =
        member("params").map_or(Some("{}".to_owned()), |value| reserialize(value.get()))
    else {
        return Ok(BuildOutcome::Decision(DecisionOutcome::Unreadable));
    };
    let result = match member("result") {
        None => Value::Null,
        Some(value) => match reserialize(value.get()) {
            Some(result) => Value::Text(result),
            None => return Ok(BuildOutcome::Decision(DecisionOutcome::Unreadable)),
        },
    };
    let or = |name: &str, fallback: Value| member(name).map_or(Field::Bound(fallback), field);
    let fields = [
        Field::Bound(Value::Text(tenant.into())),
        Field::Bound(Value::Integer(build)),
        Field::Bound(Value::Text(crate::auth::timestamp(0))),
        Field::Bound(Value::Text(actor.into())),
        Field::Bound(Value::Text(kind)),
        or("action_type", Value::Null),
        Field::Bound(Value::Text(params)),
        or("label", Value::Text(String::new())),
        or("summary", Value::Text(String::new())),
        or("rationale", Value::Null),
        Field::Bound(result),
    ];
    match bind(fields) {
        Ok(bound) => insert(tx, bound),
        Err(detail) => refused(detail),
    }
}

/// The parameters better-sqlite3 binds for `fields`, or its refusal.
fn bind(fields: [Field<'_>; PARAMETERS]) -> Result<Vec<Value>, &'static str> {
    let mut bound = Vec::with_capacity(PARAMETERS);
    let mut named = false;
    for value in fields {
        match value {
            Field::Bound(value) => push(&mut bound, value)?,
            Field::Spread(items) => {
                for item in items {
                    let Field::Bound(value) = field(item) else {
                        return Err(UNBINDABLE);
                    };
                    push(&mut bound, value)?;
                }
            }
            Field::Named if named => {
                return Err("You cannot specify named parameters in two different objects");
            }
            Field::Named => named = true,
            Field::Unbindable => return Err(UNBINDABLE),
        }
    }
    if bound.len() < PARAMETERS {
        return Err("Too few parameter values were provided");
    }
    Ok(bound)
}

/// Binds the next parameter; SQLite refuses one past the last at once, so
/// better-sqlite3 stops there.
fn push(bound: &mut Vec<Value>, value: Value) -> Result<(), &'static str> {
    if bound.len() == PARAMETERS {
        return Err("Too many parameter values were provided");
    }
    bound.push(value);
    Ok(())
}

/// Inserts the bound row; a constraint SQLite refuses is Node's 400.
fn insert(tx: &Transaction<'_>, bound: Vec<Value>) -> Result<BuildOutcome> {
    let row = tx.query_row(
        "INSERT INTO plan_decisions(tenant_id,profile_id,created_at,actor,kind,action_type,params_json,label,summary,rationale,result_json) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11) \
         RETURNING id,profile_id,created_at,actor,kind,action_type,params_json,label,summary,rationale,result_json",
        params_from_iter(bound),
        |row| {
            Ok(Created {
                id: row.get(0)?,
                plan_id: row.get(1)?,
                created_at: row.get(2)?,
                actor: row.get(3)?,
                kind: row.get(4)?,
                action_type: row.get(5)?,
                params: object(&row.get::<_, String>(6)?)?,
                label: row.get(7)?,
                summary: row.get(8)?,
                rationale: row.get(9)?,
                result: row
                    .get::<_, Option<String>>(10)?
                    .filter(|result| !result.is_empty())
                    .map(|result| object(&result))
                    .transpose()?,
            })
        },
    );
    match row {
        Ok(created) => Ok(BuildOutcome::Decision(DecisionOutcome::Created(
            serde_json::to_vec(&created)?,
        ))),
        Err(rusqlite::Error::SqliteFailure(error, Some(message)))
            if error.code == ErrorCode::ConstraintViolation =>
        {
            Ok(BuildOutcome::Decision(DecisionOutcome::Refused(message)))
        }
        Err(error) => Err(error.into()),
    }
}
