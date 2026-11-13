use crate::{DesktopRebuild, diagnostics};
use anyhow::{Result, anyhow};
use axum::Router;
use pp_api::{
    accepted::{AcceptedHttpConfig, accepted_router},
    admin::{AdminHttpConfig, admin_router},
    auth::auth_router,
    builds::{BuildHttpConfig, build_router},
    catalog::{CatalogHttpConfig, admit_catalog_routes},
    checkoff::checkoff_router,
    drafts::{DraftHttpConfig, draft_router as api_draft_router},
    events::{StreamLimits, events_router},
    file_assignments::file_assignments_router,
    history::history_router,
    integrations::integrations_router,
    jobs::{JobsHttpConfig, jobs_router as api_jobs_router},
    mcp::{McpClients, McpHttpConfig, mcp_router},
    modules::modules_router,
    planning::planning_router,
    plates::plates_router,
    printers::printers_router,
    settings::settings_router,
    workflow::workflow_router,
};
// Re-export so desktop_rebuild integration tests can build AuthRoutes without a
// non-optional pp-api [dev-dependencies] edge (gated boundary check).
pub use pp_api::auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer};
use pp_gateway::printer_host::HostReads;
use pp_storage::{
    SettingsClient, WriterOwner,
    auth::{
        AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy,
        rebuild::AuthLifetimes, rebuild::RebuildAuthClient,
    },
    build_graph::BuildGraphClient,
    checkoff_progress::ProgressClient,
    features::FeatureClient,
    read_model::{ReadClient, build_workflow::PendingInputs},
    working_drafts::{
        WorkingDraftClient,
        observation::{DraftReads, PreparationLimits},
        source_reads::{SourceFilesystemPolicy, SourceReads},
    },
};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

pub struct AuthRoutes {
    pub http: AuthHttpConfig,
    pub policy: AuthPolicy,
    pub lifetimes: AuthLifetimes,
    pub providers: ProviderClient,
    pub mail: ResetMailer,
}

/// Completed authenticated read surfaces the rebuild router may admit.
/// Job HTTP and WebSocket routes are `job_router` (GRE-821), not this set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebuildReadOwner {
    Auth,
    Builds,
    Catalog,
}

impl RebuildReadOwner {
    /// Admit a known read surface. Unknown names fail closed (`None`) before
    /// any client construction, route mount, or storage execution.
    pub fn admit(name: &str) -> Option<Self> {
        match name {
            "auth" => Some(Self::Auth),
            "builds" => Some(Self::Builds),
            "catalog" => Some(Self::Catalog),
            _ => None,
        }
    }
}

/// Completed draft mutation surfaces the rebuild router may admit.
/// Job HTTP and WebSocket routes are `job_router` (GRE-821), not this set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebuildDraftOwner {
    Drafts,
}

impl RebuildDraftOwner {
    /// Admit a known draft surface. Unknown names fail closed (`None`) before
    /// any client construction, route mount, or storage execution.
    pub fn admit(name: &str) -> Option<Self> {
        match name {
            "drafts" => Some(Self::Drafts),
            _ => None,
        }
    }
}

/// Opted-in rebuild HTTP storage clients. Constructed only when the stored
/// `DesktopRebuild` setting is on.
///
/// Catalog HTTP is not a precomposed client: `admit_catalog_routes` takes the
/// shared `WriterOwner` and builds a credentialed `SourceCatalogClient` per
/// request. `read_router` mounts catalog that way; this bundle does not hold a
/// LocalOwner catalog handle.
pub struct RebuildHttpClients {
    pub auth: RebuildAuthClient,
    pub drafts: WorkingDraftClient,
    pub builds: BuildGraphClient,
}

impl RebuildHttpClients {
    /// Drop owned client handles. Storage shutdown stays with `WriterOwner`.
    pub fn close(self) {}
}

/// Compose auth, draft, and Build clients without mounting. Draft recompute
/// and Build manifest options read Sources through `WriterOwner::source_reads`,
/// with Node's filesystem policy for `policy` and the community manifests in
/// `community_manifests` (the Node bundle's, from
/// `pp_compat::Bundle::community_manifests_dir`; `None` reads them as missing).
/// Returns `None` before creating any client unless the stored setting is on.
/// Catalog admission uses `admit_catalog_routes(owner)` from `read_router`.
pub fn clients(
    rebuild: DesktopRebuild,
    owner: &WriterOwner,
    policy: AuthPolicy,
    lifetimes: AuthLifetimes,
    community_manifests: Option<&Path>,
) -> Result<Option<RebuildHttpClients>> {
    if !rebuild.enabled() {
        return Ok(None);
    }
    let reads: Arc<dyn DraftReads> = Arc::new(source_reads(owner, policy, community_manifests)?);
    Ok(Some(RebuildHttpClients {
        auth: owner.rebuild_auth_with_policy(policy, lifetimes)?,
        drafts: owner.working_drafts_with_observations(
            policy,
            reads.clone(),
            PreparationLimits::default(),
        )?,
        builds: owner.build_graph_with_observations(policy, reads, PreparationLimits::default())?,
    }))
}

/// The Source reader `clients` gives drafts and Builds.
fn source_reads(
    owner: &WriterOwner,
    policy: AuthPolicy,
    community_manifests: Option<&Path>,
) -> Result<SourceReads> {
    let reads = owner
        .source_reads()?
        .with_filesystem_policy(SourceFilesystemPolicy::for_auth(&policy));
    Ok(match community_manifests {
        Some(root) => reads.with_community_manifests(root),
        None => reads,
    })
}

/// Rust HTTP routes for the desktop rebuild, served by the rebuild auth client.
/// Returns `None` before creating any client unless the stored setting is on.
pub fn router(
    rebuild: DesktopRebuild,
    owner: &WriterOwner,
    auth: AuthRoutes,
) -> Result<Option<Router>> {
    if !rebuild.enabled() {
        return Ok(None);
    }
    let client = owner.rebuild_auth_with_policy(auth.policy, auth.lifetimes)?;
    Ok(Some(auth_router(
        auth.http,
        client,
        auth.providers,
        auth.mail,
    )))
}

/// The printer Checkoff routes and the print-file assignment routes, which
/// read print files off printers through the stored printer hosts.
fn checkoff_routes(
    config: AcceptedHttpConfig,
    client: ProgressClient,
    settings: SettingsClient,
) -> Result<Router> {
    let snapshots = settings.print_file_snapshots();
    let hosts = crate::printer_host::StoredPrinterHosts::new(settings, HostReads::new()?);
    let files = file_assignments_router(config.clone(), client.clone(), snapshots, Arc::new(hosts));
    Ok(checkoff_router(config, client).merge(files))
}

fn accepted_routes(config: AcceptedHttpConfig, client: ReadClient) -> Result<Router> {
    // Spoolman shortfalls read inventory through pp-net's checked connections.
    Ok(accepted_router(
        config.with_stock(Arc::new(crate::spoolman::SpoolmanInventory::new()?)),
        client,
    ))
}

/// Compose authenticated read routers (auth, Plan builds, catalog, Accepted
/// Plan Checkoff and Spoolman shortfalls, managed Plates, Build history, Build
/// planning and decisions, Build workflow and task feed, legal documents, custom filaments,
/// the domain event stream and key-only MCP) from one
/// `WriterOwner` when the stored rebuild setting is on. Aliases share that
/// owner. Returns `None` before creating clients or mounting when the setting
/// is off. Does not cut over production gateway ownership (GRE-593).
///
/// `origin` must match `auth.http`'s listener origin. `api_key_tenant` is the
/// configured API-key tenant for build and catalog admission.
pub fn read_router(
    rebuild: DesktopRebuild,
    owner: Arc<Mutex<WriterOwner>>,
    auth: AuthRoutes,
    origin: &str,
    api_key_tenant: &str,
    community_manifests: Option<&Path>,
) -> Result<Option<Router>> {
    read_router_with_updates(
        rebuild,
        owner,
        auth,
        origin,
        api_key_tenant,
        community_manifests,
        ServerOptions::default(),
    )
}

/// What only the headless server turns on in the product routes.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ServerOptions {
    /// Settings may check for server updates.
    pub update_checks: bool,
    /// The routes serve an explicit `PP_LISTEN_ADDR` listener, so MCP takes its
    /// Host and Origin from the listener's validated authority.
    pub explicit_listener: bool,
}

/// MCP's config for the rebuild listener `origin`, or the explicit listener.
fn mcp_config(origin: &str, api_key_tenant: &str, server: ServerOptions) -> Result<McpHttpConfig> {
    Ok(
        McpHttpConfig::from_startup(origin, api_key_tenant, Some("1"))?
            .explicit_listener(server.explicit_listener),
    )
}

/// The Settings screen's routes outside `/settings`: the Module switches,
/// the integration writes and the printer routes, from the same owner.
fn setup_routes(
    owner: &Mutex<WriterOwner>,
    policy: AuthPolicy,
    config: AcceptedHttpConfig,
    features: FeatureClient,
) -> Result<Router> {
    let guard = owner
        .lock()
        .map_err(|_| anyhow!("rebuild owner lock poisoned"))?;
    let setup = guard.printer_setup(policy)?;
    let integrations = integrations_router(config.clone(), setup.clone());
    let printers = printers_router(config.clone(), setup);
    Ok(modules_router(config, features)
        .merge(integrations)
        .merge(printers))
}

fn read_router_with_updates(
    rebuild: DesktopRebuild,
    owner: Arc<Mutex<WriterOwner>>,
    auth: AuthRoutes,
    origin: &str,
    api_key_tenant: &str,
    community_manifests: Option<&Path>,
    server: ServerOptions,
) -> Result<Option<Router>> {
    if !rebuild.enabled() {
        return Ok(None);
    }
    let policy = auth.policy;
    let (
        RebuildHttpClients {
            auth: auth_client,
            drafts: drafts_client,
            builds: builds_client,
        },
        accepted_client,
        history_client,
        planning_client,
        workflow_client,
        (admin_files, app_settings, features, filament_client),
        events_client,
        (checkoff_client, settings),
        plates_client,
    ) = {
        let guard = owner
            .lock()
            .map_err(|_| anyhow!("rebuild owner lock poisoned"))?;
        (
            clients(rebuild, &guard, policy, auth.lifetimes, community_manifests)?
                .ok_or_else(|| anyhow!("flag on constructs clients"))?,
            guard.accepted_reads_with_policy(policy)?,
            guard.build_history(policy)?,
            guard.build_planning(policy, source_reads(&guard, policy, None)?)?,
            // No due-date field or Spoolman shortfall exists yet (slice 7,
            // GRE-1100/GRE-1095; shortfall PR #1479), so their inputs are pending.
            guard.build_workflow(policy, Arc::new(PendingInputs))?,
            (
                guard.admin_files()?,
                guard.app_settings_with_policy(policy)?,
                guard.features_with_policy(policy)?,
                guard.filaments_with_policy(policy)?,
            ),
            guard.domain_events_with_policy(policy)?,
            (guard.checkoff_progress_with_policy(policy)?, guard.client()),
            guard.accepted_plates(policy)?,
        )
    };
    let endpoint = (origin, api_key_tenant);
    let config = AcceptedHttpConfig::from_startup(origin, api_key_tenant, Some("1"))?;
    // Draft HTTP uses `draft_router` (GRE-820) from the same owner when composed
    // separately; MCP's Plan Accept uses this bundle's draft client (GRE-1236).
    let builds = build_router(
        BuildHttpConfig::from_startup(origin, api_key_tenant, Some("1"))?,
        builds_client.clone(),
    );
    let catalog = admit_catalog_routes(
        CatalogHttpConfig::from_startup(origin, api_key_tenant, policy, Some("1"))?,
        Arc::clone(&owner),
    );
    let admin_config = AdminHttpConfig::from_startup(origin, api_key_tenant, Some("1"))?;
    let admin = admin_router(
        admin_config.clone(),
        accepted_client.clone(),
        admin_files.clone(),
    );
    let probe = crate::printer_host::probe::router(admin_config.clone(), accepted_client.clone())?;
    let filaments = filaments(admin_config.clone(), &accepted_client, filament_client)?;
    let modules = setup_routes(&owner, policy, config.clone(), features)?;
    let admin_config = admin_config.with_server_update_checks(server.update_checks);
    let settings_routes = settings_router(admin_config, accepted_client.clone(), app_settings);
    let accepted = accepted_routes(config.clone(), accepted_client.clone())?;
    let history = history_router(config.clone(), history_client);
    let planning = planning_router(config.clone(), planning_client.clone());
    let workflow = workflow_router(config.clone(), workflow_client.clone());
    let events = events_router(config.clone(), events_client, StreamLimits::default());
    let checkoff = checkoff_routes(config.clone(), checkoff_client.clone(), settings)?;
    let plates = plates_router(config.clone(), plates_client);
    let mcp = mcp_router(
        mcp_config(origin, api_key_tenant, server)?,
        auth_client.clone(),
        McpClients {
            builds: builds_client,
            workflow: workflow_client,
            drafts: drafts_client,
            planning: planning_client,
            progress: checkoff_client,
            accepted: accepted_client,
        },
    );
    let auth_routes = auth_router(auth.http, auth_client, auth.providers, auth.mail);
    Ok(Some(
        auth_routes
            .merge(builds)
            .merge(catalog)
            .merge(accepted)
            .merge(history)
            .merge(planning)
            .merge(workflow)
            .merge(diagnostics::router(&owner, policy, admin_files, endpoint)?)
            .merge(admin)
            .merge(modules)
            .merge(filaments)
            .merge(settings_routes)
            .merge(probe)
            .merge(events)
            .merge(checkoff)
            .merge(plates)
            .merge(mcp),
    ))
}

/// Compose completed draft read, write and Accept routers from one
/// `WriterOwner` when the stored rebuild setting is on. Root and `/api/v2`
/// aliases share that client's executor. Returns `None` before creating
/// clients or mounting when the setting is off. Does not cut over production
/// gateway ownership (GRE-593) and does not fall back to Node on Rust failure.
///
/// `origin` is the loopback listener origin. `api_key_tenant` is the configured
/// API-key tenant for draft admission.
pub fn draft_router(
    rebuild: DesktopRebuild,
    owner: &WriterOwner,
    policy: AuthPolicy,
    lifetimes: AuthLifetimes,
    origin: &str,
    api_key_tenant: &str,
    community_manifests: Option<&Path>,
) -> Result<Option<Router>> {
    if !rebuild.enabled() {
        return Ok(None);
    }
    let RebuildHttpClients {
        auth,
        drafts,
        builds: _,
    } = clients(rebuild, owner, policy, lifetimes, community_manifests)?
        .ok_or_else(|| anyhow!("flag on constructs clients"))?;
    Ok(Some(api_draft_router(
        DraftHttpConfig::from_startup(origin, api_key_tenant, Some("1"))?,
        drafts,
        auth,
    )))
}

/// Compose the completed jobs list, read-one, and `/ws/jobs/{id}` router when
/// the stored rebuild setting is on. The listener `origin` is the current
/// bind. A request for another host or origin is refused before subscribe.
/// Returns `None` before creating a jobs client when the setting is off.
/// Owner shutdown closes live sockets through the existing subscription
/// registry. Production gateway cutover stays with GRE-593, and the shutdown
/// drain algorithm stays with GRE-448.
pub fn job_router(
    rebuild: DesktopRebuild,
    owner: &WriterOwner,
    policy: AuthPolicy,
    origin: &str,
    api_key_tenant: &str,
) -> Result<Option<Router>> {
    if !rebuild.enabled() {
        return Ok(None);
    }
    let client = owner.jobs(policy)?;
    Ok(Some(api_jobs_router(
        JobsHttpConfig::from_startup(origin, api_key_tenant, Some("1"))?,
        client,
    )))
}

/// The single-user auth policy the Rust-only runtime serves.
pub fn desktop_policy() -> AuthPolicy {
    AuthPolicy {
        registration: RegistrationPolicy::FirstAccountOnly,
        first_user: FirstUserTenant::NewUser,
        session_tenant: SessionTenantPolicy::SingleAccountDefault,
    }
}

/// The product routes a Rust-only runtime mounts in the gateway: the read
/// routers, the draft routers and the jobs router, all from one owner.
/// `community_manifests` is the bundle's
/// `pp_compat::Bundle::community_manifests_dir`. Returns `None` before
/// creating any client unless the stored setting is on.
pub fn product_router(
    rebuild: DesktopRebuild,
    owner: Arc<Mutex<WriterOwner>>,
    origin: &str,
    community_manifests: Option<&Path>,
) -> Result<Option<Router>> {
    product_router_with_updates(
        rebuild,
        owner,
        origin,
        community_manifests,
        ServerOptions::default(),
    )
}

pub(crate) fn product_router_with_updates(
    rebuild: DesktopRebuild,
    owner: Arc<Mutex<WriterOwner>>,
    origin: &str,
    community_manifests: Option<&Path>,
    server: ServerOptions,
) -> Result<Option<Router>> {
    if !rebuild.enabled() {
        return Ok(None);
    }
    let policy = desktop_policy();
    let lifetimes = AuthLifetimes::default();
    let settings = owner
        .lock()
        .map_err(|_| anyhow!("rebuild owner lock poisoned"))?
        .client();
    let auth = AuthRoutes {
        http: AuthHttpConfig::new(origin, CookieTransport::LoopbackHttp, false, None, false)?
            .with_tenant_feature_toggles()
            .with_modules(settings, DESKTOP_TENANT),
        policy,
        lifetimes,
        providers: ProviderClient::new(None, None)?,
        mail: ResetMailer::disabled(),
    };
    let (drafts, jobs) = {
        let guard = owner
            .lock()
            .map_err(|_| anyhow!("rebuild owner lock poisoned"))?;
        (
            draft_router(
                rebuild,
                &guard,
                policy,
                lifetimes,
                origin,
                DESKTOP_TENANT,
                community_manifests,
            )?,
            job_router(rebuild, &guard, policy, origin, DESKTOP_TENANT)?,
        )
    };
    let reads = read_router_with_updates(
        rebuild,
        owner,
        auth,
        origin,
        DESKTOP_TENANT,
        community_manifests,
        server,
    )?;
    Ok(reads
        .zip(drafts)
        .zip(jobs)
        .map(|((reads, drafts), jobs)| reads.merge(drafts).merge(jobs)))
}

/// API keys route to the default tenant under the single-account policy.
pub(crate) const DESKTOP_TENANT: &str = "default";

fn filaments(
    config: AdminHttpConfig,
    reads: &pp_storage::read_model::ReadClient,
    client: pp_storage::filaments::FilamentClient,
) -> Result<Router> {
    Ok(pp_api::filaments::filament_router(
        config,
        reads.clone(),
        client,
        Arc::new(crate::spoolman::SpoolmanInventory::new()?),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pp_storage::{Limits, auth::Secret, build_graph::BuildCommand, read_model::Credential};
    use serde_json::{Value, json};
    use std::{
        sync::{Arc, atomic::AtomicBool},
        time::Duration,
    };

    fn lifetimes() -> AuthLifetimes {
        AuthLifetimes::shorter(Duration::from_secs(3600), Some(Duration::from_secs(600))).unwrap()
    }

    fn routes(origin: &str) -> AuthRoutes {
        AuthRoutes {
            http: AuthHttpConfig::new(origin, CookieTransport::LoopbackHttp, false, None, false)
                .unwrap(),
            policy: desktop_policy(),
            lifetimes: lifetimes(),
            providers: ProviderClient::new(None, None).unwrap(),
            mail: ResetMailer::disabled(),
        }
    }

    fn owner() -> (WriterOwner, std::path::PathBuf) {
        let directory = std::env::temp_dir().join(format!(
            "pp-rebuild-api-{}",
            hex::encode(rand::random::<[u8; 8]>())
        ));
        (
            WriterOwner::open(&directory, Limits::default()).unwrap().0,
            directory,
        )
    }

    /// The flag-on client bundle over `owner` with the desktop policy.
    fn enabled_clients(owner: &WriterOwner) -> RebuildHttpClients {
        clients(
            DesktopRebuild::from_value(Some("1")),
            owner,
            desktop_policy(),
            lifetimes(),
            None,
        )
        .unwrap()
        .expect("flag on constructs clients")
    }

    /// A fresh writer plus a loopback listener and its origin.
    async fn bound() -> (
        WriterOwner,
        std::path::PathBuf,
        tokio::net::TcpListener,
        String,
    ) {
        let (owner, directory) = owner();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        (owner, directory, listener, origin)
    }

    #[test]
    fn runtime_setting_off_registers_no_routes() {
        let (owner, directory) = owner();
        for value in [None, Some("0"), Some("false"), Some("TRUE")] {
            let rebuild = DesktopRebuild::from_value(value);
            assert!(
                router(rebuild, &owner, routes("http://127.0.0.1:9"))
                    .unwrap()
                    .is_none(),
                "{value:?}"
            );
        }
        owner.shutdown().unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// S0-02: with the feature on and `PP_DESKTOP_REBUILD` unset, no Rust
    /// product route answers. Every mount entry point returns no router.
    #[test]
    fn s0_02_switch_unset_mounts_no_rust_product_route() {
        let origin = "http://127.0.0.1:9";
        let rebuild = DesktopRebuild::from_value(None);
        assert!(!rebuild.enabled());
        let (owner, directory) = owner();
        assert!(router(rebuild, &owner, routes(origin)).unwrap().is_none());
        assert!(
            draft_router(
                rebuild,
                &owner,
                desktop_policy(),
                lifetimes(),
                origin,
                "default",
                None
            )
            .unwrap()
            .is_none()
        );
        assert!(
            job_router(rebuild, &owner, desktop_policy(), origin, "default")
                .unwrap()
                .is_none()
        );
        let owner = Arc::new(Mutex::new(owner));
        assert!(
            read_router(
                rebuild,
                Arc::clone(&owner),
                routes(origin),
                origin,
                "default",
                None
            )
            .unwrap()
            .is_none()
        );
        let owner = Arc::into_inner(owner).unwrap().into_inner().unwrap();
        owner.shutdown().unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn application_host_flag_off_constructs_no_rebuild_clients() {
        let (owner, directory) = owner();
        for value in [None, Some("0"), Some("false"), Some("TRUE")] {
            assert!(
                clients(
                    DesktopRebuild::from_value(value),
                    &owner,
                    desktop_policy(),
                    lifetimes(),
                    None,
                )
                .unwrap()
                .is_none(),
                "{value:?}"
            );
        }
        owner.shutdown().unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// GRE-1056: `clients` reads Sources with the community manifests root it
    /// was given and Node's filesystem policy for the auth policy.
    #[test]
    fn ticket_t_10_draftreads_clients_wire_manifests_and_filesystem_policy() {
        let (owner, directory) = owner();
        // ast-grep-ignore: no-writer-family-literal -- A test path for the read-only community manifests root.
        let manifests = directory.join("bundle/data/manifests");
        let reads = source_reads(&owner, desktop_policy(), Some(&manifests)).unwrap();
        assert_eq!(reads.community_manifests(), Some(manifests.as_path()));
        assert_eq!(
            reads.filesystem_policy(),
            SourceFilesystemPolicy::TrustedSingleUser
        );
        let multi_user = AuthPolicy {
            session_tenant: SessionTenantPolicy::AccountTenant,
            ..desktop_policy()
        };
        let reads = source_reads(&owner, multi_user, None).unwrap();
        assert_eq!(reads.community_manifests(), None);
        assert_eq!(reads.filesystem_policy(), SourceFilesystemPolicy::Isolated);
        owner.shutdown().unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// T-10-draftreads (GRE-1052): production draft clients read Sources, so
    /// recompute reaches authentication instead of refusing for missing reads.
    #[test]
    fn ticket_t_10_draftreads_production_clients_read_sources() {
        let (owner, directory) = owner();
        let bundle = enabled_clients(&owner);
        let refused = bundle
            .drafts
            .recompute(
                Credential::Session(Secret::new("not-a-session".into())),
                pp_storage::working_drafts::PositiveId::new(1).unwrap(),
                pp_storage::working_drafts::RecomputeRequest::new("draftreads", true).unwrap(),
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(2),
            )
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("Authentication required") || refused.contains("Invalid session"),
            "{refused}"
        );
        bundle.close();
        owner.shutdown().unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn application_host_closes_owned_clients() {
        let (owner, directory) = owner();
        let bundle = enabled_clients(&owner);
        // Policy-backed Build client refuses a missing session (not LocalOwner).
        let refused = bundle
            .builds
            .execute(
                Credential::Session(Secret::new("not-a-session".into())),
                BuildCommand::List,
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(2),
            )
            .unwrap_err();
        let message = refused.to_string();
        assert!(
            message.contains("Authentication required") || message.contains("Invalid session"),
            "{message}"
        );
        bundle.close();
        owner.shutdown().unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// Serves `router` on `listener`; `Serving::stop` returns once no connection
    /// holds the router, so the owner can then be taken back.
    fn serve(listener: tokio::net::TcpListener, router: axum::Router) -> pp_api::serving::Serving {
        pp_api::serving::Serving::start(listener, router)
    }

    /// Registers the first account and returns its session cookie.
    async fn session(client: &reqwest::Client, origin: &str, email: &str) -> String {
        let registered = client
            .post(format!("{origin}/auth/register"))
            .header("Origin", origin)
            .json(&json!({"email": email, "password": "password-1234"}))
            .send()
            .await
            .unwrap();
        assert_eq!(registered.status(), 200);
        let cookie = registered.headers()["set-cookie"].to_str().unwrap();
        cookie.split(';').next().unwrap().to_owned()
    }

    /// A second session for the account `session` registered.
    async fn sign_in(client: &reqwest::Client, origin: &str) -> String {
        let body = Some(sample_body("SignInRequest"));
        let request = contract_request(client, origin, ("POST", "/auth/login"), &[], None, body);
        let signed_in = request.send().await.unwrap();
        assert_eq!(signed_in.status(), 200);
        let cookie = signed_in.headers()["set-cookie"].to_str().unwrap();
        cookie.split(';').next().unwrap().to_owned()
    }

    /// The single owner already exists, so a second register is refused by its
    /// handler, which still shows the route runs. The auth guard's own 403s (origin,
    /// host, loopback) carry other details and do not count.
    fn owner_exists(
        route: &pp_api::client_contract::ClientRoute,
        status: u16,
        body: &Value,
    ) -> bool {
        route.path == "/auth/register"
            && status == 403
            && body["detail"] == pp_storage::auth::AuthFailure::SingleAccountExists.to_string()
    }

    /// A body each client route's request DTO accepts.
    fn sample_body(dto: &str) -> Value {
        match dto {
            "EditDraftPartsRequest" => json!({"expected_snapshot_digest": "0".repeat(64),
                "decision": {"kind": "set_included", "draft_part_ids": [1], "value": true}}),
            "AbandonDraftRequest" => json!({"expected_lifecycle_version": 0}),
            "PrinterProbeRequest" => {
                json!({"kind":"unsupported", "address":"http://fixture.invalid"})
            }
            "SetModuleRequest" => json!({"on": true}),
            "CreateIntegrationRequest" => {
                json!({"kind": "moonraker", "name": "Client contract", "base_url": "http://fixture.invalid"})
            }
            "CreatePrinterRequest" => json!({"name": "Client contract", "model": "fixture",
                "bed_width_mm": 250, "bed_depth_mm": 210, "bed_height_mm": 220}),
            "UpdatePrinterRequest" => json!({"name": "Client contract"}),
            "SaveDraftHeaderRequest" => {
                json!({"expected_base": {"revision_id": null, "plan_version": 0},
                "expected_draft": null, "expected_header_version": null,
                "header": {"name": "Client contract", "order_number": null, "customer": null, "due_date": null}})
            }
            "SavePlanChoicesRequest" => {
                json!({"expected_base": {"revision_id": null, "plan_version": 0},
                "expected_draft": null, "remap_checkoff_links": false, "decisions": [{"kind": "set_included",
                    "target": {"part_key": "client.stl", "relative_path": "client.stl", "source_layer": null},
                    "value": true}]})
            }
            "AcceptDraftRequest" => json!({"expected_snapshot_digest": "0".repeat(64),
                "expected_lifecycle_version": 0,
                "expected_base": {"revision_id": null, "plan_version": 0}}),
            "ReconcileDraftRequest" => {
                json!({"expected_snapshot_digest": "0".repeat(64), "decisions": []})
            }
            "RestoreSnapshotRequest" => json!({"mode": "draft",
                "expected_base": {"revision_id": null, "plan_version": 0}, "expected_draft": null}),
            "VerifyBatchRequest" => json!({"idempotency_key": "client-contract",
                "beds": [{"link_id": "no-such-link", "decisions": [
                    {"part_id": 1, "unit_index": 0, "result": "confirmed"}]}]}),
            "UndoBatchRequest" => json!({"undo_token": "ppb_unknown"}),
            "ManualBurstRequest" => json!({"idempotency_key": "client-contract",
                "expected": {"profile_id": 1, "plan_version": 1, "plan_revision_id": 1,
                    "plan_revision_digest": "0".repeat(64), "required_unit_mapping_digest": "0".repeat(64)},
                "units": [{"token": format!("ppu_{}", "0".repeat(32)), "printed": true}]}),
            "ClaimUnattributedRequest" => json!({"profile_id": 1}),
            "DismissUnattributedRequest" => json!({}),
            "SignInRequest" => json!({"email": "client@example.com", "password": "password-1234"}),
            "RegisterRequest" => {
                json!({"email": "second@example.com", "password": "password-1234"})
            }
            other => panic!("no sample body for {other}"),
        }
    }

    /// One request to the composed routers, with the route's required
    /// headers, an optional session cookie and an optional JSON body.
    fn contract_request(
        client: &reqwest::Client,
        origin: &str,
        (method, path): (&str, &str),
        headers: &[&str],
        cookie: Option<&str>,
        body: Option<Value>,
    ) -> reqwest::RequestBuilder {
        let mut request = client
            .request(method.parse().unwrap(), format!("{origin}{path}"))
            .header("Origin", origin);
        for header in headers {
            request = request.header(*header, "client-contract");
        }
        if let Some(cookie) = cookie {
            request = request.header("Cookie", cookie);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        request
    }

    /// Every `pp_api::client_contract::CLIENT_ROUTES` entry, which generates
    /// `web/packages/rust-client`, is registered on the composed routers with
    /// its stated method and authentication.
    struct NoObservations;

    impl pp_api::farm::FarmObservations for NoObservations {
        fn observe(
            &self,
            _: &pp_storage::read_model::farm::PrinterIntegration,
        ) -> pp_storage::read_model::farm::Observation {
            pp_storage::read_model::farm::Observation::Unavailable
        }
    }

    /// Whether an anonymous request got the answer the route's stated auth predicts.
    fn answers_without_credential(
        route: &pp_api::client_contract::ClientRoute,
        status: u16,
        body: &Value,
    ) -> bool {
        match route.auth {
            pp_api::client_contract::ClientAuth::Credential => status == 401,
            pp_api::client_contract::ClientAuth::None => {
                status == 200 || owner_exists(route, status, body)
            }
        }
    }

    /// A 200, or a refusal only the route's own handler gives for the sample
    /// request.
    fn reached_handler(
        route: &pp_api::client_contract::ClientRoute,
        status: u16,
        body: &Value,
    ) -> bool {
        // A manual burst names an Accepted Plan basis no stored Build has.
        let stale = status == 409 && body["detail"] == "Accepted Plan changed; reload and retry";
        // A draft header save on a Build with no layers can prepare no draft.
        let no_layers = status == 422 && body["code"] == "no_layers";
        status == 200
            || handler_not_found(status, body)
            || stale
            || no_layers
            || owner_exists(route, status, body)
    }

    /// The sample bodies name no stored draft, snapshot, link, burst, History entry or
    /// print, so those handlers answer their own 404 with a code or detail.
    fn handler_not_found(status: u16, body: &Value) -> bool {
        const CODES: [&str; 5] = [
            "integration_not_found",
            "printer_not_found",
            "draft_not_found",
            "snapshot_not_found",
            "farm_log_entry_not_found",
        ];
        const DETAILS: [&str; 4] = [
            "beds[0]: Checkoff link not found",
            "Checkoff burst not found",
            "History entry not found",
            "Unattributed print not found",
        ];
        status == 404
            && (CODES.iter().any(|code| body["code"] == *code)
                || DETAILS.iter().any(|detail| body["detail"] == *detail))
    }

    #[tokio::test]
    async fn client_routes_match_registrations() {
        use pp_api::client_contract::CLIENT_ROUTES;
        let (owner, directory, listener, origin) = bound().await;
        let rebuild = DesktopRebuild::from_value(Some("1"));
        let (policy, lifetimes) = (desktop_policy(), lifetimes());
        let drafts = draft_router(rebuild, &owner, policy, lifetimes, &origin, "default", None);
        let owner = Arc::new(std::sync::Mutex::new(owner));
        let reads = read_router(
            rebuild,
            Arc::clone(&owner),
            routes(&origin),
            &origin,
            "default",
            None,
        );
        let farm = pp_api::farm::farm_router(
            pp_api::accepted::AcceptedHttpConfig::from_startup(&origin, "default", Some("1"))
                .unwrap(),
            owner.lock().unwrap().farm_reads(policy).unwrap(),
            Arc::new(NoObservations),
        );
        let mut server = serve(
            listener,
            reads
                .unwrap()
                .unwrap()
                .merge(drafts.unwrap().unwrap())
                .merge(farm),
        );
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let send = |method: &str, path: &str, headers: &[&str], cookie: Option<&str>, body| {
            contract_request(&client, &origin, (method, path), headers, cookie, body).send()
        };
        let cookie = session(&client, &origin, "client@example.com").await;
        let created = send(
            "POST",
            "/api/v2/plans",
            &[],
            Some(&cookie),
            Some(json!({"name": "Client contract"})),
        );
        let created: Value = created.await.unwrap().json().await.unwrap();
        let build = created["id"]
            .as_i64()
            .expect("created Build id")
            .to_string();
        // An unregistered path answers 404 with no body; a registered one
        // answers from its handler.
        let unregistered = send(
            "GET",
            &format!("/api/v2/plans/{build}/nope"),
            &[],
            Some(&cookie),
            None,
        );
        let unregistered = unregistered.await.unwrap();
        assert_eq!(unregistered.status(), 404);
        assert_eq!(unregistered.text().await.unwrap(), "");
        // Sign-out revokes the session it is sent with, so it gets its own.
        let spare = sign_in(&client, &origin).await;
        for route in CLIENT_ROUTES
            .iter()
            .chain(std::iter::once(&pp_api::printer_host::probe::CLIENT_ROUTE))
        {
            let path = route
                .path
                .replace("{id}", &build)
                .replace("{draft_id}", "1")
                .replace("{snapshot_id}", "1")
                .replace("{module}", "orders");
            let body = route.request.map(sample_body);
            let anonymous = send(route.method, &path, route.headers, None, body.clone())
                .await
                .unwrap();
            let anonymous_status = anonymous.status().as_u16();
            let anonymous_body: Value = anonymous.json().await.unwrap_or(Value::Null);
            assert!(
                answers_without_credential(route, anonymous_status, &anonymous_body),
                "{} {path} without credentials: {anonymous_status} {anonymous_body}",
                route.method
            );
            let credential = if route.path == "/auth/logout" {
                &spare
            } else {
                &cookie
            };
            let response = send(route.method, &path, route.headers, Some(credential), body)
                .await
                .unwrap();
            let status = response.status().as_u16();
            let body: Value = response.json().await.unwrap_or(Value::Null);
            assert!(
                reached_handler(route, status, &body),
                "{} {path}: {status} {body}",
                route.method
            );
        }
        server.stop().await.unwrap();
        let owner = Arc::into_inner(owner).expect("routers dropped");
        owner.into_inner().unwrap().shutdown().unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn runtime_setting_on_serves_auth_through_the_rebuild_client() {
        let (owner, directory, listener, origin) = bound().await;
        let router = router(
            DesktopRebuild::from_value(Some("1")),
            &owner,
            routes(&origin),
        )
        .unwrap()
        .unwrap();
        let mut server = serve(listener, router);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        // `/auth/status` says only whether the owner account is still missing.
        let status = || async {
            let response = client
                .get(format!("{origin}/auth/status"))
                .header("Origin", &origin)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            response.json::<Value>().await.unwrap()
        };
        assert_eq!(status().await, json!({"setup_required": true}));
        let cookie = session(&client, &origin, "rebuild@example.com").await;
        assert_eq!(status().await, json!({"setup_required": false}));
        // A register the auth guard refuses (foreign Origin) is never read as the
        // handler's owner-exists answer; one that reaches the handler is.
        let register = pp_api::client_contract::CLIENT_ROUTES
            .iter()
            .find(|route| route.name == "registerOwner")
            .unwrap();
        for (sent_origin, handled) in [("http://other.invalid", false), (origin.as_str(), true)] {
            let refused = client
                .post(format!("{origin}/auth/register"))
                .header("Origin", sent_origin)
                .json(&sample_body("RegisterRequest"))
                .send()
                .await
                .unwrap();
            let refused_status = refused.status().as_u16();
            let body: Value = refused.json().await.unwrap_or(Value::Null);
            assert_eq!(refused_status, 403, "{sent_origin}: {body}");
            assert_eq!(
                owner_exists(register, refused_status, &body),
                handled,
                "{sent_origin}: {body}"
            );
        }
        let me: Value = client
            .get(format!("{origin}/auth/me"))
            .header("Origin", &origin)
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(me["user"]["email"], "rebuild@example.com");
        let key = client
            .post(format!("{origin}/settings/api-keys"))
            .header("Origin", &origin)
            .header("Cookie", &cookie)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(key.status(), 201);
        let key: Value = key.json().await.unwrap();
        // Only the rebuild client's lifetimes give API keys an expiry.
        assert!(key["expiresAt"].is_string(), "{key}");
        // Signing out leaves the account, so setup stays done.
        let out = client
            .post(format!("{origin}/auth/logout"))
            .header("Origin", &origin)
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(out.status(), 200);
        assert_eq!(status().await, json!({"setup_required": false}));
        server.stop().await.unwrap();
        owner.shutdown().unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }
}
