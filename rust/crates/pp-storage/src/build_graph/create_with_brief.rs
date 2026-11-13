//! Create a Build together with its planning brief (GRE-1441), the operation
//! behind MCP `propose_create_build`.
//!
//! [`BuildGraphClient::create_with_brief`] runs in one write transaction on
//! the write seam: prove the credential, check the Orders Module, answer a
//! recorded idempotency key, refuse a taken name, then insert the Build with
//! its special request, store a new brief and record the key.
//! [`BuildGraphClient::check_create_with_brief`] makes the same checks in a
//! read transaction and writes nothing.
//!
//! Errors are [`CreateBuildError`]. This module holds no status codes and no
//! response text; `pp-api` maps the variants.

use super::{BuildGraphClient, BuildName, header};
use crate::{
    WriterFailure, auth,
    features::{self, InvalidLayer},
    read_model::{Credential, planning::BuildPlanningBrief},
    write::{Installation, authorize},
};
use pp_domain::{PositiveId, build_identity::trim_js, features::Feature};
use rusqlite::{OptionalExtension, Transaction, params};
use std::{sync::atomic::AtomicBool, time::Duration};

/// The longest idempotency key, in characters, after trimming.
pub const MAX_IDEMPOTENCY_KEY_CHARS: usize = 200;

/// A checked request to create a Build with its brief.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewBuild {
    name: BuildName,
    request: String,
    idempotency_key: Option<String>,
}

/// Why a create request is refused before it reaches storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidNewBuild {
    BlankName,
    NameTooLong,
    BlankRequest,
    /// Longer than [`MAX_IDEMPOTENCY_KEY_CHARS`] after trimming.
    IdempotencyKeyTooLong,
}

impl NewBuild {
    /// Trims `name`, normalises `request` as PATCH /plans/{id} stores a
    /// special request, and trims `idempotency_key`. A key that is blank
    /// after trimming means no key.
    pub fn parse(
        name: &str,
        request: &str,
        idempotency_key: Option<&str>,
    ) -> Result<Self, InvalidNewBuild> {
        if trim_js(name).is_empty() {
            return Err(InvalidNewBuild::BlankName);
        }
        let name = BuildName::parse(name.to_owned()).map_err(|_| InvalidNewBuild::NameTooLong)?;
        let request = header::normalized_special_request(request)
            .ok_or(InvalidNewBuild::BlankRequest)?
            .to_owned();
        let idempotency_key = idempotency_key
            .map(trim_js)
            .filter(|key| !key.is_empty())
            .map(str::to_owned);
        if idempotency_key
            .as_ref()
            .is_some_and(|key| key.chars().count() > MAX_IDEMPOTENCY_KEY_CHARS)
        {
            return Err(InvalidNewBuild::IdempotencyKeyTooLong);
        }
        Ok(Self {
            name,
            request,
            idempotency_key,
        })
    }

    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    pub fn request(&self) -> &str {
        &self.request
    }

    pub fn idempotency_key(&self) -> Option<&str> {
        self.idempotency_key.as_deref()
    }
}

/// The Build a create made, or the one its idempotency key recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedBuild {
    pub id: PositiveId,
    pub name: String,
    /// The key named an existing Build, so nothing was written.
    pub replayed: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum CreateBuildError {
    #[error("another Build has this name")]
    NameTaken,
    #[error("the Orders Module is off")]
    OrdersOff,
    /// The stored Module settings do not parse, so the Orders state is unknown.
    #[error("the stored Module settings are invalid")]
    ModuleSettingsInvalid,
    /// The setting for this idempotency key holds something other than a
    /// Build id. Nothing is written until it is repaired.
    #[error("the stored idempotency record is invalid")]
    IdempotencyRecordInvalid,
    #[error(transparent)]
    Writer(#[from] WriterFailure),
}

impl From<rusqlite::Error> for CreateBuildError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Writer(error.into())
    }
}

impl From<anyhow::Error> for CreateBuildError {
    fn from(error: anyhow::Error) -> Self {
        Self::Writer(error.into())
    }
}

impl BuildGraphClient {
    /// Refuses what [`Self::create_with_brief`] would refuse now, in a read
    /// transaction. `Ok` is no promise: confirm checks again.
    pub fn check_create_with_brief(
        &self,
        credential: Credential,
        new: &NewBuild,
        wait: Duration,
    ) -> Result<(), CreateBuildError> {
        // A reader that cannot be checked out (pool full, storage stopping)
        // is busy storage, not a failure.
        let mut reader = self
            .settings
            .reader(wait)
            .map_err(|_| WriterFailure::Busy)?;
        let tx = reader
            .connection
            .as_mut()
            .ok_or(WriterFailure::Stopped)?
            .transaction()?;
        let installation = authorize(&tx, &credential, self.policy)?;
        decide(&tx, &installation, new)?;
        Ok(())
    }

    /// Creates the Build, sets its special request, stores its new brief and
    /// records the idempotency key, in one transaction. A key that names an
    /// existing Build answers that Build and writes nothing.
    pub fn create_with_brief(
        &self,
        credential: Credential,
        new: NewBuild,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<CreatedBuild, CreateBuildError> {
        let policy = self.policy;
        let name = "build_graph.create_with_brief";
        self.shared.write(name, cancelled, wait, move |tx, _| {
            let installation = authorize(tx, &credential, policy)?;
            if let Some(replay) = decide(tx, &installation, &new)? {
                return Ok(replay);
            }
            let tenant = installation.id();
            let now = auth::catalog_timestamp();
            tx.execute(
                "INSERT INTO build_profiles(tenant_id,name,special_request,config_modified_at,last_used_at) VALUES(?1,?2,?3,?4,?4)",
                params![tenant, new.name(), new.request(), now],
            )?;
            let row = tx.last_insert_rowid();
            let id = u64::try_from(row)
                .ok()
                .and_then(|id| PositiveId::new(id).ok())
                .ok_or_else(|| anyhow::anyhow!("new Build id {row} is not positive"))?;
            let brief = BuildPlanningBrief::new(row, new.request.clone(), now);
            save(tx, tenant, &brief_key(id), &brief.stored_text()?)?;
            if let Some(key) = new.idempotency_key() {
                save(tx, tenant, &record_key(key), &id.get().to_string())?;
            }
            Ok(CreatedBuild {
                id,
                name: new.name().to_owned(),
                replayed: false,
            })
        })?
    }
}

/// The checks create and its dry run share, in order: Orders, the recorded
/// key, then the name. `Some` is the Build the key recorded.
fn decide(
    tx: &Transaction<'_>,
    installation: &Installation,
    new: &NewBuild,
) -> Result<Option<CreatedBuild>, CreateBuildError> {
    let tenant = installation.id();
    let view = match features::resolve_for_tenant(tx, tenant, None) {
        Ok(view) => view,
        Err(error) if error.is::<InvalidLayer>() => {
            return Err(CreateBuildError::ModuleSettingsInvalid);
        }
        Err(error) => return Err(error.into()),
    };
    if !view.effective().enabled(Feature::Orders) {
        return Err(CreateBuildError::OrdersOff);
    }
    if let Some(key) = new.idempotency_key()
        && let Some(replay) = recorded(tx, tenant, key)?
    {
        return Ok(Some(replay));
    }
    let taken: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM build_profiles WHERE tenant_id=?1 AND name=?2)",
        params![tenant, new.name()],
        |row| row.get(0),
    )?;
    if taken {
        return Err(CreateBuildError::NameTaken);
    }
    Ok(None)
}

/// The Build `key` recorded, when it still exists. A Build deleted since
/// leaves the key free for a new one.
fn recorded(
    tx: &Transaction<'_>,
    tenant: &str,
    key: &str,
) -> Result<Option<CreatedBuild>, CreateBuildError> {
    let stored: Option<String> = tx
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
            params![tenant, record_key(key)],
            |row| row.get(0),
        )
        .optional()?;
    let Some(stored) = stored else {
        return Ok(None);
    };
    let id = stored
        .parse::<u64>()
        .ok()
        .and_then(|id| PositiveId::new(id).ok())
        .ok_or(CreateBuildError::IdempotencyRecordInvalid)?;
    let name: Option<String> = tx
        .query_row(
            "SELECT name FROM build_profiles WHERE tenant_id=?1 AND id=?2",
            params![tenant, id.get() as i64],
            |row| row.get(0),
        )
        .optional()?;
    Ok(name.map(|name| CreatedBuild {
        id,
        name,
        replayed: true,
    }))
}

pub(super) fn brief_key(build: PositiveId) -> String {
    format!("build_planning.v1.{}", build.get())
}

fn record_key(key: &str) -> String {
    format!("build_planning.create.{key}")
}

pub(super) fn save(
    tx: &Transaction<'_>,
    tenant: &str,
    key: &str,
    value: &str,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO app_settings(tenant_id,key,value) VALUES(?1,?2,?3) ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value",
        params![tenant, key, value],
    )?;
    Ok(())
}

/// The two ways a Build's stored planning brief is unusable, as each brief
/// writer's own error enum names them.
pub(super) trait BriefError: From<rusqlite::Error> {
    fn no_brief() -> Self;
    /// The stored brief does not parse or names another Build.
    fn invalid() -> Self;
}

/// `build`'s stored planning brief, read in `tx`.
pub(super) fn stored_brief<E: BriefError>(
    tx: &Transaction<'_>,
    tenant: &str,
    build: PositiveId,
) -> Result<BuildPlanningBrief, E> {
    let raw: Option<String> = tx
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
            params![tenant, brief_key(build)],
            |row| row.get(0),
        )
        .optional()?;
    let raw = raw.ok_or_else(E::no_brief)?;
    let id = i64::try_from(build.get()).map_err(|_| E::invalid())?;
    BuildPlanningBrief::parse(&raw, id).map_err(|_| E::invalid())
}

/// Shared with the brief writers' tests, which start from a created brief.
#[cfg(test)]
pub(super) mod tests;
