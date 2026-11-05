//! Accepted Plan Checkoff progress writes: unit completion, unit assembly and
//! printed-count import. Mirrors `db/accepted-plan-progress.ts`,
//! `setAcceptedPrintedCounts` in `db/repository.ts`, and the response shapes of
//! `routes/parts.ts` and `routes/plan-drafts.ts`.

use crate::{
    Envelope, Shared, WriterOwner, auth,
    read_model::{AcceptedRead, Credential, Part, Snapshot, graph},
};
use anyhow::{Result, anyhow, ensure};
use rusqlite::{Connection, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const MAX_SAFE: i64 = 9_007_199_254_740_991;
const MAX_IMPORT: i64 = 10_000;

/// Accepts JSON numbers that JavaScript treats as safe integers, including `1.0`.
fn safe_integer<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<i64, D::Error> {
    let number = serde_json::Number::deserialize(deserializer)?;
    if let Some(value) = number
        .as_i64()
        .filter(|n| n.unsigned_abs() <= MAX_SAFE as u64)
    {
        return Ok(value);
    }
    match number.as_f64() {
        Some(n) if n.is_finite() && n.fract() == 0.0 && n.abs() <= MAX_SAFE as f64 => Ok(n as i64),
        _ => Err(serde::de::Error::custom(
            "Expected a JavaScript-safe integer",
        )),
    }
}

fn lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_digest(value: &str) -> bool {
    lower_hex(value, 64)
}

fn is_token(value: &str) -> bool {
    value
        .strip_prefix("ppu_")
        .is_some_and(|hex| lower_hex(hex, 32))
}

/// The Accepted Plan a caller observed. Every write refuses a different basis.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Basis {
    #[serde(deserialize_with = "safe_integer")]
    pub profile_id: i64,
    #[serde(deserialize_with = "safe_integer")]
    pub plan_version: i64,
    #[serde(deserialize_with = "safe_integer")]
    pub plan_revision_id: i64,
    pub plan_revision_digest: String,
    pub required_unit_mapping_digest: String,
}
impl From<&Snapshot> for Basis {
    fn from(s: &Snapshot) -> Self {
        Self {
            profile_id: s.profile.id,
            plan_version: s.plan_version,
            plan_revision_id: s.revision_id,
            plan_revision_digest: s.revision_digest.clone(),
            required_unit_mapping_digest: s.required_unit_mapping_digest.clone(),
        }
    }
}
impl Basis {
    fn valid(&self) -> bool {
        [self.profile_id, self.plan_version, self.plan_revision_id]
            .into_iter()
            .all(|v| (1..=MAX_SAFE).contains(&v))
            && is_digest(&self.plan_revision_digest)
            && is_digest(&self.required_unit_mapping_digest)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportRow {
    #[serde(deserialize_with = "safe_integer")]
    pub part_id: i64,
    #[serde(deserialize_with = "safe_integer")]
    pub printed_count: i64,
}

/// `POST /plans/:id/progress/import` body.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Import {
    pub expected: Basis,
    pub rows: Vec<ImportRow>,
}
impl Import {
    /// Parses with the `acceptedProgressImportRequestSchema` rules.
    pub fn parse(value: Value) -> std::result::Result<Self, Response> {
        let request: Self =
            serde_json::from_value(value).map_err(|_| Refusal::InvalidRequest.response(true))?;
        request.validate()?;
        Ok(request)
    }
    fn validate(&self) -> std::result::Result<(), Response> {
        let mut seen = HashSet::new();
        let rows_valid = (1..=MAX_IMPORT as usize).contains(&self.rows.len())
            && self.rows.iter().all(|r| {
                (1..=MAX_SAFE).contains(&r.part_id)
                    && (0..=MAX_IMPORT).contains(&r.printed_count)
                    && seen.insert(r.part_id)
            });
        if self.expected.valid() && rows_valid {
            Ok(())
        } else {
            Err(Refusal::InvalidRequest.response(true))
        }
    }
}

#[derive(Clone, Debug)]
pub enum Request {
    /// Marks the unit and every lower unit printed, or the unit and every
    /// higher unit unprinted and unassembled.
    Completion {
        expected: Basis,
        token: String,
        completed: bool,
    },
    /// Sets assembly on one unit. Assembling an unprinted unit is a no-op.
    Assembly {
        expected: Basis,
        token: String,
        assembled: bool,
    },
    /// Sets each listed Part's printed prefix to `printed_count` units.
    Import(Import),
    /// `PATCH /parts/:id/progress` with its raw JSON body.
    CompletionCoordinate { part_id: i64, body: Value },
    /// `PATCH /parts/:id/assembled` with its raw JSON body.
    AssemblyCoordinate { part_id: i64, body: Value },
}
impl Request {
    fn is_import(&self) -> bool {
        matches!(self, Self::Import(_))
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct CompletionBody {
    pub part_id: i64,
    pub printed_count: usize,
    pub print_units: Vec<bool>,
    pub assembled_units: Vec<bool>,
    pub missing: bool,
}
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AssemblyBody {
    pub part_id: i64,
    pub assembled_count: usize,
    pub assembled_units: Vec<bool>,
}
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(untagged)]
pub enum Body {
    Completion(CompletionBody),
    Assembly(AssemblyBody),
    Imported {
        updated_parts: usize,
    },
    Failure {
        detail: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
}
/// HTTP status and JSON body, matching the Node route for the operation.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Response {
    pub status: u16,
    pub body: Body,
}
impl Response {
    fn ok(body: Body) -> Self {
        Self { status: 200, body }
    }
    fn failure(status: u16, detail: &str, code: Option<&str>) -> Self {
        Self {
            status,
            body: Body::Failure {
                detail: detail.into(),
                code: code.map(str::to_owned),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Refusal {
    InvalidRequest,
    PartNotFound,
    Missing,
    Stale,
    Archived,
    Dirty,
    Uninitialized,
    InvalidRows,
    Unavailable,
    Integrity,
    Internal,
}
impl Refusal {
    /// Import responses come from `routes/plan-drafts.ts` and carry a code; the
    /// unit responses come from `routes/parts.ts` and do not.
    fn response(self, import: bool) -> Response {
        use Refusal::*;
        let (status, detail, code) = match (self, import) {
            (InvalidRequest, true) => (400, "Request is invalid", "invalid_request"),
            (InvalidRequest, false) => (400, "Invalid Accepted Plan progress request", ""),
            (PartNotFound, _) => (404, "Part not found", ""),
            (Missing, true) => (404, "Plan or Part not found", "progress_target_not_found"),
            (Missing, false) => (404, "Part unit not found", ""),
            (Stale, _) => (
                409,
                "Accepted Plan changed; reload and retry",
                "stale_accepted_plan",
            ),
            (Archived, _) => (
                409,
                "Archived Plan Progress cannot be changed",
                "plan_archived",
            ),
            (Dirty | Uninitialized, true) => (
                409,
                "Accepted Plan state is unavailable",
                "accepted_state_unavailable",
            ),
            (Dirty, false) => (409, "Accepted Plan requires compatibility repair", ""),
            (Uninitialized, false) => (
                409,
                "Accepted Plan operational state is not initialized",
                "",
            ),
            (InvalidRows, _) => (422, "Printed counts are invalid", "invalid_rows"),
            (Unavailable, true) => (
                503,
                "Plan draft update is unavailable",
                "transaction_unavailable",
            ),
            (Unavailable, false) => (503, "Accepted Plan update is unavailable", ""),
            (Integrity, false) => (500, "Accepted Plan data is inconsistent", ""),
            (Integrity | Internal, true) => {
                (500, "Accepted Progress import failed", "internal_error")
            }
            (Internal, false) => (500, "Internal Server Error", ""),
        };
        Response::failure(status, detail, (import && !code.is_empty()).then_some(code))
    }
}

#[derive(Clone)]
pub struct ProgressClient {
    shared: Arc<Shared>,
    repos: PathBuf,
    policy: auth::AuthPolicy,
}
/// A validated request. Part coordinates resolve to a token inside the writer.
enum Op {
    Unit { target: Target, change: Change },
    Import(Import),
}
enum Target {
    Token { expected: Basis, token: String },
    Coordinate { part_id: i64, unit_index: i64 },
}
#[derive(Clone, Copy)]
enum Change {
    Completed(bool),
    Assembled(bool),
}
impl Op {
    fn is_import(&self) -> bool {
        matches!(self, Self::Import(_))
    }
}
pub(crate) struct Command {
    credential: Credential,
    op: Op,
    repos: PathBuf,
    policy: auth::AuthPolicy,
}
impl WriterOwner {
    pub fn checkoff_progress(&self) -> Result<ProgressClient> {
        self.checkoff_progress_with_policy(auth::AuthPolicy {
            registration: auth::RegistrationPolicy::Open,
            session_tenant: auth::SessionTenantPolicy::AccountTenant,
            first_user: auth::FirstUserTenant::NewUser,
        })
    }
    pub fn checkoff_progress_with_policy(
        &self,
        policy: auth::AuthPolicy,
    ) -> Result<ProgressClient> {
        auth::validate_policy(policy)?;
        Ok(ProgressClient {
            shared: self.client.shared.clone(),
            repos: self
                .lease
                .as_ref()
                .ok_or_else(|| anyhow!("Storage owner is closed"))?
                .data_dir()
                .join("repos"),
            policy,
        })
    }
}
impl ProgressClient {
    /// Returns the Node response for the request. `Err` means authentication
    /// failed or the outcome is unknown after admission.
    pub fn apply(
        &self,
        credential: Credential,
        request: Request,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Response> {
        let import = request.is_import();
        let op = match validate(request) {
            Ok(op) => op,
            Err(response) => return Ok(response),
        };
        let deadline = Instant::now() + wait;
        let mut queue = self
            .shared
            .queue
            .lock()
            .map_err(|_| anyhow!("Writer admission poisoned"))?;
        loop {
            if queue.closed || cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
                return Ok(Refusal::Unavailable.response(import));
            }
            if queue.pending.len() < self.shared.capacity {
                break;
            }
            queue = self
                .shared
                .changed
                .wait_timeout(queue, Duration::from_millis(5))
                .map_err(|_| anyhow!("Writer admission poisoned"))?
                .0;
        }
        let (reply, receiver) = mpsc::channel();
        queue.pending.push_back(Envelope::Checkoff {
            command: Command {
                credential,
                op,
                repos: self.repos.clone(),
                policy: self.policy,
            },
            reply,
        });
        self.shared.changed.notify_all();
        drop(queue);
        receiver.recv().map_err(|_| {
            anyhow!("Checkoff reply lost after admission; reread progress before retrying")
        })?
    }
}

/// Mirrors the `routes/parts.ts` body checks: `== null` first, then
/// `Number.isInteger` and `typeof boolean`.
fn coordinate(body: &Value, field: &str) -> std::result::Result<(i64, bool), Response> {
    let absent = |key: &str| body.get(key).is_none_or(Value::is_null);
    if absent("unit_index") || absent(field) {
        return Err(Response::failure(
            400,
            &format!("unit_index and {field} required"),
            None,
        ));
    }
    let index = body["unit_index"]
        .as_f64()
        .filter(|x| x.is_finite() && x.fract() == 0.0 && *x >= 0.0);
    match (index, body[field].as_bool()) {
        // Indices past the safe range cannot name a unit; clamping keeps them out of range.
        (Some(index), Some(flag)) => Ok((index.min(MAX_SAFE as f64) as i64, flag)),
        _ => Err(Response::failure(
            400,
            &format!("unit_index must be a non-negative integer and {field} a boolean"),
            None,
        )),
    }
}

fn validate(request: Request) -> std::result::Result<Op, Response> {
    let (target, change) = match request {
        Request::Import(import) => return import.validate().map(|()| Op::Import(import)),
        Request::Completion {
            expected,
            token,
            completed,
        } => (
            Target::Token { expected, token },
            Change::Completed(completed),
        ),
        Request::Assembly {
            expected,
            token,
            assembled,
        } => (
            Target::Token { expected, token },
            Change::Assembled(assembled),
        ),
        Request::CompletionCoordinate { part_id, body } => {
            let (unit_index, completed) = coordinate(&body, "completed")?;
            (
                Target::Coordinate {
                    part_id,
                    unit_index,
                },
                Change::Completed(completed),
            )
        }
        Request::AssemblyCoordinate { part_id, body } => {
            let (unit_index, assembled) = coordinate(&body, "assembled")?;
            (
                Target::Coordinate {
                    part_id,
                    unit_index,
                },
                Change::Assembled(assembled),
            )
        }
    };
    if let Target::Token { expected, token } = &target
        && !(expected.valid() && is_token(token))
    {
        return Err(Refusal::InvalidRequest.response(false));
    }
    Ok(Op::Unit { target, change })
}

pub(crate) fn execute(
    connection: &mut Connection,
    command: Command,
    admission: crate::writer_admission::Deadline,
) -> Result<Response> {
    let import = command.op.is_import();
    let tx = match crate::writer_admission::begin_immediate(connection, admission) {
        Ok(tx) => tx,
        Err(error) => {
            let refusal = match error.downcast_ref::<rusqlite::Error>() {
                Some(error)
                    if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy) =>
                {
                    Refusal::Unavailable
                }
                _ => Refusal::Internal,
            };
            return Ok(refusal.response(import));
        }
    };
    let (tenant, _) =
        auth::authority::queries::reconciliation_actor(&tx, command.credential, command.policy)?;
    let response = match mutate(&tx, &tenant, command.op, &command.repos) {
        Ok(Ok(response)) => response,
        Ok(Err(refusal)) => return Ok(refusal.response(import)),
        Err(error) if error.is::<graph::Integrity>() => {
            return Ok(Refusal::Integrity.response(import));
        }
        Err(_) => return Ok(Refusal::Internal.response(import)),
    };
    // Authentication may have updated key metadata, so only success commits.
    if response.status == 200 && tx.commit().is_err() {
        return Ok(Refusal::Internal.response(import));
    }
    Ok(response)
}

type Outcome = Result<std::result::Result<Response, Refusal>>;

/// Mirrors `readExpectedSnapshot`. Returns the tenant's Ready snapshot only when
/// it matches the expected basis.
fn current(
    tx: &Transaction<'_>,
    tenant: &str,
    expected: &Basis,
    repos: &Path,
) -> Result<std::result::Result<Snapshot, Refusal>> {
    let snapshot = match read(tx, tenant, expected.profile_id, repos)? {
        AcceptedRead::Ready { snapshot } => *snapshot,
        AcceptedRead::Missing => return Ok(Err(Refusal::Missing)),
        AcceptedRead::Empty { .. } => return Ok(Err(Refusal::Stale)),
        AcceptedRead::CompatibilityDirty => return Ok(Err(Refusal::Dirty)),
        AcceptedRead::Uninitialized => return Ok(Err(Refusal::Uninitialized)),
        AcceptedRead::IntegrityFailure { .. } => return Ok(Err(Refusal::Integrity)),
    };
    if *expected != Basis::from(&snapshot) {
        return Ok(Err(Refusal::Stale));
    }
    Ok(Ok(snapshot))
}

fn read(tx: &Transaction<'_>, tenant: &str, profile: i64, repos: &Path) -> Result<AcceptedRead> {
    graph::read(tx, tenant, profile, repos, &mut graph::Budget::default())
}

fn archived(snapshot: &Snapshot) -> bool {
    snapshot
        .profile
        .archived_at
        .as_deref()
        .is_some_and(|s| !s.is_empty())
}

fn mutate(tx: &Transaction<'_>, tenant: &str, op: Op, repos: &Path) -> Outcome {
    let (mut snapshot, token, change) = match op {
        Op::Import(import) => {
            let snapshot = match current(tx, tenant, &import.expected, repos)? {
                Ok(snapshot) => snapshot,
                Err(refusal) => return Ok(Err(refusal)),
            };
            if archived(&snapshot) {
                return Ok(Err(Refusal::Archived));
            }
            let outcome = import_counts(tx, tenant, &snapshot, import)?;
            if matches!(&outcome, Ok(response) if response.status == 200) {
                progress_changed(tx, tenant, snapshot.profile.id, None)?;
            }
            return Ok(outcome);
        }
        Op::Unit {
            target: Target::Token { expected, token },
            change,
        } => match current(tx, tenant, &expected, repos)? {
            Ok(snapshot) => (snapshot, token, change),
            Err(refusal) => return Ok(Err(refusal)),
        },
        Op::Unit {
            target:
                Target::Coordinate {
                    part_id,
                    unit_index,
                },
            change,
        } => match locate_coordinate(tx, tenant, part_id, unit_index, repos)? {
            Ok((snapshot, token)) => (snapshot, token, change),
            Err(response) => return Ok(Ok(response)),
        },
    };
    if archived(&snapshot) {
        return Ok(Err(Refusal::Archived));
    }
    complete_rows(tx, tenant, &snapshot)?;
    let profile = snapshot.profile.id;
    // The read model already refuses duplicate tokens.
    let Some((part, position)) = snapshot.parts.iter_mut().find_map(|part| {
        let position = part.units.iter().position(|u| u.token == token)?;
        Some((part, position))
    }) else {
        return Ok(Err(Refusal::Missing));
    };
    let part_id = part.projection_part_id;
    let index = part.units[position].unit_index;
    match change {
        Change::Completed(completed) => {
            for unit in &mut part.units {
                let changes = if completed {
                    unit.unit_index <= index
                } else {
                    unit.unit_index >= index
                };
                if changes {
                    set_completed(tx, tenant, part_id, unit.unit_index, completed)?;
                    unit.completed = completed;
                    unit.assembled &= completed;
                }
            }
            progress_changed(tx, tenant, profile, Some(part_id))?;
            Ok(Ok(completion(part)))
        }
        Change::Assembled(assembled) => {
            let unit = &mut part.units[position];
            if !assembled || unit.completed {
                let changed = tx.execute(
                    "UPDATE print_progress SET assembled=?1 WHERE tenant_id=?2 AND part_id=?3 AND unit_index=?4",
                    params![assembled, tenant, part_id, index],
                )?;
                ensure!(
                    changed == 1,
                    "Progress update did not find exactly one current unit"
                );
                unit.assembled = assembled;
            }
            progress_changed(tx, tenant, profile, Some(part_id))?;
            Ok(Ok(assembly(part)))
        }
    }
}

/// Records the change in the domain event log inside the Checkoff transaction.
fn progress_changed(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    part: Option<i64>,
) -> Result<()> {
    crate::domain_events::append(
        tx,
        tenant,
        crate::domain_events::Change {
            kind: crate::domain_events::PROGRESS_CHANGED,
            aggregate_type: "build",
            aggregate_id: profile.to_string(),
            aggregate_version: None,
            payload: match part {
                Some(part) => serde_json::json!({ "part_id": part }),
                None => serde_json::json!({ "import": true }),
            },
        },
    )
}

/// Resolves `/parts/:id` and a unit index to the current snapshot and unit
/// token, with the `readAcceptedPartRequest` responses.
fn locate_coordinate(
    tx: &Transaction<'_>,
    tenant: &str,
    part_id: i64,
    index: i64,
    repos: &Path,
) -> Result<std::result::Result<(Snapshot, String), Response>> {
    let not_found = || Ok(Err(Refusal::PartNotFound.response(false)));
    let profile = tx
        .query_row(
            "SELECT profile_id FROM parts WHERE tenant_id=?1 AND id=?2",
            params![tenant, part_id],
            |r| r.get::<_, i64>(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            e => Err(e),
        })?;
    let Some(profile) = profile else {
        return not_found();
    };
    let snapshot = match read(tx, tenant, profile, repos)? {
        AcceptedRead::Ready { snapshot } => *snapshot,
        AcceptedRead::Missing | AcceptedRead::Empty { .. } => return not_found(),
        AcceptedRead::CompatibilityDirty => return Ok(Err(Refusal::Dirty.response(false))),
        AcceptedRead::Uninitialized => return Ok(Err(Refusal::Uninitialized.response(false))),
        AcceptedRead::IntegrityFailure { .. } => {
            return Ok(Err(Refusal::Integrity.response(false)));
        }
    };
    let Some(part) = snapshot
        .parts
        .iter()
        .find(|p| p.projection_part_id == part_id)
    else {
        return not_found();
    };
    let Some(unit) = part.units.iter().find(|u| u.unit_index == index) else {
        return Ok(Err(Response::failure(400, "unit_index out of range", None)));
    };
    let token = unit.token.clone();
    Ok(Ok((snapshot, token)))
}

/// Mirrors `assertCompleteProgress`. Each Part must have one tenant row per unit.
fn complete_rows(tx: &Transaction<'_>, tenant: &str, snapshot: &Snapshot) -> Result<()> {
    let mut stmt = tx.prepare(
        "SELECT tenant_id,unit_index FROM print_progress WHERE part_id=?1 AND unit_index<?2 ORDER BY unit_index",
    )?;
    for part in &snapshot.parts {
        let rows = stmt
            .query_map(
                params![part.projection_part_id, part.units.len() as i64],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if rows.len() != part.units.len()
            || rows
                .iter()
                .enumerate()
                .any(|(i, (t, index))| t != tenant || *index != i as i64)
        {
            return Err(graph::Integrity {
                code: "progress",
                message: "Accepted Plan progress rows are incomplete".into(),
            }
            .into());
        }
    }
    Ok(())
}

fn set_completed(
    tx: &Transaction<'_>,
    tenant: &str,
    part: i64,
    index: i64,
    completed: bool,
) -> Result<()> {
    let changed = tx.execute(
        "UPDATE print_progress SET completed=?1, assembled=CASE WHEN ?1 THEN assembled ELSE 0 END WHERE tenant_id=?2 AND part_id=?3 AND unit_index=?4",
        params![completed, tenant, part, index],
    )?;
    ensure!(
        changed == 1,
        "Progress update did not find exactly one current unit"
    );
    Ok(())
}

/// Mirrors `setAcceptedPrintedCounts`. Checks every row before writing any.
fn import_counts(
    tx: &Transaction<'_>,
    tenant: &str,
    snapshot: &Snapshot,
    import: Import,
) -> Outcome {
    let mut targets = Vec::with_capacity(import.rows.len());
    for row in &import.rows {
        let Some(part) = snapshot
            .parts
            .iter()
            .find(|p| p.projection_part_id == row.part_id)
        else {
            return Ok(Err(Refusal::Missing));
        };
        if row.printed_count > part.units.len() as i64 {
            return Ok(Err(Refusal::InvalidRows));
        }
        targets.push((part, row.printed_count));
    }
    let mut updated_parts = 0;
    for (part, printed) in targets {
        if part
            .units
            .iter()
            .any(|u| u.completed != (u.unit_index < printed))
        {
            updated_parts += 1;
        }
        for unit in &part.units {
            set_completed(
                tx,
                tenant,
                part.projection_part_id,
                unit.unit_index,
                unit.unit_index < printed,
            )?;
        }
    }
    Ok(Ok(Response::ok(Body::Imported { updated_parts })))
}

fn completion(part: &Part) -> Response {
    let printed_count = part.units.iter().filter(|u| u.completed).count();
    Response::ok(Body::Completion(CompletionBody {
        part_id: part.projection_part_id,
        printed_count,
        print_units: part.units.iter().map(|u| u.completed).collect(),
        assembled_units: part.units.iter().map(|u| u.assembled).collect(),
        missing: printed_count < part.units.len(),
    }))
}

fn assembly(part: &Part) -> Response {
    Response::ok(Body::Assembly(AssemblyBody {
        part_id: part.projection_part_id,
        assembled_count: part.units.iter().filter(|u| u.assembled).count(),
        assembled_units: part.units.iter().map(|u| u.assembled).collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer_admission::Deadline;
    use serde_json::json;

    fn command(import: bool) -> Command {
        let request = if import {
            Request::Import(Import {
                expected: Basis {
                    profile_id: 1,
                    plan_version: 1,
                    plan_revision_id: 1,
                    plan_revision_digest: "a".repeat(64),
                    required_unit_mapping_digest: "b".repeat(64),
                },
                rows: vec![ImportRow {
                    part_id: 1,
                    printed_count: 1,
                }],
            })
        } else {
            Request::CompletionCoordinate {
                part_id: 1,
                body: json!({ "unit_index": 0, "completed": true }),
            }
        };
        Command {
            credential: Credential::Session(auth::Secret::new("unused".into())),
            op: validate(request).unwrap_or_else(|response| panic!("{response:?}")),
            repos: PathBuf::new(),
            policy: auth::AuthPolicy {
                registration: auth::RegistrationPolicy::Open,
                session_tenant: auth::SessionTenantPolicy::AccountTenant,
                first_user: auth::FirstUserTenant::NewUser,
            },
        }
    }

    fn assert_expired_admission(import: bool) {
        let mut connection = Connection::open_in_memory().unwrap();
        let response = execute(&mut connection, command(import), Deadline::expired()).unwrap();
        let body = if import {
            json!({ "detail": "Plan draft update is unavailable", "code": "transaction_unavailable" })
        } else {
            json!({ "detail": "Accepted Plan update is unavailable" })
        };
        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({ "status": 503, "body": body })
        );
        assert!(connection.is_autocommit());
    }

    fn assert_nonbusy_begin_failure(import: bool) {
        let mut connection = Connection::open_in_memory().unwrap();
        connection.pragma_update(None, "query_only", true).unwrap();
        let error =
            crate::writer_admission::begin_immediate(&mut connection, Deadline::at_dequeue())
                .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<rusqlite::Error>()
                .unwrap()
                .sqlite_error_code(),
            Some(rusqlite::ErrorCode::ReadOnly)
        );
        let response = execute(&mut connection, command(import), Deadline::at_dequeue()).unwrap();
        let body = if import {
            json!({ "detail": "Accepted Progress import failed", "code": "internal_error" })
        } else {
            json!({ "detail": "Internal Server Error" })
        };
        assert_eq!(
            serde_json::to_value(response).unwrap(),
            json!({ "status": 500, "body": body })
        );
        assert!(connection.is_autocommit());
    }

    #[test]
    fn import_admission_deadline_is_unavailable() {
        assert_expired_admission(true);
    }

    #[test]
    fn completion_admission_deadline_is_unavailable() {
        assert_expired_admission(false);
    }

    #[test]
    fn import_nonbusy_begin_failure_is_internal() {
        assert_nonbusy_begin_failure(true);
    }

    #[test]
    fn completion_nonbusy_begin_failure_is_internal() {
        assert_nonbusy_begin_failure(false);
    }
}
