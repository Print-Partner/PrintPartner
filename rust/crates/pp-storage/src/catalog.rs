#[cfg(feature = "desktop_rebuild")]
mod activity;
#[cfg(feature = "desktop_rebuild")]
mod location;
#[cfg(feature = "desktop_rebuild")]
mod rebuild;
#[cfg(feature = "desktop_rebuild")]
pub use activity::SourceActivityEvent;
#[cfg(feature = "desktop_rebuild")]
pub use rebuild::{
    BulkCategoryResult, CatalogAccess, CatalogFailure, CatalogKeyAccess, CategoryNode,
    CategorySettings, IMPORT_RULE_BODY_LIMIT, NumericCategoryCommand,
};

mod categories;
mod json;
pub(crate) mod naming;

use crate::{Envelope, SettingsClient, WriterOwner, auth};
use anyhow::{Result, anyhow, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

pub enum Credentials {
    Session(auth::Secret),
    Key {
        tenant_id: String,
        key: auth::Secret,
    },
}
pub(super) enum Authority {
    Credentials(Credentials, auth::AuthPolicy),
    LocalOwner,
}
pub struct SourceCatalogClient {
    client: SettingsClient,
    authority: Authority,
}
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSource {
    pub name: String,
    pub url: Option<String>,
    pub branch: Option<String>,
    pub tag: Option<String>,
    pub source_kind: Option<String>,
    pub source_type: Option<String>,
    pub role: Option<String>,
    pub metadata: Option<Map<String, Value>>,
}
#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourcePatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub tag: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
    /// Caller-observed catalog metadata version. Omitted requests overwrite.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_version: Option<i64>,
}
fn nullable<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}
#[derive(Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    List {},
    #[cfg(feature = "desktop_rebuild")]
    SourceActivity {
        limit: u8,
    },
    #[cfg(feature = "desktop_rebuild")]
    GetCategoriesWithTree {},
    #[cfg(feature = "desktop_rebuild")]
    SaveCategoriesWithTree {
        categories: Vec<String>,
        replacements: HashMap<String, Option<String>>,
    },
    #[cfg(feature = "desktop_rebuild")]
    CreateCatalogSource {
        source: CreateSource,
    },
    #[cfg(feature = "desktop_rebuild")]
    UpdateCatalogSource {
        id: i64,
        patch: SourcePatch,
    },
    #[cfg(feature = "desktop_rebuild")]
    ApplyNumericCategory {
        command: NumericCategoryCommand,
    },

    Get {
        id: i64,
    },
    Create {
        source: CreateSource,
    },
    Update {
        id: i64,
        patch: SourcePatch,
    },
    Delete {
        id: i64,
    },
    GetImportRules {
        id: i64,
    },
    SaveImportRules {
        id: i64,
        rules: Vec<String>,
    },
    BulkCategory {
        source_ids: Vec<i64>,
        category: Option<String>,
    },
    GetCategories {},
    GetCategoryTree {},
    SaveCategories {
        categories: Vec<String>,
        #[serde(default)]
        replacements: HashMap<String, Option<String>>,
    },
    GetNaming {
        id: i64,
    },
    SaveNaming {
        id: i64,
        settings: naming::NamingCommand,
    },
    #[cfg(feature = "desktop_rebuild")]
    PreviewNaming {
        relative_path: String,
        profile: Option<NamingOverride>,
    },
    GetGlobalNaming {},
    SaveGlobalNaming {
        profile: naming::NamingProfile,
    },
}
pub use naming::{
    FolderRule, FunctionalClass, NamingCommand, NamingProfile, Quantity, Role, RoleId, Slug,
};
#[cfg(feature = "desktop_rebuild")]
pub use naming::{NamingOverride, NamingPreview, QuantityOverride, RoleOverride, SlugOverride};
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct SourceSummary {
    pub id: i64,
    pub name: String,
    pub url: String,
    pub source_kind: String,
    pub source_type: String,
    pub role: String,
    pub category: Option<String>,
    pub branch: String,
    pub tag: Option<String>,
    pub local_path: Option<String>,
    pub content_available: bool,
    pub last_synced_at: Option<String>,
    pub last_commit_sha: Option<String>,
    pub current_source_revision_id: Option<i64>,
    pub docs_url: Option<String>,
    pub manifest_community_slug: Option<String>,
    pub metadata: Option<Map<String, Value>>,
    pub naming_use_defaults: bool,
    pub update_status: Option<String>,
    pub update_checked_at: Option<String>,
    pub doc_count: i64,
    /// Present only while the desktop rebuild runtime flag is on.
    #[cfg(feature = "desktop_rebuild")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_metadata_version: Option<i64>,
}
#[derive(Debug, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Deletion {
    Deleted { source_id: i64 },
    NotFound,
    RetainedHistory,
    Referenced,
    ActiveWork,
}
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Outcome {
    #[cfg(feature = "desktop_rebuild")]
    Preview(NamingPreview),
    #[cfg(feature = "desktop_rebuild")]
    Activity(Vec<SourceActivityEvent>),
    #[cfg(feature = "desktop_rebuild")]
    Categories(CategorySettings),
    #[cfg(feature = "desktop_rebuild")]
    BulkCategory(BulkCategoryResult),
    Source(Option<Box<SourceSummary>>),
    Sources(Vec<SourceSummary>),
    Deletion(Deletion),
    Data(Value),
}
pub type CatalogReply = mpsc::Receiver<Result<Outcome>>;
pub(super) enum Command {
    Run {
        authority: Authority,
        request: Box<Request>,
    },
    Begin {
        authority: Authority,
        id: i64,
    },
    BeginJob {
        lease: crate::jobs::AttemptLease,
        source_id: Option<i64>,
    },
    End(u64),
}
pub(super) enum Reply {
    Outcome(Outcome),
    Lease(SourceWorkProof),
    Released,
}
/// Another live lease already holds work on this Source.
#[cfg(feature = "desktop_rebuild")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceBusy;
#[cfg(feature = "desktop_rebuild")]
impl std::fmt::Display for SourceBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Source work is already active")
    }
}
#[cfg(feature = "desktop_rebuild")]
impl std::error::Error for SourceBusy {}
pub(super) struct State {
    /// Distinguishes this writer's tokens from those of an earlier writer.
    #[cfg(feature = "desktop_rebuild")]
    incarnation: String,
    issued_high_water: u64,
    active: HashMap<u64, SourceBinding>,
    pub directory: PathBuf,
}
impl Default for State {
    fn default() -> Self {
        Self::new(PathBuf::new())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SourceBinding {
    pub(crate) tenant: String,
    pub(crate) id: i64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SourceWorkProof {
    #[cfg(feature = "desktop_rebuild")]
    pub(crate) incarnation: String,
    pub(crate) token: u64,
}
#[cfg(feature = "desktop_rebuild")]
pub(crate) enum IssuedTokenState<'a> {
    Active(&'a SourceBinding),
    Retired,
    NeverIssued,
    WrongIncarnation,
}
#[cfg(feature = "desktop_rebuild")]
pub(crate) enum Retirement {
    RetiredNow,
    AlreadyRetired,
}
impl State {
    pub(super) fn new(directory: PathBuf) -> Self {
        Self {
            #[cfg(feature = "desktop_rebuild")]
            incarnation: loop {
                let bytes = rand::random::<[u8; 32]>();
                if bytes.iter().any(|byte| *byte != 0) {
                    break hex::encode(bytes);
                }
            },
            issued_high_water: 0,
            active: HashMap::new(),
            directory,
        }
    }
    pub(crate) fn source_busy(&self, tenant: &str, id: i64) -> bool {
        self.active
            .values()
            .any(|binding| binding.tenant == tenant && binding.id == id)
    }
    #[cfg(feature = "desktop_rebuild")]
    pub(crate) fn incarnation(&self) -> &str {
        &self.incarnation
    }
    pub(crate) fn next_token(&self) -> Result<u64> {
        self.issued_high_water
            .checked_add(1)
            .ok_or_else(|| anyhow!("Work lease overflow"))
    }
    pub(crate) fn activate(&mut self, token: u64, tenant: String, id: i64) {
        debug_assert_eq!(token, self.issued_high_water + 1);
        self.issued_high_water = token;
        self.active.insert(token, SourceBinding { tenant, id });
    }
    /// Drops the bindings of leases whose release could not be queued.
    #[cfg(feature = "desktop_rebuild")]
    pub(crate) fn reap(&mut self, tokens: Vec<u64>) {
        for token in tokens {
            self.active.remove(&token);
        }
    }
    pub(crate) fn proof(&self, token: u64) -> Result<SourceWorkProof> {
        ensure!(self.active.contains_key(&token), "Unknown work lease");
        Ok(SourceWorkProof {
            #[cfg(feature = "desktop_rebuild")]
            incarnation: self.incarnation.clone(),
            token,
        })
    }
    #[cfg(feature = "desktop_rebuild")]
    pub(crate) fn classify_issued(&self, incarnation: &str, token: u64) -> IssuedTokenState<'_> {
        if incarnation != self.incarnation {
            return IssuedTokenState::WrongIncarnation;
        }
        if let Some(binding) = self.active.get(&token) {
            return IssuedTokenState::Active(binding);
        }
        if token > 0 && token <= self.issued_high_water {
            IssuedTokenState::Retired
        } else {
            IssuedTokenState::NeverIssued
        }
    }
    #[cfg(feature = "desktop_rebuild")]
    pub(crate) fn retire_issued(
        &mut self,
        proof: &SourceWorkProof,
        expected: &SourceBinding,
    ) -> Result<Retirement> {
        match self.classify_issued(&proof.incarnation, proof.token) {
            IssuedTokenState::Active(actual) => {
                ensure!(actual == expected, "Source reservation binding conflict");
                self.active.remove(&proof.token);
                Ok(Retirement::RetiredNow)
            }
            IssuedTokenState::Retired => Ok(Retirement::AlreadyRetired),
            IssuedTokenState::NeverIssued => Err(anyhow!("Never-issued Source reservation")),
            IssuedTokenState::WrongIncarnation => Err(anyhow!("Wrong Source writer incarnation")),
        }
    }
}
pub struct SourceWorkLease {
    client: SettingsClient,
    proof: Option<SourceWorkProof>,
}
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn lease(client: SettingsClient, proof: SourceWorkProof) -> SourceWorkLease {
    SourceWorkLease {
        client,
        proof: Some(proof),
    }
}
impl WriterOwner {
    pub fn source_catalog(&self, credentials: Credentials) -> SourceCatalogClient {
        SourceCatalogClient {
            client: self.client(),
            authority: Authority::Credentials(
                credentials,
                auth::AuthPolicy {
                    registration: auth::RegistrationPolicy::Open,
                    session_tenant: auth::SessionTenantPolicy::AccountTenant,
                    first_user: auth::FirstUserTenant::NewUser,
                },
            ),
        }
    }
    pub fn source_catalog_with_policy(
        &self,
        credentials: Credentials,
        policy: auth::AuthPolicy,
    ) -> Result<SourceCatalogClient> {
        auth::validate_policy(policy)?;
        Ok(SourceCatalogClient {
            client: self.client(),
            authority: Authority::Credentials(credentials, policy),
        })
    }
    pub fn local_source_catalog(&self) -> SourceCatalogClient {
        SourceCatalogClient {
            client: self.client(),
            authority: Authority::LocalOwner,
        }
    }
}
impl Authority {
    fn duplicate(&self) -> Self {
        match self {
            Self::LocalOwner => Self::LocalOwner,
            Self::Credentials(Credentials::Session(s), policy) => Self::Credentials(
                Credentials::Session(auth::Secret::new(s.expose().to_owned())),
                *policy,
            ),
            Self::Credentials(Credentials::Key { tenant_id, key }, policy) => Self::Credentials(
                Credentials::Key {
                    tenant_id: tenant_id.clone(),
                    key: auth::Secret::new(key.expose().to_owned()),
                },
                *policy,
            ),
        }
    }
    fn tenant(self, tx: &Transaction<'_>) -> Result<String> {
        match self {
            Self::LocalOwner => Ok("default".into()),
            Self::Credentials(c, policy) => auth::catalog_tenant(tx, c, policy),
        }
    }
}
fn enqueue(
    client: &SettingsClient,
    command: Command,
    cancelled: &AtomicBool,
    wait: Duration,
) -> Result<mpsc::Receiver<Result<Reply>>> {
    let deadline = Instant::now() + wait;
    let mut q = client
        .shared
        .queue
        .lock()
        .map_err(|_| anyhow!("Writer admission poisoned"))?;
    loop {
        ensure!(!q.closed, "Storage stopped");
        ensure!(
            !cancelled.load(Ordering::Acquire),
            "Cancelled before admission"
        );
        if q.pending.len() < client.shared.capacity {
            let (reply, rx) = mpsc::channel();
            q.pending.push_back(Envelope::Catalog { command, reply });
            client.shared.changed.notify_all();
            return Ok(rx);
        }
        ensure!(Instant::now() < deadline, "Writer queue full");
        q = client
            .shared
            .changed
            .wait_timeout(q, Duration::from_millis(5))
            .map_err(|_| anyhow!("Writer admission poisoned"))?
            .0;
    }
}
impl SourceCatalogClient {
    pub fn submit(
        &self,
        request: Request,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<CatalogReply> {
        #[cfg(feature = "desktop_rebuild")]
        rebuild::request_size(&request)?;
        #[cfg(not(feature = "desktop_rebuild"))]
        ensure!(
            serde_json::to_vec(&request)?.len() <= 1024 * 1024,
            "Catalog request too large"
        );
        let (reply, rx) = mpsc::channel();
        let command = Command::Run {
            authority: self.authority.duplicate(),
            request: Box::new(request),
        };
        let deadline = Instant::now() + wait;
        let mut q = self
            .client
            .shared
            .queue
            .lock()
            .map_err(|_| anyhow!("Writer admission poisoned"))?;
        loop {
            ensure!(!q.closed, "Storage stopped");
            ensure!(
                !cancelled.load(Ordering::Acquire),
                "Cancelled before admission"
            );
            if q.pending.len() < self.client.shared.capacity {
                q.pending
                    .push_back(Envelope::CatalogRequest { command, reply });
                self.client.shared.changed.notify_all();
                return Ok(rx);
            }
            ensure!(Instant::now() < deadline, "Writer queue full");
            q = self
                .client
                .shared
                .changed
                .wait_timeout(q, Duration::from_millis(5))
                .map_err(|_| anyhow!("Writer admission poisoned"))?
                .0;
        }
    }
    pub fn execute(&self, request: Request) -> Result<Outcome> {
        self.submit(request, &AtomicBool::new(false), Duration::from_secs(5))?
            .recv()
            .map_err(|_| anyhow!("Writer stopped without catalog result"))?
    }
    pub fn begin_work(
        &self,
        id: i64,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<SourceWorkLease> {
        let reply = enqueue(
            &self.client,
            Command::Begin {
                authority: self.authority.duplicate(),
                id,
            },
            cancelled,
            wait,
        )?
        .recv()??;
        match reply {
            Reply::Lease(proof) => Ok(SourceWorkLease {
                client: self.client.clone(),
                proof: Some(proof),
            }),
            _ => Err(anyhow!("Unexpected lease reply")),
        }
    }
}
pub(crate) fn begin_job_work(
    client: &SettingsClient,
    lease: crate::jobs::AttemptLease,
    source_id: Option<i64>,
    cancelled: &AtomicBool,
    wait: Duration,
) -> Result<SourceWorkLease> {
    match enqueue(
        client,
        Command::BeginJob { lease, source_id },
        cancelled,
        wait,
    )?
    .recv()??
    {
        Reply::Lease(proof) => Ok(SourceWorkLease {
            client: client.clone(),
            proof: Some(proof),
        }),
        _ => Err(anyhow!("Unexpected lease reply")),
    }
}
impl SourceWorkLease {
    #[cfg(feature = "desktop_rebuild")]
    pub(crate) fn proof(&self) -> Result<SourceWorkProof> {
        self.proof
            .clone()
            .ok_or_else(|| anyhow!("Lease already released"))
    }
    /// Forgets a token the writer already retired, so dropping the lease
    /// sends no release.
    #[cfg(feature = "desktop_rebuild")]
    pub(crate) fn consume_retired(&mut self) -> Result<()> {
        ensure!(self.proof.take().is_some(), "Lease already released");
        Ok(())
    }
    pub fn release(&mut self) -> Result<()> {
        let token = self
            .proof
            .as_ref()
            .map(|proof| proof.token)
            .ok_or_else(|| anyhow!("Lease already released"))?;
        let reply = enqueue(
            &self.client,
            Command::End(token),
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?;
        #[cfg(not(feature = "desktop_rebuild"))]
        {
            self.proof = None;
        }
        reply.recv()??;
        #[cfg(feature = "desktop_rebuild")]
        {
            self.proof = None;
        }
        Ok(())
    }
}
impl Drop for SourceWorkLease {
    fn drop(&mut self) {
        if let Some(token) = self.proof.take().map(|proof| proof.token) {
            let queued = enqueue(
                &self.client,
                Command::End(token),
                &AtomicBool::new(false),
                Duration::ZERO,
            );
            #[cfg(feature = "desktop_rebuild")]
            if queued.is_err() {
                self.client
                    .shared
                    .orphaned_source_leases
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(token);
                self.client.shared.changed.notify_all();
            }
            #[cfg(not(feature = "desktop_rebuild"))]
            drop(queued);
        }
    }
}
pub(super) fn execute(
    connection: &mut Connection,
    state: &mut State,
    command: Command,
    admission: crate::writer_admission::Deadline,
) -> Result<Reply> {
    #[cfg(feature = "desktop_rebuild")]
    if let Command::End(token) = command {
        match state.classify_issued(&state.incarnation, token) {
            IssuedTokenState::Active(_) => {
                state.active.remove(&token);
            }
            IssuedTokenState::Retired => {}
            IssuedTokenState::NeverIssued | IssuedTokenState::WrongIncarnation => {
                return Err(anyhow!("Unknown work lease"));
            }
        }
        return Ok(Reply::Released);
    }
    let tx = crate::writer_admission::begin_immediate(connection, admission)?;
    match command {
        Command::End(token) => {
            ensure!(state.active.remove(&token).is_some(), "Unknown work lease");
            tx.commit()?;
            Ok(Reply::Released)
        }
        Command::BeginJob { lease, source_id } => {
            let (tenant, id) = crate::jobs::claimed_source(&tx, &lease, source_id)?;
            require(&tx, &tenant, id)?;
            #[cfg(feature = "desktop_rebuild")]
            ensure!(!state.source_busy(&tenant, id), SourceBusy);
            let token = state.next_token()?;
            tx.commit()?;
            state.activate(token, tenant, id);
            Ok(Reply::Lease(state.proof(token)?))
        }
        Command::Begin { authority, id } => {
            let tenant = authority.tenant(&tx)?;
            require(&tx, &tenant, id)?;
            #[cfg(feature = "desktop_rebuild")]
            ensure!(!state.source_busy(&tenant, id), SourceBusy);
            let token = state.next_token()?;
            tx.commit()?;
            state.activate(token, tenant, id);
            Ok(Reply::Lease(state.proof(token)?))
        }
        Command::Run { authority, request } => {
            let tenant = authority.tenant(&tx)?;
            let outcome = run(&tx, state, &tenant, *request)?;
            tx.commit()?;
            Ok(Reply::Outcome(outcome))
        }
    }
}
fn js_space(c: char) -> bool {
    matches!(c,'\u{0009}'..='\u{000d}'|' '|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')
}
fn trim(s: &str) -> &str {
    s.trim_matches(js_space)
}
#[cfg(feature = "desktop_rebuild")]
const CATALOG_METADATA_VERSION: &str = "catalog_metadata_version";

fn metadata(raw: Option<String>) -> Option<Map<String, Value>> {
    raw.filter(|s| !s.is_empty())
        .map(|s| match serde_json::from_str::<Value>(&s) {
            Ok(Value::Object(m)) => m,
            _ => Map::from_iter([("raw".into(), Value::String(s))]),
        })
}

#[cfg(feature = "desktop_rebuild")]
fn metadata_guard_enabled() -> bool {
    matches!(
        std::env::var("PP_DESKTOP_REBUILD").ok().as_deref(),
        Some("1" | "true")
    )
}

#[cfg(feature = "desktop_rebuild")]
fn catalog_metadata_version_of(map: &Map<String, Value>) -> i64 {
    match map.get(CATALOG_METADATA_VERSION) {
        Some(Value::Number(number)) => number.as_i64().filter(|value| *value >= 0).unwrap_or(0),
        _ => 0,
    }
}

#[cfg(feature = "desktop_rebuild")]
fn publish_catalog_metadata_version(metadata: &mut Option<Map<String, Value>>) -> Option<i64> {
    if !metadata_guard_enabled() {
        return None;
    }
    let version = metadata
        .as_ref()
        .map(catalog_metadata_version_of)
        .unwrap_or(0);
    if let Some(map) = metadata.as_mut() {
        map.shift_remove(CATALOG_METADATA_VERSION);
    }
    Some(version)
}

#[cfg(feature = "desktop_rebuild")]
fn stamp_catalog_metadata_version(map: &mut Map<String, Value>, current: i64) -> Result<()> {
    map.shift_remove(CATALOG_METADATA_VERSION);
    let next = current.checked_add(1).ok_or_else(|| {
        anyhow!(CatalogFailure::Input(
            "Catalog metadata version overflow".into()
        ))
    })?;
    map.insert(CATALOG_METADATA_VERSION.into(), json!(next));
    Ok(())
}

#[cfg(feature = "desktop_rebuild")]
fn stored_catalog_metadata_version(tx: &Transaction<'_>, tenant: &str, id: i64) -> Result<i64> {
    let raw: Option<String> = tx.query_row(
        "SELECT metadata_json FROM projects WHERE tenant_id=?1 AND id=?2",
        params![tenant, id],
        |row| row.get(0),
    )?;
    Ok(metadata(raw)
        .as_ref()
        .map(catalog_metadata_version_of)
        .unwrap_or(0))
}
/// `categories::normalize` over WTF-8. Lone surrogates are not whitespace, so trimming stops at them.
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn normalize_category_path(category: &[u8]) -> Vec<u8> {
    category
        .split(|byte| *byte == b'/')
        .map(|segment| {
            let head = segment
                .utf8_chunks()
                .next()
                .map_or("", |chunk| chunk.valid());
            let segment = &segment[head.len() - head.trim_start_matches(js_space).len()..];
            let tail = segment
                .utf8_chunks()
                .last()
                .filter(|chunk| chunk.invalid().is_empty())
                .map_or("", |chunk| chunk.valid());
            &segment[..segment.len() - (tail.len() - tail.trim_end_matches(js_space).len())]
        })
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join(&b'/')
}
fn read_source(row: &rusqlite::Row<'_>) -> rusqlite::Result<SourceSummary> {
    let metadata = metadata(row.get(15)?);
    #[cfg(feature = "desktop_rebuild")]
    let (metadata, catalog_metadata_version) = {
        let mut metadata = metadata;
        let version = publish_catalog_metadata_version(&mut metadata);
        (metadata, version)
    };
    let m = metadata.clone().unwrap_or_default();
    let role: String = row
        .get::<_, Option<String>>(5)?
        .unwrap_or("unassigned".into());
    let category = categories::resolve(&m, &role);
    let source_type = row.get::<_, Option<String>>(4)?.unwrap_or("git".into());
    let kind = row
        .get::<_, Option<String>>(3)?
        .filter(|s| !s.is_empty())
        .unwrap_or(
            if source_type == "local" {
                "local"
            } else {
                "github"
            }
            .into(),
        );
    let local_path: Option<String> = row.get(8)?;
    let status = if m.get("sync_required") == Some(&Value::Bool(true))
        || m.get("sync_error").is_some_and(Value::is_string)
    {
        Some("unknown".into())
    } else {
        m.get("remote_update_status")
            .and_then(Value::as_str)
            .filter(|s| matches!(*s, "up_to_date" | "updates_available" | "unknown"))
            .map(str::to_owned)
    };
    Ok(SourceSummary {
        id: row.get(0)?,
        name: row.get(1)?,
        url: row.get(2)?,
        source_kind: kind,
        source_type,
        role,
        category,
        branch: row.get::<_, Option<String>>(6)?.unwrap_or("main".into()),
        tag: row.get(7)?,
        content_available: local_path.as_ref().is_some_and(|p| !p.is_empty()),
        local_path,
        last_synced_at: row.get(9)?,
        last_commit_sha: row.get(10)?,
        current_source_revision_id: row.get(11)?,
        docs_url: row.get(12)?,
        manifest_community_slug: row.get(13)?,
        doc_count: row.get(14)?,
        naming_use_defaults: naming::uses_defaults(&m),
        update_status: status,
        update_checked_at: m
            .get("remote_checked_at")
            .and_then(Value::as_str)
            .map(str::to_owned),
        metadata,
        #[cfg(feature = "desktop_rebuild")]
        catalog_metadata_version,
    })
}
const SELECT: &str = "SELECT p.id,p.name,p.url,p.source_kind,p.source_type,p.role,p.branch,p.tag,p.local_path,p.last_synced_at,p.last_commit_sha,p.current_source_revision_id,p.docs_url,p.manifest_community_slug,(SELECT count(*) FROM source_docs d WHERE d.project_id=p.id AND d.tenant_id=p.tenant_id),p.metadata_json FROM projects p";
pub(crate) fn get(tx: &Transaction<'_>, tenant: &str, id: i64) -> Result<Option<SourceSummary>> {
    Ok(tx
        .query_row(
            &format!("{SELECT} WHERE p.tenant_id=?1 AND p.id=?2"),
            params![tenant, id],
            read_source,
        )
        .optional()?)
}
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn validate_capture_existing(tx: &Transaction<'_>, tenant: &str, id: i64) -> Result<()> {
    require(tx, tenant, id).map(drop)
}
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn validate_capture_create(
    tx: &Transaction<'_>,
    tenant: &str,
    source: &mut CreateSource,
) -> Result<()> {
    *source = rebuild::create(tx, tenant, std::mem::take(source))?;
    Ok(())
}
fn require(tx: &Transaction<'_>, tenant: &str, id: i64) -> Result<SourceSummary> {
    get(tx, tenant, id)?.ok_or_else(|| anyhow!("Source not found"))
}
fn list(tx: &Transaction<'_>, tenant: &str) -> Result<Vec<SourceSummary>> {
    Ok(tx
        .prepare(&format!(
            "{SELECT} WHERE p.tenant_id=?1 ORDER BY p.name ASC"
        ))?
        .query_map([tenant], read_source)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}
fn save_metadata(tx: &Transaction<'_>, tenant: &str, id: i64, m: Map<String, Value>) -> Result<()> {
    #[cfg(feature = "desktop_rebuild")]
    let m = {
        let mut m = m;
        if metadata_guard_enabled() {
            let current = stored_catalog_metadata_version(tx, tenant, id)?;
            stamp_catalog_metadata_version(&mut m, current)?;
        }
        m
    };
    tx.execute(
        "UPDATE projects SET metadata_json=?1 WHERE tenant_id=?2 AND id=?3",
        params![json::metadata(&m)?, tenant, id],
    )?;
    Ok(())
}
fn patch(tx: &Transaction<'_>, tenant: &str, id: i64, p: SourcePatch) -> Result<SourceSummary> {
    let mut row = require(tx, tenant, id)?;
    #[cfg(feature = "desktop_rebuild")]
    if metadata_guard_enabled()
        && let Some(expected) = p.metadata_version
    {
        let current = row.catalog_metadata_version.unwrap_or(0);
        if expected != current {
            return Err(CatalogFailure::Conflict.into());
        }
    }
    let (source_kind, original_metadata): (String, Option<String>) = tx.query_row(
        "SELECT source_kind,metadata_json FROM projects WHERE tenant_id=?1 AND id=?2",
        params![tenant, id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    row.source_kind = source_kind;
    let identity = p.url.as_ref().is_some_and(|v| v != &row.url)
        || p.branch.as_ref().is_some_and(|v| v != &row.branch)
        || p.source_kind
            .as_ref()
            .is_some_and(|v| v != &row.source_kind)
        || p.source_type
            .as_ref()
            .is_some_and(|v| v != &row.source_type)
        || p.tag.as_ref().is_some_and(|v| {
            v.as_deref().map(trim).filter(|s| !s.is_empty()) != row.tag.as_deref()
        });
    if let Some(v) = p.name {
        let name = trim(&v);
        ensure!(!name.is_empty(), "Source name is required");
        ensure!(
            name == row.name || !name_referenced(tx, tenant, &row.name)?,
            "Source historical name is referenced and cannot be changed"
        );
        row.name = name.into();
    }
    if let Some(v) = p.url {
        row.url = v;
    }
    if let Some(v) = p.branch {
        row.branch = v;
    }
    if let Some(v) = p.tag {
        row.tag = v.map(|s| trim(&s).to_owned()).filter(|s| !s.is_empty());
    }
    if let Some(v) = p.source_kind {
        row.source_kind = v;
    }
    if let Some(v) = p.source_type {
        row.source_type = v;
    }
    if let Some(v) = p.role {
        row.role = v;
    }
    let metadata_changed = p.metadata.is_some() || identity;
    if let Some(m) = p.metadata {
        let base = row.metadata.get_or_insert_default();
        base.extend(m);
        if let Some(Value::String(c)) = base.get_mut("category") {
            *c = categories::normalize(c);
        }
    }
    if identity {
        let m = row.metadata.get_or_insert_default();
        m.insert("sync_required".into(), json!(true));
        m.insert("remote_update_status".into(), json!("unknown"));
        m.shift_remove("remote_checked_at");
        m.shift_remove("sync_error");
    }
    #[cfg(feature = "desktop_rebuild")]
    if metadata_changed && metadata_guard_enabled() {
        let current = row.catalog_metadata_version.unwrap_or(0);
        let map = row.metadata.get_or_insert_default();
        stamp_catalog_metadata_version(map, current)?;
    }
    tx.execute("UPDATE projects SET name=?1,url=?2,branch=?3,tag=?4,source_kind=?5,source_type=?6,role=?7,metadata_json=?8 WHERE tenant_id=?9 AND id=?10",params![row.name,row.url,row.branch,row.tag,row.source_kind,row.source_type,row.role,if metadata_changed {row.metadata.map(|m|json::metadata(&m)).transpose()?}else{original_metadata},tenant,id])?;
    require(tx, tenant, id)
}
fn rules(raw: Option<String>) -> (Vec<String>, bool) {
    let Some(raw) = raw else {
        return (vec![], true);
    };
    let parsed: Vec<Value> = serde_json::from_str(&raw).unwrap_or_default();
    (
        parsed
            .iter()
            .filter_map(Value::as_str)
            .filter(|s| !trim(s).is_empty())
            .map(normalize_rule)
            .collect(),
        false,
    )
}
fn normalize_rule(s: &str) -> String {
    let s = s.replace('\\', "/");
    let s = trim(&s).trim_start_matches('/');
    if s.is_empty() || s.ends_with('/') || s.to_lowercase().ends_with(".stl") {
        s.into()
    } else {
        format!("{s}/")
    }
}
pub(crate) fn run(
    tx: &Transaction<'_>,
    state: &State,
    tenant: &str,
    request: Request,
) -> Result<Outcome> {
    Ok(match request {
        #[cfg(feature = "desktop_rebuild")]
        Request::SourceActivity { limit } => Outcome::Activity(activity::list(tx, tenant, limit)?),
        #[cfg(feature = "desktop_rebuild")]
        Request::GetCategoriesWithTree {} => {
            Outcome::Categories(CategorySettings::new(categories::load(tx, tenant)?))
        }
        #[cfg(feature = "desktop_rebuild")]
        Request::SaveCategoriesWithTree {
            categories,
            replacements,
        } => Outcome::Categories(CategorySettings::new(categories::save(
            tx,
            tenant,
            categories,
            replacements,
        )?)),
        #[cfg(feature = "desktop_rebuild")]
        Request::CreateCatalogSource { source } => {
            let source = rebuild::create(tx, tenant, source)?;
            Outcome::Source(Some(Box::new(create_source(tx, state, tenant, source)?)))
        }
        #[cfg(feature = "desktop_rebuild")]
        Request::UpdateCatalogSource { id, patch: p } => {
            let p = rebuild::update(tx, tenant, id, p)?;
            Outcome::Source(Some(Box::new(patch(tx, tenant, id, p)?)))
        }
        #[cfg(feature = "desktop_rebuild")]
        Request::ApplyNumericCategory { command } => {
            Outcome::BulkCategory(rebuild::apply_numeric_category(tx, tenant, command)?)
        }
        Request::List {} => Outcome::Sources(list(tx, tenant)?),
        Request::Get { id } => Outcome::Source(get(tx, tenant, id)?.map(Box::new)),
        Request::Create { source: s } => {
            Outcome::Source(Some(Box::new(create_source(tx, state, tenant, s)?)))
        }

        Request::Update { id, patch: p } => {
            Outcome::Source(Some(Box::new(patch(tx, tenant, id, p)?)))
        }
        Request::Delete { id } => Outcome::Deletion(delete(tx, state, tenant, id)?),
        Request::GetImportRules { id } => {
            require(tx, tenant, id)?;
            let raw = tx.query_row(
                "SELECT imported_paths FROM projects WHERE tenant_id=?1 AND id=?2",
                params![tenant, id],
                |r| r.get(0),
            )?;
            let (rules, legacy) = rules(raw);
            Outcome::Data(json!({"rules":rules,"legacy_import_all":legacy}))
        }
        Request::SaveImportRules { id, rules } => {
            require(tx, tenant, id)?;
            let mut out = Vec::new();
            for r in rules {
                let r = normalize_rule(&r);
                if !r.is_empty() && !out.contains(&r) {
                    out.push(r);
                }
            }
            tx.execute(
                "UPDATE projects SET imported_paths=?1 WHERE tenant_id=?2 AND id=?3",
                params![serde_json::to_string(&out)?, tenant, id],
            )?;
            tx.execute("UPDATE build_profiles SET config_modified_at=?1 WHERE tenant_id=?2 AND id IN (SELECT profile_id FROM profile_layers WHERE tenant_id=?2 AND project_id=?3)",params![auth::catalog_timestamp(),tenant,id])?;
            Outcome::Data(json!({"rules":out}))
        }
        Request::BulkCategory {
            source_ids,
            category,
        } => {
            ensure!(
                !source_ids.is_empty(),
                "source_ids must be a non-empty array"
            );
            let mut seen = Vec::new();
            let mut results = Vec::new();
            let mut updated = Vec::new();
            for id in source_ids {
                if seen.contains(&id) {
                    continue;
                }
                seen.push(id);
                if get(tx, tenant, id)?.is_none() {
                    results.push(json!({"source_id":id,"ok":false,"detail":"Source not found"}));
                    continue;
                }
                let metadata = Map::from_iter([(
                    "category".into(),
                    json!(categories::normalize(category.as_deref().unwrap_or(""))),
                )]);
                updated.push(patch(
                    tx,
                    tenant,
                    id,
                    SourcePatch {
                        metadata: Some(metadata),
                        ..Default::default()
                    },
                )?);
                results.push(json!({"source_id":id,"ok":true}));
            }
            Outcome::Data(
                json!({"succeeded":updated.len(),"failed":results.len()-updated.len(),"updated":updated,"results":results}),
            )
        }
        Request::GetCategories {} => Outcome::Data(json!(categories::load(tx, tenant)?)),
        Request::GetCategoryTree {} => {
            Outcome::Data(categories::tree(&categories::load(tx, tenant)?))
        }
        Request::SaveCategories {
            categories,
            replacements,
        } => Outcome::Data(json!(categories::save(
            tx,
            tenant,
            categories,
            replacements
        )?)),
        Request::GetNaming { id } => Outcome::Data(naming::get(tx, tenant, id)?),
        Request::SaveNaming { id, settings } => {
            Outcome::Data(naming::save(tx, tenant, id, settings)?)
        }
        #[cfg(feature = "desktop_rebuild")]
        Request::PreviewNaming {
            relative_path,
            profile,
        } => Outcome::Preview(naming::preview(tx, tenant, relative_path, profile)?),
        Request::GetGlobalNaming {} => {
            Outcome::Data(serde_json::to_value(naming::global(tx, tenant)?)?)
        }
        Request::SaveGlobalNaming { profile } => {
            let profile = profile.validate_input()?;
            set_setting(
                tx,
                tenant,
                "stl_naming_defaults",
                serde_json::to_string(&profile)?,
            )?;
            Outcome::Data(serde_json::to_value(profile)?)
        }
    })
}
fn create_source(
    tx: &Transaction<'_>,
    state: &State,
    tenant: &str,
    s: CreateSource,
) -> Result<SourceSummary> {
    let name = trim(&s.name);
    ensure!(!name.is_empty(), "Source name is required");
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM projects WHERE tenant_id=?1 AND name=?2)",
        params![tenant, name],
        |r| r.get(0),
    )?;
    ensure!(!exists, "Source already exists: {name}");
    let kind = s.source_kind.unwrap_or("github".into()).to_lowercase();
    let source_type = s.source_type.unwrap_or(
        if matches!(kind.as_str(), "github" | "git") {
            "git"
        } else {
            "local"
        }
        .into(),
    );
    let metadata_json = stamped_create_metadata(s.metadata)?;
    tx.execute("INSERT INTO projects(tenant_id,name,url,branch,tag,source_kind,source_type,role,metadata_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![tenant,name,s.url.unwrap_or_default(),s.branch.unwrap_or("main".into()),s.tag.as_deref().map(trim).filter(|s|!s.is_empty()),kind,source_type,s.role.unwrap_or("unassigned".into()),metadata_json])?;
    let id = tx.last_insert_rowid();
    let path = state.directory.join("repos").join(id.to_string());
    tx.execute(
        "UPDATE projects SET local_path=?1 WHERE tenant_id=?2 AND id=?3",
        params![pp_os::display_path(&path).to_string_lossy(), tenant, id],
    )?;
    require(tx, tenant, id)
}

fn stamped_create_metadata(metadata: Option<Map<String, Value>>) -> Result<Option<String>> {
    let Some(metadata) = metadata else {
        return Ok(None);
    };
    #[cfg(feature = "desktop_rebuild")]
    let metadata = {
        let mut metadata = metadata;
        if metadata_guard_enabled() {
            metadata.shift_remove(CATALOG_METADATA_VERSION);
            metadata.insert(CATALOG_METADATA_VERSION.into(), json!(1));
        }
        metadata
    };
    Ok(Some(json::metadata(&metadata)?))
}

fn set_setting(tx: &Transaction<'_>, tenant: &str, key: &str, value: String) -> Result<()> {
    tx.execute("INSERT INTO app_settings(tenant_id,key,value) VALUES(?1,?2,?3) ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value",params![tenant,key,value])?;
    Ok(())
}
fn get_setting(tx: &Transaction<'_>, tenant: &str, key: &str) -> Result<Option<String>> {
    Ok(tx
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
            params![tenant, key],
            |r| r.get(0),
        )
        .optional()?)
}
fn name_referenced(tx: &Transaction<'_>, tenant: &str, name: &str) -> Result<bool> {
    let name = name.to_lowercase();
    for table in ["parts", "plan_revision_parts", "plan_draft_parts"] {
        let mut statement = tx.prepare(&format!(
            "SELECT substr(source_layer,instr(source_layer, ':')+1) FROM {table} WHERE tenant_id=?1 AND instr(source_layer, ':')>0"
        ))?;
        for reference in statement.query_map([tenant], |row| row.get::<_, String>(0))? {
            if reference?.to_lowercase() == name {
                return Ok(true);
            }
        }
    }
    let mut statement = tx.prepare("SELECT payload_json FROM plan_snapshots WHERE tenant_id=?1")?;
    for raw in statement.query_map([tenant], |r| r.get::<_, String>(0))? {
        let payload: Value = serde_json::from_str(&raw?)
            .map_err(|_| anyhow!("Invalid Build snapshot references"))?;
        if let Some(layers) = payload.get("layers") {
            let layers = layers
                .as_array()
                .ok_or_else(|| anyhow!("Invalid Build snapshot layers"))?;
            if layers.iter().any(|layer| {
                layer
                    .get("source_name")
                    .and_then(Value::as_str)
                    .is_some_and(|reference| reference.to_lowercase() == name)
            }) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn delete(tx: &Transaction<'_>, state: &State, tenant: &str, id: i64) -> Result<Deletion> {
    let Some(source) = get(tx, tenant, id)? else {
        return Ok(Deletion::NotFound);
    };
    if state.source_busy(tenant, id) || crate::jobs::source_reserved(tx, tenant, id)? {
        return Ok(Deletion::ActiveWork);
    }
    #[cfg(feature = "desktop_rebuild")]
    if crate::uploads::source_reserved(tx, tenant, id)? {
        return Ok(Deletion::ActiveWork);
    }
    if tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM source_revisions WHERE project_id=?1)",
        [id],
        |r| r.get::<_, bool>(0),
    )? {
        return Ok(Deletion::RetainedHistory);
    }
    for (table, column) in [
        ("profile_layers", "project_id"),
        ("plan_revision_inputs", "source_id"),
        ("plan_draft_inputs", "source_id"),
    ] {
        if tx.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {column}=?1)"),
            [id],
            |r| r.get::<_, bool>(0),
        )? {
            return Ok(Deletion::Referenced);
        }
    }
    if tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM source_notes WHERE project_id=?1 AND profile_id IS NOT NULL)",
        [id],
        |r| r.get::<_, bool>(0),
    )? {
        return Ok(Deletion::Referenced);
    }
    let mut planning = tx.prepare(
        "SELECT value FROM app_settings WHERE tenant_id=?1 AND key LIKE 'build_planning.v1.%'",
    )?;
    for raw in planning.query_map([tenant], |r| r.get::<_, String>(0))? {
        let value: Value = serde_json::from_str(&raw?)
            .map_err(|_| anyhow!("Invalid Build planning references"))?;
        if json_reference(&value, id)
            || value
                .get("draft_source_revisions")
                .and_then(Value::as_object)
                .is_some_and(|revisions| revisions.contains_key(&id.to_string()))
        {
            return Ok(Deletion::Referenced);
        }
    }
    let mut decisions =
        tx.prepare("SELECT params_json,result_json FROM plan_decisions WHERE tenant_id=?1")?;
    for row in decisions.query_map([tenant], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    })? {
        let (params, result) = row?;
        for raw in std::iter::once(params).chain(result) {
            let value: Value = serde_json::from_str(&raw)
                .map_err(|_| anyhow!("Invalid Build decision references"))?;
            if json_reference(&value, id) {
                return Ok(Deletion::Referenced);
            }
        }
    }
    if name_referenced(tx, tenant, &source.name)? {
        return Ok(Deletion::Referenced);
    }
    let mut statement = tx.prepare("SELECT payload_json FROM plan_snapshots WHERE tenant_id=?1")?;
    for raw in statement.query_map([tenant], |r| r.get::<_, String>(0))? {
        let payload: Value = serde_json::from_str(&raw?)
            .map_err(|_| anyhow!("Invalid Build snapshot references"))?;
        let layers = payload
            .get("layers")
            .map(|v| {
                v.as_array()
                    .ok_or_else(|| anyhow!("Invalid Build snapshot layers"))
            })
            .transpose()?;
        if layers.is_some_and(|layers| {
            layers
                .iter()
                .any(|layer| layer.get("project_id").and_then(Value::as_i64) == Some(id))
        }) {
            return Ok(Deletion::Referenced);
        }
    }
    tx.execute(
        "DELETE FROM projects WHERE tenant_id=?1 AND id=?2",
        params![tenant, id],
    )?;
    Ok(Deletion::Deleted { source_id: id })
}

#[cfg(test)]
mod tests;

fn json_reference(value: &Value, id: i64) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| match key.as_str() {
            "source_id" | "project_id" => value.as_i64() == Some(id),
            "source_ids" | "project_ids" | "managed_source_ids" => value
                .as_array()
                .is_some_and(|ids| ids.iter().any(|v| v.as_i64() == Some(id))),
            _ => json_reference(value, id),
        }),
        Value::Array(values) => values.iter().any(|v| json_reference(v, id)),
        _ => false,
    }
}
