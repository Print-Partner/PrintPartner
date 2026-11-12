pub mod manifest_options;
mod model;
pub mod wire;
pub use manifest_options::{ManifestOptionsCommand, ManifestOptionsOutcome};
mod experiments;
pub(crate) const SCHEMA_STEP: u64 = 48;
pub use experiments::{ExperimentBuild, ExperimentCopy};
pub mod header;
pub use header::{ArchiveRefusal, BuildPatch, SpecialRequest};
pub mod variants;
pub use variants::{VariantCommand, VariantOutcome};
mod decisions;
pub mod mcp;
pub use decisions::DecisionOutcome;
pub(crate) mod projection;
use crate::{Envelope, Shared, WriterOwner, auth, read_model};
use anyhow::{Result, anyhow, ensure};
pub use model::*;
pub use pp_domain::PositiveId;
pub use pp_domain::build_identity::BuildName;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetResource {
    GraphRows,
    GraphBytes,
    PlateOverlapChecks,
}

#[derive(Debug)]
pub enum Failure {
    Cancelled,
    TooManyBuilds,
    TooManyLayers,
    BudgetExhausted { resource: BudgetResource },
    QueueFull,
    Stopped,
    OutcomeUnknown,
}

crate::impl_debug_error!(Failure);

#[derive(Clone)]
pub enum BuildCommand {
    Mcp(mcp::Request),
    Manifest(ManifestOptionsCommand),
    Variants(VariantCommand),
    /// Node's GET or PATCH /plans/{id}/features; read-only without a patch.
    PlanFeatures(crate::features::PlanFeatureRequest),
    /// Node's POST /plans/{id}/decisions with its JSON body.
    AppendDecision {
        build: PositiveId,
        body: Vec<u8>,
    },
    List,
    Read {
        build: PositiveId,
    },
    ListLayers {
        build: PositiveId,
    },
    Create {
        name: BuildName,
        base_source: Option<PositiveId>,
    },
    Duplicate {
        build: PositiveId,
        name: BuildName,
    },
    Touch {
        build: PositiveId,
    },
    /// Node's PATCH /plans/{id}. `legacy` is the /api/v1 contract.
    Update {
        build: PositiveId,
        patch: BuildPatch,
        legacy: bool,
    },
    SetBase {
        build: PositiveId,
        source: PositiveId,
    },
    AttachAddon {
        build: PositiveId,
        source: PositiveId,
    },
    ReplaceAttachment {
        build: PositiveId,
        layer: PositiveId,
        source: PositiveId,
    },
    Detach {
        build: PositiveId,
        layer: PositiveId,
    },
    Delete {
        build: PositiveId,
    },
}
#[derive(Debug)]
pub enum BuildOutcome {
    Mcp(serde_json::Value),
    Manifest(ManifestOptionsOutcome),
    Variants(VariantOutcome),
    PlanFeatures(crate::features::PlanFeatureOutcome),
    Decision(DecisionOutcome),
    Listed {
        profiles: Vec<ProfileSummary>,
    },
    Read {
        profile: Box<ProfileSummary>,
    },
    Layers {
        profile_id: i64,
        layers: Vec<ProfileLayer>,
    },
    Created {
        profile: Box<ProfileSummary>,
        layers: Vec<ProfileLayer>,
    },
    Touched {
        profile: Box<ProfileSummary>,
    },
    Duplicated(ExperimentCopy),
    HasExperiments {
        builds: Vec<ExperimentBuild>,
    },
    Deleted,
    MissingBuild,
    /// The name another Build of the tenant has (`duplicate_build_name`).
    RenameTaken {
        name: String,
    },
    NameRequired,
    FeatureDisabled,
    /// `String(special_request)` threw after any rename was saved: Node's 500
    /// beside an archive, else its 400.
    Uncoercible {
        archive: bool,
    },
    FeatureTogglesInvalid,
    ArchiveRefused(ArchiveRefusal),
    InvalidBaseSource,
    MissingSource,
    MissingLayer,
    DuplicateName {
        name: String,
    },
    DuplicateAttachment {
        source_name: String,
    },
}
#[derive(Clone)]
pub struct BuildGraphClient {
    settings: crate::SettingsClient,
    manifest_reads: Option<Arc<dyn crate::working_drafts::observation::DraftReads>>,
    manifest_limits: crate::working_drafts::observation::PreparationLimits,
    shared: Arc<Shared>,
    policy: auth::AuthPolicy,
    repos: PathBuf,
}
pub(super) struct Command {
    prepared_manifest: Option<Box<manifest_options::PreparedManifestCommand>>,
    credential: read_model::Credential,
    policy: auth::AuthPolicy,
    command: BuildCommand,
    cancelled: Arc<AtomicBool>,
    repos: PathBuf,
}
pub(super) struct Reply {
    sender: mpsc::Sender<Result<BuildOutcome>>,
    #[cfg(test)]
    discard: bool,
}

impl Reply {
    pub(super) fn send(self, outcome: Result<BuildOutcome>) {
        #[cfg(test)]
        if self.discard {
            return;
        }
        let _ = self.sender.send(outcome);
    }
}

impl WriterOwner {
    pub fn build_graph_with_policy(&self, policy: auth::AuthPolicy) -> Result<BuildGraphClient> {
        auth::validate_policy(policy)?;
        Ok(BuildGraphClient {
            settings: self.client.clone(),
            manifest_reads: None,
            manifest_limits: crate::working_drafts::observation::PreparationLimits::default(),
            shared: self.client.shared.clone(),
            policy,
            repos: self
                .lease
                .as_ref()
                .ok_or_else(|| anyhow!("Storage owner is closed"))?
                .data_dir()
                .join("repos"),
        })
    }

    pub fn build_graph_with_observations(
        &self,
        policy: auth::AuthPolicy,
        reads: Arc<dyn crate::working_drafts::observation::DraftReads>,
        limits: crate::working_drafts::observation::PreparationLimits,
    ) -> Result<BuildGraphClient> {
        limits.validate()?;
        let mut client = self.build_graph_with_policy(policy)?;
        client.manifest_reads = Some(reads);
        client.manifest_limits = limits;
        Ok(client)
    }
}
impl BuildGraphClient {
    pub fn execute(
        &self,
        credential: read_model::Credential,
        command: BuildCommand,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<BuildOutcome> {
        if let BuildCommand::Mcp(request) = command {
            return self.execute_mcp(credential, request, cancelled, wait);
        }
        if let BuildCommand::Variants(command) = command {
            return self.execute_variants(credential, command, cancelled, wait);
        }
        if let BuildCommand::Manifest(manifest) = &command
            && !manifest.is_read()
        {
            return self.execute_observed_manifest(credential, manifest.clone(), cancelled, wait);
        }
        self.execute_with_reply(credential, command, None, cancelled, wait, false)
    }
    fn execute_with_reply(
        &self,
        credential: read_model::Credential,
        command: BuildCommand,
        prepared_manifest: Option<manifest_options::PreparedManifestCommand>,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
        discard_reply: bool,
    ) -> Result<BuildOutcome> {
        #[cfg(not(test))]
        let _ = discard_reply;
        let deadline = Instant::now() + wait;
        let mut queue = self
            .shared
            .queue
            .lock()
            .map_err(|_| anyhow!(Failure::Stopped))?;
        loop {
            ensure!(!queue.closed, Failure::Stopped);
            ensure!(!cancelled.load(Ordering::Acquire), Failure::Cancelled);
            if queue.pending.len() < self.shared.capacity {
                break;
            }
            ensure!(Instant::now() < deadline, Failure::QueueFull);
            queue = self
                .shared
                .changed
                .wait_timeout(queue, Duration::from_millis(5))
                .map_err(|_| anyhow!(Failure::Stopped))?
                .0;
        }
        let (reply, receiver) = mpsc::channel();
        queue.pending.push_back(Envelope::BuildGraph {
            command: Command {
                prepared_manifest: prepared_manifest.map(Box::new),
                credential,
                policy: self.policy,
                command,
                cancelled,
                repos: self.repos.clone(),
            },
            reply: Reply {
                sender: reply,
                #[cfg(test)]
                discard: discard_reply,
            },
        });
        self.shared.changed.notify_all();
        drop(queue);
        receiver
            .recv()
            .map_err(|_| anyhow!(Failure::OutcomeUnknown))?
    }
}
fn credential_tenant(
    tx: &Transaction<'_>,
    credential: &read_model::Credential,
    policy: auth::AuthPolicy,
) -> Result<String> {
    match credential {
        read_model::Credential::Session(secret) => auth::read_session_tenant(tx, secret, policy),
        read_model::Credential::ApiKey {
            routed_tenant,
            secret,
        } => auth::read_key_tenant(tx, routed_tenant, secret),
    }
}

fn tenant(tx: &Transaction<'_>, command: &Command) -> Result<String> {
    credential_tenant(tx, &command.credential, command.policy)
}

fn build_exists(tx: &Transaction<'_>, tenant: &str, build: i64) -> Result<bool> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM build_profiles WHERE tenant_id=?1 AND id=?2)",
        params![tenant, build],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn source_name(tx: &Transaction<'_>, tenant: &str, source: i64) -> Result<Option<String>> {
    tx.query_row(
        "SELECT name FROM projects WHERE tenant_id=?1 AND id=?2",
        params![tenant, source],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

fn duplicate_source(
    tx: &Transaction<'_>,
    tenant: &str,
    build: i64,
    source: i64,
    except_layer: Option<i64>,
) -> Result<bool> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM profile_layers WHERE tenant_id=?1 AND profile_id=?2 AND project_id=?3 AND (?4 IS NULL OR id<>?4))",
        params![tenant, build, source, except_layer],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn layers(
    tx: &Transaction<'_>,
    tenant: &str,
    build: i64,
    cancelled: &AtomicBool,
) -> Result<Vec<ProfileLayer>> {
    let mut statement = tx.prepare(
        "SELECT layer.id,layer.layer_order,layer.layer_type,layer.project_id,source.name FROM profile_layers layer LEFT JOIN projects source ON source.tenant_id=layer.tenant_id AND source.id=layer.project_id WHERE layer.tenant_id=?1 AND layer.profile_id=?2 ORDER BY layer.layer_order,layer.id LIMIT 1025",
    )?;
    let rows = statement.query_map(params![tenant, build], |row| {
        Ok(ProfileLayer {
            id: row.get(0)?,
            layer_order: row.get(1)?,
            layer_type: row.get(2)?,
            project_id: row.get(3)?,
            project_name: row.get(4)?,
        })
    })?;
    let mut layers = Vec::new();
    for row in rows {
        ensure!(!cancelled.load(Ordering::Acquire), Failure::Cancelled);
        let layer = row?;
        ensure!(layers.len() < 1024, Failure::TooManyLayers);
        layers.push(layer);
    }
    Ok(layers)
}

fn mark_modified(tx: &Transaction<'_>, tenant: &str, build: i64, now: &str) -> Result<()> {
    ensure!(
        tx.execute(
            "UPDATE build_profiles SET config_modified_at=?1 WHERE tenant_id=?2 AND id=?3",
            params![now, tenant, build],
        )? == 1,
        "Build freshness update failed"
    );
    Ok(())
}

pub(crate) fn mutation(
    tx: &Transaction<'_>,
    tenant: &str,
    command: &BuildCommand,
    repos: &std::path::Path,
    cancelled: &AtomicBool,
) -> Result<BuildOutcome> {
    let mut budget = read_model::graph::Budget::default();
    match command {
        BuildCommand::Mcp(request) => {
            mcp::execute(tx, tenant, request, repos, cancelled).map(BuildOutcome::Mcp)
        }
        BuildCommand::Create { name, base_source } => {
            let duplicate: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM build_profiles WHERE tenant_id=?1 AND name=?2)",
                params![tenant, name.as_str()],
                |row| row.get(0),
            )?;
            if duplicate {
                return Ok(BuildOutcome::DuplicateName {
                    name: name.as_str().to_owned(),
                });
            }
            let source = if let Some(source) = base_source {
                let id = source.get() as i64;
                if source_name(tx, tenant, id)?.is_none() {
                    return Ok(BuildOutcome::InvalidBaseSource);
                }
                Some(id)
            } else {
                None
            };
            let now = auth::catalog_timestamp();
            tx.execute(
                "INSERT INTO build_profiles(tenant_id,name,config_modified_at,last_used_at) VALUES(?1,?2,?3,?3)",
                params![tenant, name.as_str(), now],
            )?;
            let build = tx.last_insert_rowid();
            if let Some(source) = source {
                tx.execute(
                    "INSERT INTO profile_layers(tenant_id,profile_id,layer_order,layer_type,project_id) VALUES(?1,?2,0,'base',?3)",
                    params![tenant, build, source],
                )?;
            }
            let profile = read_model::graph::build_summary(tx, tenant, build, repos, &mut budget)?
                .ok_or_else(|| anyhow!("Created Build missing"))?;
            Ok(BuildOutcome::Created {
                profile: Box::new(profile),
                layers: layers(tx, tenant, build, cancelled)?,
            })
        }
        BuildCommand::Duplicate { build, name } => experiments::duplicate(tx, tenant, *build, name),
        BuildCommand::Touch { build } => {
            let build = build.get() as i64;
            let now = auth::catalog_timestamp();
            if tx.execute(
                "UPDATE build_profiles SET last_used_at=?1 WHERE tenant_id=?2 AND id=?3",
                params![now, tenant, build],
            )? == 0
            {
                return Ok(BuildOutcome::MissingBuild);
            }
            Ok(BuildOutcome::Touched {
                profile: Box::new(
                    read_model::graph::build_summary(tx, tenant, build, repos, &mut budget)?
                        .ok_or_else(|| anyhow!("Touched Build missing"))?,
                ),
            })
        }
        BuildCommand::Update {
            build,
            patch,
            legacy,
        } => header::update(tx, tenant, *build, patch, *legacy, repos),
        BuildCommand::SetBase { build, source } => {
            let build = build.get() as i64;
            let source = source.get() as i64;
            if !build_exists(tx, tenant, build)? {
                return Ok(BuildOutcome::MissingBuild);
            }
            let Some(source_name) = source_name(tx, tenant, source)? else {
                return Ok(BuildOutcome::MissingSource);
            };
            let existing: Option<i64> = tx.query_row(
                "SELECT id FROM profile_layers WHERE tenant_id=?1 AND profile_id=?2 AND layer_type='base' ORDER BY id LIMIT 1",
                params![tenant, build],
                |row| row.get(0),
            ).optional()?;
            if duplicate_source(tx, tenant, build, source, existing)? {
                return Ok(BuildOutcome::DuplicateAttachment { source_name });
            }
            if let Some(layer) = existing {
                tx.execute(
                    "UPDATE profile_layers SET project_id=?1,layer_order=0 WHERE tenant_id=?2 AND profile_id=?3 AND id=?4",
                    params![source, tenant, build, layer],
                )?;
            } else {
                tx.execute(
                    "INSERT INTO profile_layers(tenant_id,profile_id,layer_order,layer_type,project_id) VALUES(?1,?2,0,'base',?3)",
                    params![tenant, build, source],
                )?;
            }
            mark_modified(tx, tenant, build, &auth::catalog_timestamp())?;
            Ok(BuildOutcome::Layers {
                profile_id: build,
                layers: layers(tx, tenant, build, cancelled)?,
            })
        }
        BuildCommand::AttachAddon { build, source } => {
            let build = build.get() as i64;
            let source = source.get() as i64;
            if !build_exists(tx, tenant, build)? {
                return Ok(BuildOutcome::MissingBuild);
            }
            let Some(source_name) = source_name(tx, tenant, source)? else {
                return Ok(BuildOutcome::MissingSource);
            };
            if duplicate_source(tx, tenant, build, source, None)? {
                return Ok(BuildOutcome::DuplicateAttachment { source_name });
            }
            let next: i64 = tx.query_row(
                "SELECT coalesce(max(layer_order),-1)+1 FROM profile_layers WHERE tenant_id=?1 AND profile_id=?2",
                params![tenant, build],
                |row| row.get(0),
            )?;
            tx.execute(
                "INSERT INTO profile_layers(tenant_id,profile_id,layer_order,layer_type,project_id) VALUES(?1,?2,?3,'addon',?4)",
                params![tenant, build, next, source],
            )?;
            mark_modified(tx, tenant, build, &auth::catalog_timestamp())?;
            Ok(BuildOutcome::Layers {
                profile_id: build,
                layers: layers(tx, tenant, build, cancelled)?,
            })
        }
        BuildCommand::ReplaceAttachment {
            build,
            layer,
            source,
        } => {
            let build = build.get() as i64;
            let layer = layer.get() as i64;
            let source = source.get() as i64;
            if !build_exists(tx, tenant, build)? {
                return Ok(BuildOutcome::MissingBuild);
            }
            let member: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM profile_layers WHERE tenant_id=?1 AND profile_id=?2 AND id=?3)",
                params![tenant, build, layer],
                |row| row.get(0),
            )?;
            if !member {
                return Ok(BuildOutcome::MissingLayer);
            }
            let Some(source_name) = source_name(tx, tenant, source)? else {
                return Ok(BuildOutcome::MissingSource);
            };
            if duplicate_source(tx, tenant, build, source, Some(layer))? {
                return Ok(BuildOutcome::DuplicateAttachment { source_name });
            }
            ensure!(
                tx.execute(
                    "UPDATE profile_layers SET project_id=?1 WHERE tenant_id=?2 AND profile_id=?3 AND id=?4",
                    params![source, tenant, build, layer],
                )? == 1,
                "Build attachment changed concurrently"
            );
            mark_modified(tx, tenant, build, &auth::catalog_timestamp())?;
            Ok(BuildOutcome::Layers {
                profile_id: build,
                layers: layers(tx, tenant, build, cancelled)?,
            })
        }
        BuildCommand::Detach { build, layer } => {
            let build = build.get() as i64;
            let layer = layer.get() as i64;
            if !build_exists(tx, tenant, build)? {
                return Ok(BuildOutcome::MissingBuild);
            }
            if tx.execute(
                "DELETE FROM profile_layers WHERE tenant_id=?1 AND profile_id=?2 AND id=?3",
                params![tenant, build, layer],
            )? == 0
            {
                return Ok(BuildOutcome::MissingLayer);
            }
            mark_modified(tx, tenant, build, &auth::catalog_timestamp())?;
            Ok(BuildOutcome::Deleted)
        }
        BuildCommand::Delete { build } => {
            let build = build.get() as i64;
            if !build_exists(tx, tenant, build)? {
                return Ok(BuildOutcome::MissingBuild);
            }
            let builds = experiments::children(tx, build)?;
            if !builds.is_empty() {
                return Ok(BuildOutcome::HasExperiments { builds });
            }
            tx.pragma_update(None, "defer_foreign_keys", "ON")?;
            tx.execute(
                "DELETE FROM app_settings WHERE tenant_id=?1 AND key IN (?2,?3,?4,?5)",
                params![
                    tenant,
                    format!("production_setup:{build}"),
                    format!("role_filaments_{build}"),
                    format!("feature_toggles:plan:{build}"),
                    format!("kit_manifest_{build}")
                ],
            )?;
            ensure!(
                tx.execute(
                    "DELETE FROM build_profiles WHERE tenant_id=?1 AND id=?2",
                    params![tenant, build],
                )? == 1,
                "Build changed concurrently"
            );
            Ok(BuildOutcome::Deleted)
        }
        BuildCommand::Variants(command) => variants::commit(tx, tenant, command),
        BuildCommand::PlanFeatures(request) => Ok(BuildOutcome::PlanFeatures(
            crate::features::plan_features(tx, tenant, request)?,
        )),
        BuildCommand::AppendDecision { build, body } => decisions::append(tx, tenant, *build, body),
        BuildCommand::Manifest(_)
        | BuildCommand::List
        | BuildCommand::Read { .. }
        | BuildCommand::ListLayers { .. } => {
            unreachable!("read command in mutation transaction")
        }
    }
}

fn read(
    tx: &Transaction<'_>,
    tenant: &str,
    command: &BuildCommand,
    repos: &std::path::Path,
    cancelled: &AtomicBool,
) -> Result<BuildOutcome> {
    match command {
        BuildCommand::Mcp(_) => mutation(tx, tenant, command, repos, cancelled),
        BuildCommand::Manifest(command) => Ok(BuildOutcome::Manifest(manifest_options::read(
            tx, tenant, command,
        )?)),
        BuildCommand::List => {
            let mut budget = read_model::graph::Budget::default();
            let ids = {
                let mut statement = tx.prepare(
                    "SELECT id FROM build_profiles WHERE tenant_id=?1 ORDER BY name,id LIMIT 65",
                )?;
                let rows = statement.query_map([tenant], |row| row.get::<_, i64>(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            ensure!(ids.len() <= 64, Failure::TooManyBuilds);
            let profiles = ids
                .into_iter()
                .map(|build| {
                    ensure!(!cancelled.load(Ordering::Acquire), Failure::Cancelled);
                    read_model::graph::build_summary(tx, tenant, build, repos, &mut budget)?
                        .ok_or_else(|| anyhow!("Listed Build missing"))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(BuildOutcome::Listed { profiles })
        }
        BuildCommand::Read { build } => {
            let mut budget = read_model::graph::Budget::default();
            let Some(profile) = read_model::graph::build_summary(
                tx,
                tenant,
                build.get() as i64,
                repos,
                &mut budget,
            )?
            else {
                return Ok(BuildOutcome::MissingBuild);
            };
            Ok(BuildOutcome::Read {
                profile: Box::new(profile),
            })
        }
        BuildCommand::ListLayers { build } => {
            let build = build.get() as i64;
            if !build_exists(tx, tenant, build)? {
                return Ok(BuildOutcome::MissingBuild);
            }
            Ok(BuildOutcome::Layers {
                profile_id: build,
                layers: layers(tx, tenant, build, cancelled)?,
            })
        }
        BuildCommand::PlanFeatures(request) => Ok(BuildOutcome::PlanFeatures(
            crate::features::plan_features(tx, tenant, request)?,
        )),
        _ => unreachable!("mutation command in read transaction"),
    }
}

pub(super) fn execute(
    connection: &mut Connection,
    mut command: Command,
    admission: crate::writer_admission::Deadline,
) -> Result<BuildOutcome> {
    ensure!(
        !command.cancelled.load(Ordering::Acquire),
        Failure::Cancelled
    );
    let read_only = matches!(
        command.command,
        BuildCommand::List | BuildCommand::Read { .. } | BuildCommand::ListLayers { .. }
    ) || matches!(&command.command, BuildCommand::Mcp(request) if !request.apply || request.verify_only)
        || matches!(&command.command, BuildCommand::Manifest(command) if command.is_read())
        || matches!(&command.command, BuildCommand::PlanFeatures(request) if request.patch.is_none());
    let tx = if read_only {
        connection.transaction()?
    } else {
        crate::writer_admission::begin_immediate(connection, admission)?
    };
    let tenant = tenant(&tx, &command)?;
    let outcome = if let Some(prepared) = command.prepared_manifest.take() {
        BuildOutcome::Manifest(manifest_options::finish(&tx, &tenant, *prepared)?)
    } else {
        let operation = if read_only { read } else { mutation };
        operation(
            &tx,
            &tenant,
            &command.command,
            &command.repos,
            &command.cancelled,
        )?
    };
    ensure!(
        !command.cancelled.load(Ordering::Acquire),
        Failure::Cancelled
    );
    if matches!(&outcome, BuildOutcome::Mcp(value) if value["ok"] == false || value.get("error").is_some())
    {
        tx.rollback()?;
    } else {
        tx.commit()?;
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(command: BuildCommand, repos: PathBuf) -> Command {
        Command {
            prepared_manifest: None,
            credential: read_model::Credential::Session(auth::Secret::new(
                "read-fixture-secret".into(),
            )),
            policy: auth::AuthPolicy {
                registration: auth::RegistrationPolicy::Open,
                first_user: auth::FirstUserTenant::NewUser,
                session_tenant: auth::SessionTenantPolicy::AccountTenant,
            },
            command,
            cancelled: Arc::new(AtomicBool::new(false)),
            repos,
        }
    }

    #[test]
    fn expired_mutations_skip_sql_and_preserve_timeout_before_fresh_mutation() {
        use crate::writer_admission::Deadline;
        let root = crate::tests::directory("build-admission");
        let mut connection = Connection::open(root.join("admission.db")).unwrap();
        connection
            .busy_timeout(Duration::from_millis(1234))
            .unwrap();
        let build = PositiveId::new(1).unwrap();
        for mutation in [
            BuildCommand::Create {
                name: BuildName::parse("Expired".into()).unwrap(),
                base_source: None,
            },
            BuildCommand::Touch { build },
            BuildCommand::SetBase {
                build,
                source: build,
            },
            BuildCommand::AttachAddon {
                build,
                source: build,
            },
            BuildCommand::ReplaceAttachment {
                build,
                layer: build,
                source: build,
            },
            BuildCommand::Detach {
                build,
                layer: build,
            },
            BuildCommand::Delete { build },
        ] {
            let error = execute(
                &mut connection,
                command(mutation, PathBuf::new()),
                Deadline::expired(),
            )
            .unwrap_err();
            assert_eq!(
                error
                    .downcast_ref::<rusqlite::Error>()
                    .unwrap()
                    .sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy),
                "{error:#}"
            );
            assert!(connection.is_autocommit());
            assert_eq!(
                crate::tests::pragma_value(&connection, "busy_timeout"),
                1234
            );
            assert_eq!(connection.total_changes(), 0);
        }
        std::fs::write(
            root.join("fixture.db"),
            include_bytes!("../tests/fixtures/accepted-plan-node.db"),
        )
        .unwrap();
        drop(
            crate::schema::initialize(&root.join("fixture.db"), 34, &auth::catalog_timestamp())
                .unwrap(),
        );
        connection
            .restore(
                "main",
                root.join("fixture.db"),
                None::<fn(rusqlite::backup::Progress)>,
            )
            .unwrap();
        crate::schema::configure(&connection, false).unwrap();
        assert!(matches!(
            execute(
                &mut connection,
                command(BuildCommand::Delete { build }, root.join("repos")),
                Deadline::at_dequeue()
            )
            .unwrap(),
            BuildOutcome::Deleted
        ));
        let count: i64 = connection
            .query_row("SELECT count(*) FROM build_profiles", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
        assert!(connection.is_autocommit());
        assert_eq!(
            crate::tests::pragma_value(&connection, "busy_timeout"),
            5000
        );
        drop(connection);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn expired_writer_deadline_does_not_block_deferred_reads() {
        use crate::writer_admission::Deadline;
        let root = crate::tests::directory("build-deferred-read");
        let database = root.join("fixture.db");
        std::fs::write(
            &database,
            include_bytes!("../tests/fixtures/accepted-plan-node.db"),
        )
        .unwrap();
        let (mut connection, _) =
            crate::schema::initialize(&database, 34, &auth::catalog_timestamp()).unwrap();
        connection
            .busy_timeout(Duration::from_millis(1234))
            .unwrap();
        let holder = Connection::open(&database).unwrap();
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();
        let build = PositiveId::new(1).unwrap();
        for read in [
            BuildCommand::List,
            BuildCommand::Read { build },
            BuildCommand::ListLayers { build },
        ] {
            let outcome = execute(
                &mut connection,
                command(read, root.join("repos")),
                Deadline::expired(),
            )
            .unwrap();
            match outcome {
                BuildOutcome::Listed { profiles } => assert_eq!(profiles.len(), 1),
                BuildOutcome::Read { profile } => assert_eq!(profile.id, 1),
                BuildOutcome::Layers { profile_id, layers } => {
                    assert_eq!(profile_id, 1);
                    assert_eq!(layers.len(), 1);
                }
                outcome => panic!("{outcome:?}"),
            }
            assert!(connection.is_autocommit());
            assert_eq!(
                crate::tests::pragma_value(&connection, "busy_timeout"),
                1234
            );
        }
        holder.execute_batch("ROLLBACK").unwrap();
        drop(holder);
        drop(connection);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lost_reply_is_outcome_unknown() {
        let root = std::env::temp_dir().join(format!("pp-build-reply-{}", rand::random::<u64>()));
        let (owner, _) = WriterOwner::open(&root, crate::Limits::default()).unwrap();
        let client = owner
            .build_graph_with_policy(auth::AuthPolicy {
                registration: auth::RegistrationPolicy::Open,
                first_user: auth::FirstUserTenant::NewUser,
                session_tenant: auth::SessionTenantPolicy::AccountTenant,
            })
            .unwrap();
        let error = client
            .execute_with_reply(
                read_model::Credential::Session(auth::Secret::new("missing".into())),
                BuildCommand::List,
                None,
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(2),
                true,
            )
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref(),
            Some(Failure::OutcomeUnknown)
        ));
        owner.shutdown().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
