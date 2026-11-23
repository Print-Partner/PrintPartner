//! Opening the database turns `applying` into `unknown`
//! ([`interrupt_applying`]), and nothing returns `unknown` to `pending`.
//! Rows are never deleted. Staging accepts only dependencies that exist and
//! can still apply, and `depends_on` never changes afterwards, so
//! dependencies form a DAG and staging order is an apply order. `staged_by`
//! is a display label, and no query filters on it.

use crate::{SettingsClient, WriterFailure, auth::AuthPolicy, read_model::Credential, write};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{fmt, sync::atomic::AtomicBool, time::Duration};

pub(crate) const SCHEMA_STEP: u64 = 51;
/// How long a staged action stays confirmable.
pub const TTL: Duration = Duration::from_secs(8 * 60 * 60);
/// `list` returns at most this many actions: the newest, oldest first.
pub const LIST_LIMIT: usize = 200;
pub const MAX_DEPENDENCIES: usize = 16;
/// Rows a dependency walk reads before it calls the stored graph corrupt.
const MAX_WALK: usize = 64;
const LABEL_LIMIT: usize = 120;
const COLUMNS: &str = "id, action_type, plan_id, label, summary, params_json, depends_on_json, \
                       staged_by, state, created_at, expires_at, settled_at";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Pending,
    /// Pending and past `expires_at`. Never stored.
    Expired,
    Applying,
    Applied,
    Dismissed,
    Unknown,
    /// Invalidated by a restore while open. Must be staged again.
    InvalidatedByRestore,
}

impl State {
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "pending" => Self::Pending,
            "applying" => Self::Applying,
            "applied" => Self::Applied,
            "dismissed" => Self::Dismissed,
            "unknown" => Self::Unknown,
            "invalidated_by_restore" => Self::InvalidatedByRestore,
            _ => return None,
        })
    }
}

/// Who staged an action, for display: trimmed, control characters removed,
/// at most 120 characters, never empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedBy(String);

impl StagedBy {
    pub fn label(text: &str) -> Self {
        let cleaned: String = text
            .chars()
            .filter(|c| !c.is_control())
            .collect::<String>()
            .trim()
            .chars()
            .take(LABEL_LIMIT)
            .collect();
        match cleaned.trim_end() {
            "" => Self("MCP client".into()),
            label => Self(label.into()),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub struct NewAction {
    pub id: String,
    pub action_type: String,
    pub plan_id: i64,
    pub label: String,
    pub summary: String,
    /// Opaque to storage. Stored and returned as given.
    pub params: Map<String, Value>,
    pub depends_on: Vec<String>,
    pub staged_by: StagedBy,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PendingAction {
    pub id: String,
    pub action_type: String,
    pub plan_id: i64,
    pub label: String,
    pub summary: String,
    pub params: Map<String, Value>,
    pub depends_on: Vec<String>,
    pub staged_by: String,
    pub state: State,
    pub created_at: String,
    pub expires_at: String,
    /// When it was applied, dismissed, invalidated by restore, or became unknown.
    pub settled_at: Option<String>,
}

#[derive(Debug, PartialEq)]
pub struct Listing {
    pub actions: Vec<PendingAction>,
    /// Open actions older than the newest [`LIST_LIMIT`].
    pub omitted: u64,
}

/// The action that has to be applied first, and its state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blocker {
    pub id: String,
    pub label: String,
    pub state: State,
}

#[must_use = "settle the claim, or the action stays applying until restart"]
pub struct Claim {
    action: PendingAction,
    token: ClaimToken,
}

impl Claim {
    pub fn action(&self) -> &PendingAction {
        &self.action
    }

    pub fn token(&self) -> &ClaimToken {
        &self.token
    }

    pub fn into_parts(self) -> (PendingAction, ClaimToken) {
        (self.action, self.token)
    }
}

impl fmt::Debug for Claim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Claim")
            .field("id", &self.action.id)
            .finish_non_exhaustive()
    }
}

/// Proof of holding one claim attempt on one action. Only [`PendingActionsClient::claim`]
/// creates one.
pub struct ClaimToken {
    id: String,
    secret: [u8; 32],
}

impl fmt::Debug for ClaimToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaimToken")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    Applied,
    /// Only for a failure positively known to have written nothing. The action
    /// is pending again. Settle an ambiguous outcome as [`End::Unknown`].
    Released,
    /// The write may have committed. Never applied again.
    Unknown,
}

#[derive(Debug, thiserror::Error)]
pub enum ActionError {
    #[error("no pending action has this id")]
    NotFound,
    #[error("the action expired")]
    Expired { expires_at: String },
    #[error("another action must be applied first")]
    Blocked(Blocker),
    #[error("the action is being applied")]
    InFlight,
    #[error("the action was already applied")]
    AlreadyApplied { at: String },
    #[error("the action was dismissed")]
    AlreadyDismissed { at: String },
    #[error("the action's outcome is unknown")]
    OutcomeUnknown,
    #[error("the action was invalidated by a restore and must be staged again")]
    InvalidatedByRestore { at: String },
    #[error("could not allocate a pending action claim token")]
    ClaimTokenUnavailable,
    /// Stage only. `id` names no action (`None`) or one that cannot apply.
    #[error("a dependency names no action or cannot apply")]
    Dependency { id: String, state: Option<State> },
    #[error("an action lists more than {MAX_DEPENDENCIES} dependencies")]
    TooManyDependencies,
    #[error("an action with this id is already staged")]
    IdTaken,
    #[error("stored pending action {id} is not the typed shape")]
    StoredRowInvalid { id: String },
    #[error(transparent)]
    Writer(#[from] WriterFailure),
}

impl From<rusqlite::Error> for ActionError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Writer(error.into())
    }
}

#[derive(Clone)]
pub struct PendingActionsClient {
    storage: SettingsClient,
    policy: AuthPolicy,
}

impl crate::read_model::ReadClient {
    pub fn pending_actions(&self) -> PendingActionsClient {
        PendingActionsClient {
            storage: self.storage.clone(),
            policy: self.policy,
        }
    }
}

impl PendingActionsClient {
    pub fn stage(
        &self,
        credential: Credential,
        mut action: NewAction,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<PendingAction, ActionError> {
        let mut seen = std::collections::HashSet::new();
        action.depends_on.retain(|id| seen.insert(id.clone()));
        if action.depends_on.len() > MAX_DEPENDENCIES {
            return Err(ActionError::TooManyDependencies);
        }
        let policy = self.policy;
        self.storage
            .shared
            .write("pending_actions.stage", cancelled, wait, move |tx, _| {
                let tenant = write::authorize(tx, &credential, policy)?;
                insert(tx, tenant.id(), action)
            })?
    }

    /// Every action not applied or dismissed: the newest [`LIST_LIMIT`],
    /// oldest first.
    pub fn list(
        &self,
        credential: Credential,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Listing, ActionError> {
        let policy = self.policy;
        self.storage
            .read("pending_actions.list", cancelled, wait, move |tx| {
                let tenant = write::authorize(tx, &credential, policy)?;
                let now = now(tx)?;
                let open = "tenant_id=?1 AND state IN ('pending','applying','unknown','invalidated_by_restore')";
                let mut statement = tx.prepare(&format!(
                    "SELECT {COLUMNS} FROM (SELECT * FROM pending_actions WHERE {open}
                     ORDER BY seq DESC LIMIT ?2) ORDER BY seq"
                ))?;
                let mut rows = statement.query(params![tenant.id(), LIST_LIMIT as i64])?;
                let mut actions = Vec::new();
                while let Some(row) = rows.next()? {
                    actions.push(action(row, &now)?);
                }
                let total: i64 = tx.query_row(
                    &format!("SELECT count(*) FROM pending_actions WHERE {open}"),
                    [tenant.id()],
                    |row| row.get(0),
                )?;
                Ok(Listing {
                    omitted: (total as u64).saturating_sub(actions.len() as u64),
                    actions,
                })
            })?
    }

    /// Moves `id` from `pending` to `applying` for one apply. The write commits
    /// before this returns, and the returned [`Claim`] carries the attempt token
    /// required to settle it. Refused when it expired or a dependency is not
    /// applied yet; [`ActionError::Blocked`] names the action to apply first.
    pub fn claim(
        &self,
        credential: Credential,
        id: &str,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Claim, ActionError> {
        let (policy, id) = (self.policy, id.to_owned());
        self.storage
            .shared
            .write("pending_actions.claim", cancelled, wait, move |tx, _| {
                let tenant = write::authorize(tx, &credential, policy)?;
                let now = now(tx)?;
                let mut row = load(tx, tenant.id(), &id, &now)?.ok_or(ActionError::NotFound)?;
                if row.state != State::Pending {
                    return Err(refusal(row));
                }
                if let Some(first) = must_go_first(tx, tenant.id(), &row, &now)? {
                    return Err(ActionError::Blocked(first));
                }
                let mut secret = [0u8; 32];
                getrandom::fill(&mut secret).map_err(|_| ActionError::ClaimTokenUnavailable)?;
                let digest: [u8; 32] = Sha256::digest(secret).into();
                let claimed = tx.execute(
                    "UPDATE pending_actions SET state='applying', claimed_at=?1, claim_digest=?4
                     WHERE tenant_id=?2 AND id=?3 AND state='pending' AND expires_at > ?1",
                    params![now, tenant.id(), id, digest.as_slice()],
                )?;
                if claimed != 1 {
                    return Err(ActionError::InFlight);
                }
                row.state = State::Applying;
                Ok(Claim {
                    action: row,
                    token: ClaimToken { id, secret },
                })
            })?
    }

    /// Ends the apply a claim started. It acts on the claim token, not on a
    /// credential, so a key revoked during the apply cannot leave the row
    /// `applying`. Changes the row only when id, token digest, and `applying`
    /// all match. Returns `Ok(false)` and changes nothing otherwise.
    pub fn settle(
        &self,
        token: &ClaimToken,
        end: End,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<bool, ActionError> {
        let id = token.id.clone();
        let digest: [u8; 32] = Sha256::digest(token.secret).into();
        self.storage
            .shared
            .write("pending_actions.settle", cancelled, wait, move |tx, _| {
                let now = now(tx)?;
                let (state, settled_at) = match end {
                    End::Applied => ("applied", Some(now)),
                    End::Released => ("pending", None),
                    End::Unknown => ("unknown", Some(now)),
                };
                let changed = tx.execute(
                    "UPDATE pending_actions SET state=?1, settled_at=?2, claim_digest=NULL
                     WHERE id=?3 AND state='applying' AND claim_digest=?4",
                    params![state, settled_at, id, digest.as_slice()],
                )?;
                Ok(changed == 1)
            })?
    }

    pub fn dismiss(
        &self,
        credential: Credential,
        id: &str,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<(), ActionError> {
        let (policy, id) = (self.policy, id.to_owned());
        self.storage
            .shared
            .write("pending_actions.dismiss", cancelled, wait, move |tx, _| {
                let tenant = write::authorize(tx, &credential, policy)?;
                let now = now(tx)?;
                let row = load(tx, tenant.id(), &id, &now)?.ok_or(ActionError::NotFound)?;
                if !matches!(
                    row.state,
                    State::Pending | State::Expired | State::Unknown | State::InvalidatedByRestore
                ) {
                    return Err(refusal(row));
                }
                tx.execute(
                    "UPDATE pending_actions SET state='dismissed', settled_at=?1
                     WHERE tenant_id=?2 AND id=?3 AND state IN ('pending','unknown','invalidated_by_restore')",
                    params![now, tenant.id(), id],
                )?;
                Ok(())
            })?
    }
}

/// Every `applying` row becomes `unknown`. Called once from schema open,
/// after validation, because an apply a crash interrupted may have committed.
pub(crate) fn interrupt_applying(connection: &mut Connection) -> anyhow::Result<()> {
    let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let now = now(&tx)?;
    tx.execute(
        "UPDATE pending_actions SET state='unknown', settled_at=?1, claim_digest=NULL WHERE state='applying'",
        [now],
    )?;
    tx.commit()?;
    Ok(())
}

/// Moves every open row in a restored database to `invalidated_by_restore`
/// so restoring a backup never makes a previously settled or uncertain action
/// claimable again.
pub(crate) fn invalidate_after_restore(connection: &mut Connection) -> anyhow::Result<()> {
    let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let now = now(&tx)?;
    tx.execute(
        "UPDATE pending_actions SET state='invalidated_by_restore', settled_at=?1, claim_digest=NULL
         WHERE state IN ('pending','applying','unknown')",
        [now],
    )?;
    tx.commit()?;
    Ok(())
}

/// Whether an apply is between its claim and its settle. Restore refuses
/// then, so it cannot replace the database under that apply.
pub(crate) fn applying(connection: &Connection) -> rusqlite::Result<bool> {
    connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM pending_actions WHERE state='applying')",
        [],
        |row| row.get(0),
    )
}

fn now(tx: &Transaction<'_>) -> rusqlite::Result<String> {
    tx.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |row| {
        row.get(0)
    })
}

fn insert(
    tx: &Transaction<'_>,
    tenant: &str,
    action: NewAction,
) -> Result<PendingAction, ActionError> {
    let now = now(tx)?;
    for id in &action.depends_on {
        match load(tx, tenant, id, &now)? {
            None => return Err(dependency(id, None)),
            Some(row) if matches!(row.state, State::Pending | State::Applying | State::Applied) => {
            }
            Some(row) => return Err(dependency(id, Some(row.state))),
        }
    }
    let taken = tx
        .query_row(
            "SELECT 1 FROM pending_actions WHERE id=?1",
            [&action.id],
            |_| Ok(()),
        )
        .optional()?;
    if taken.is_some() {
        return Err(ActionError::IdTaken);
    }
    let to_json = |value: Value| value.to_string();
    tx.execute(
        "INSERT INTO pending_actions (tenant_id, id, action_type, plan_id, label, summary,
         params_json, depends_on_json, staged_by, state, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending', ?10,
         strftime('%Y-%m-%dT%H:%M:%fZ', ?10, ?11))",
        params![
            tenant,
            action.id,
            action.action_type,
            action.plan_id,
            action.label,
            action.summary,
            to_json(Value::Object(action.params)),
            to_json(action.depends_on.into()),
            action.staged_by.as_str(),
            now,
            format!("+{} seconds", TTL.as_secs()),
        ],
    )?;
    load(tx, tenant, &action.id, &now)?.ok_or(ActionError::NotFound)
}

fn dependency(id: &str, state: Option<State>) -> ActionError {
    ActionError::Dependency {
        id: id.into(),
        state,
    }
}

fn refusal(row: PendingAction) -> ActionError {
    let at = row.settled_at.unwrap_or_default();
    match row.state {
        State::Applied => ActionError::AlreadyApplied { at },
        State::Dismissed => ActionError::AlreadyDismissed { at },
        State::Applying => ActionError::InFlight,
        State::Unknown => ActionError::OutcomeUnknown,
        State::InvalidatedByRestore => ActionError::InvalidatedByRestore { at },
        State::Expired | State::Pending => ActionError::Expired {
            expires_at: row.expires_at,
        },
    }
}

fn load(
    tx: &Transaction<'_>,
    tenant: &str,
    id: &str,
    now: &str,
) -> Result<Option<PendingAction>, ActionError> {
    let mut statement = tx.prepare(&format!(
        "SELECT {COLUMNS} FROM pending_actions WHERE tenant_id=?1 AND id=?2"
    ))?;
    let mut rows = statement.query(params![tenant, id])?;
    rows.next()?.map(|row| action(row, now)).transpose()
}

/// Both stamps share one fixed-width UTC format, so text comparison orders them.
fn action(row: &Row<'_>, now: &str) -> Result<PendingAction, ActionError> {
    let id: String = row.get("id")?;
    let invalid = || ActionError::StoredRowInvalid { id: id.clone() };
    let expires_at: String = row.get("expires_at")?;
    let state = match State::parse(&row.get::<_, String>("state")?).ok_or_else(invalid)? {
        State::Pending if expires_at.as_str() <= now => State::Expired,
        state => state,
    };
    let params =
        serde_json::from_str(&row.get::<_, String>("params_json")?).map_err(|_| invalid())?;
    let depends_on =
        serde_json::from_str(&row.get::<_, String>("depends_on_json")?).map_err(|_| invalid())?;
    Ok(PendingAction {
        action_type: row.get("action_type")?,
        plan_id: row.get("plan_id")?,
        label: row.get("label")?,
        summary: row.get("summary")?,
        params,
        depends_on,
        staged_by: row.get("staged_by")?,
        state,
        created_at: row.get("created_at")?,
        expires_at,
        settled_at: row.get("settled_at")?,
        id,
    })
}

fn must_go_first(
    tx: &Transaction<'_>,
    tenant: &str,
    action: &PendingAction,
    now: &str,
) -> Result<Option<Blocker>, ActionError> {
    let mut reads = 0;
    walk(tx, tenant, &action.id, &action.depends_on, now, &mut reads)
}

fn walk(
    tx: &Transaction<'_>,
    tenant: &str,
    origin: &str,
    depends_on: &[String],
    now: &str,
    reads: &mut usize,
) -> Result<Option<Blocker>, ActionError> {
    let corrupt = || ActionError::StoredRowInvalid { id: origin.into() };
    for id in depends_on {
        *reads += 1;
        if *reads > MAX_WALK {
            return Err(corrupt());
        }
        let dependency = load(tx, tenant, id, now)?.ok_or_else(corrupt)?;
        let blocker = || Blocker {
            id: dependency.id.clone(),
            label: dependency.label.clone(),
            state: dependency.state,
        };
        match dependency.state {
            State::Applied => continue,
            State::Pending => {
                let deeper = walk(tx, tenant, origin, &dependency.depends_on, now, reads)?;
                return Ok(Some(deeper.unwrap_or_else(blocker)));
            }
            _ => return Ok(Some(blocker())),
        }
    }
    Ok(None)
}

#[cfg(test)]
#[path = "pending_actions/tests.rs"]
mod tests;
