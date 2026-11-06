//! Node's `GET` and `PATCH /plans/{id}/production-setup`
//! (`routes/production-setup.ts`, `services/production-setup-store.ts`): the
//! Build's Production setup in the `production_setup:<id>` setting, and the one
//! command a PATCH applies to it.
//!
//! Node retries its setting compare-and-set and answers 409 after eight
//! conflicts. Here the read, the lock check and the write share one immediate
//! transaction on the single writer, so no conflict can occur.

use super::*;
use crate::checkoff_progress::links;
use crate::production::setup::{apply, default_setup, setup_command, stored_setup};
use pp_domain::gcode_objects::interpret_sliced_object_name;

fn detail(status: u16, detail: &str) -> Response {
    Response {
        status,
        body: json!({"detail": detail}),
    }
}

/// Node's `parseProfileId`: `Number(raw)`, then a positive integer. `None` is 400;
/// an integer past the safe range names no Build.
fn build_id(raw: &str) -> Option<Option<i64>> {
    let text = trim_ecmascript(raw);
    let radix = [
        ("0x", 16),
        ("0X", 16),
        ("0o", 8),
        ("0O", 8),
        ("0b", 2),
        ("0B", 2),
    ]
    .into_iter()
    .find_map(|(prefix, radix)| Some((text.strip_prefix(prefix)?, radix)));
    let number = match radix {
        Some((digits, radix)) => {
            if digits.is_empty() {
                return None;
            }
            digits.chars().try_fold(0.0, |total: f64, digit| {
                Some(total * f64::from(radix) + f64::from(digit.to_digit(radix)?))
            })?
        }
        None if text.is_empty() => 0.0,
        None => {
            let mantissa = text.split(['e', 'E']).next().unwrap_or_default();
            if !text
                .bytes()
                .all(|b| b.is_ascii_digit() || b"+-.eE".contains(&b))
                || !mantissa.bytes().any(|b| b.is_ascii_digit())
            {
                return None;
            }
            text.parse().ok()?
        }
    };
    (number.fract() == 0.0 && number > 0.0)
        .then(|| (number <= MAX_JS_SAFE_INTEGER as f64).then_some(number as i64))
}

pub(super) fn run(
    tx: &Transaction<'_>,
    tenant: &str,
    repos: &Path,
    profile: &str,
    body: Option<Value>,
) -> Result<Response> {
    let Some(profile) = build_id(profile) else {
        return Ok(detail(400, "invalid Build id"));
    };
    let Some(profile) = profile else {
        return Ok(detail(404, "Build not found"));
    };
    if !visible(tx, tenant, profile)? {
        return Ok(detail(404, "Build not found"));
    }
    let key = format!("production_setup:{profile}");
    let stored: Option<String> = tx
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
            params![tenant, key],
            |row| row.get(0),
        )
        .optional()?;
    let setup = stored_setup(stored.as_deref(), profile).unwrap_or_else(|| default_setup(profile));
    let Some(body) = body else {
        return Ok(Response {
            status: 200,
            body: setup,
        });
    };
    let command = match setup_command(&body) {
        Ok(command) => command,
        Err(issues) => {
            return Ok(Response {
                status: 400,
                body: json!({"detail": "Invalid production setup", "issues": issues}),
            });
        }
    };
    if command.field == "route" && !setup["route"].is_null() && command.value != setup["route"] {
        match locked(tx, tenant, profile, repos, &setup)? {
            Some(true) => {
                return Ok(failure(
                    409,
                    "production_method_locked",
                    "Recorded prints for these Required units keep the Production method",
                    Map::new(),
                ));
            }
            Some(false) => {}
            None => {
                return Ok(detail(
                    503,
                    "Production method could not be checked against recorded prints",
                ));
            }
        }
    }
    let next = apply(&setup, command, profile, &links::now());
    tx.execute(
        "INSERT INTO app_settings(tenant_id,key,value) VALUES(?1,?2,?3) ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value",
        params![tenant, key, next.to_string()],
    )?;
    Ok(Response {
        status: 200,
        body: next,
    })
}

/// Node's `recordedPrintsLockProductionMethod`: whether a Printer Checkoff link
/// of this Build that is not dismissed names a Required unit the next package
/// makes. `None` when the Accepted Plan state cannot be read.
fn locked(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    repos: &Path,
    setup: &Value,
) -> Result<Option<bool>> {
    let snapshot = match accepted(tx, tenant, profile, repos)? {
        Ok(snapshot) => Some(snapshot),
        Err(AcceptedRead::Empty { .. } | AcceptedRead::Missing) => None,
        Err(AcceptedRead::CompatibilityDirty | AcceptedRead::Uninitialized) => return Ok(None),
        Err(AcceptedRead::IntegrityFailure { .. } | AcceptedRead::Ready { .. }) => {
            bail!(Integrity("accepted"))
        }
    };
    let units = match &snapshot {
        Some(snapshot) => {
            stored_layout(tx, tenant, profile, &Basis::of(snapshot), snapshot)?;
            required(snapshot)
        }
        None => Vec::new(),
    };
    let token = |token: &str| RequiredUnitToken::parse(token.to_owned()).is_ok();
    let package: HashSet<&str> = match setup["selection"]["selected_unit_tokens"].as_array() {
        Some(selected) => {
            let available: HashSet<&str> =
                units.iter().map(|(_, unit)| unit.token.as_str()).collect();
            selected
                .iter()
                .filter_map(Value::as_str)
                .filter(|selected| available.contains(selected) && token(selected))
                .collect()
        }
        None => units
            .iter()
            .filter(|(_, unit)| !unit.completed && token(&unit.token))
            .map(|(_, unit)| unit.token.as_str())
            .collect(),
    };
    if package.is_empty() {
        return Ok(Some(false));
    }
    Ok(Some(links::load(tx, tenant, &links::now())?.iter().any(
        |link| {
            link["profile_id"].as_i64() == Some(profile)
                && link["state"] != "dismissed"
                && link["units"].as_array().into_iter().flatten().any(|unit| {
                    unit["object_name"].as_str().is_some_and(|name| {
                        let name = interpret_sliced_object_name(name).unwrapped_name;
                        name.rsplit_once("__")
                            .is_some_and(|(_, tail)| token(tail) && package.contains(tail))
                    })
                })
        },
    )))
}

#[cfg(test)]
mod tests {
    use super::build_id;

    #[test]
    fn build_ids_follow_javascript_number() {
        for (raw, id) in [
            ("1", Some(Some(1))),
            (" 1 ", Some(Some(1))),
            ("0x1", Some(Some(1))),
            ("0b11", Some(Some(3))),
            ("1e0", Some(Some(1))),
            ("1.", Some(Some(1))),
            ("+2", Some(Some(2))),
            ("9007199254740993", Some(None)),
            ("abc", None),
            ("0", None),
            ("-1", None),
            ("1.5", None),
            ("", None),
            ("0x", None),
            ("Infinity", None),
            ("inf", None),
            ("1_0", None),
            ("e3", None),
        ] {
            assert_eq!(build_id(raw), id, "{raw:?}");
        }
    }
}
