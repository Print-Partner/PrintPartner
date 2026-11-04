use crate::DesktopRebuild;
use anyhow::{Result, anyhow};
use axum::Router;
use pp_api::{
    accepted::{AcceptedHttpConfig, accepted_router},
    admin::{AdminHttpConfig, admin_router},
    auth::auth_router,
    builds::{BuildHttpConfig, build_router},
    catalog::{CatalogHttpConfig, admit_catalog_routes},
    drafts::{DraftHttpConfig, draft_router as api_draft_router},
    history::history_router,
    jobs::{JobsHttpConfig, jobs_router as api_jobs_router},
    mcp::{McpHttpConfig, mcp_router},
};
// Re-export so desktop_rebuild integration tests can build AuthRoutes without a
// non-optional pp-api [dev-dependencies] edge (gated boundary check).
pub use pp_api::auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer};
use pp_storage::{
    WriterOwner,
    auth::{
        AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy,
        rebuild::AuthLifetimes, rebuild::RebuildAuthClient,
    },
    build_graph::BuildGraphClient,
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

/// Compose authenticated read routers (auth, Plan builds, catalog, Accepted
/// Plan Checkoff, Build history, legal documents, custom filaments and
/// key-only MCP) from one
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
    if !rebuild.enabled() {
        return Ok(None);
    }
    let policy = auth.policy;
    let (
        RebuildHttpClients {
            auth: auth_client,
            drafts: _drafts,
            builds: builds_client,
        },
        accepted_client,
        history_client,
        admin_files,
    ) = {
        let guard = owner
            .lock()
            .map_err(|_| anyhow!("rebuild owner lock poisoned"))?;
        (
            clients(rebuild, &guard, policy, auth.lifetimes, community_manifests)?
                .ok_or_else(|| anyhow!("flag on constructs clients"))?,
            guard.accepted_reads_with_policy(policy)?,
            guard.build_history(policy)?,
            guard.admin_files()?,
        )
    };
    // `_drafts` is opened with the bundle for GRE-818 parity; draft HTTP uses
    // `draft_router` (GRE-820) from the same owner when composed separately.
    let builds = build_router(
        BuildHttpConfig::from_startup(origin, api_key_tenant, Some("1"))?,
        builds_client,
    );
    let catalog = admit_catalog_routes(
        CatalogHttpConfig::from_startup(origin, api_key_tenant, policy, Some("1"))?,
        Arc::clone(&owner),
    );
    let admin = admin_router(
        AdminHttpConfig::from_startup(origin, api_key_tenant, Some("1"))?,
        accepted_client.clone(),
        admin_files,
    );
    let accepted = accepted_router(
        AcceptedHttpConfig::from_startup(origin, api_key_tenant, Some("1"))?,
        accepted_client,
    );
    let history = history_router(
        AcceptedHttpConfig::from_startup(origin, api_key_tenant, Some("1"))?,
        history_client,
    );
    let mcp = mcp_router(
        McpHttpConfig::from_startup(origin, api_key_tenant, Some("1"))?,
        auth_client.clone(),
    );
    let auth_routes = auth_router(auth.http, auth_client, auth.providers, auth.mail);
    Ok(Some(
        auth_routes
            .merge(builds)
            .merge(catalog)
            .merge(accepted)
            .merge(history)
            .merge(admin)
            .merge(mcp),
    ))
}

/// Compose completed draft list/read/edit/abandon routers from one
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
    if !rebuild.enabled() {
        return Ok(None);
    }
    let policy = desktop_policy();
    let lifetimes = AuthLifetimes::default();
    let auth = AuthRoutes {
        http: AuthHttpConfig::new(origin, CookieTransport::LoopbackHttp, false, None, false)?
            .with_tenant_feature_toggles(),
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
    let reads = read_router(
        rebuild,
        owner,
        auth,
        origin,
        DESKTOP_TENANT,
        community_manifests,
    )?;
    Ok(reads
        .zip(drafts)
        .zip(jobs)
        .map(|((reads, drafts), jobs)| reads.merge(drafts).merge(jobs)))
}

/// API keys route to the default tenant under the single-account policy.
const DESKTOP_TENANT: &str = "default";

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

    /// Serves `router` on `listener` until the returned sender fires.
    fn serve(
        listener: tokio::net::TcpListener,
        router: axum::Router,
    ) -> (
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
        });
        (stop, server)
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

    /// A body each client route's request DTO accepts.
    fn sample_body(dto: &str) -> Value {
        match dto {
            "EditDraftPartsRequest" => json!({"expected_snapshot_digest": "0".repeat(64),
                "decision": {"kind": "set_included", "draft_part_ids": [1], "value": true}}),
            "AbandonDraftRequest" => json!({"expected_lifecycle_version": 0}),
            other => panic!("no sample body for {other}"),
        }
    }

    /// Every `pp_api::client_contract::CLIENT_ROUTES` entry, which generates
    /// `web/packages/rust-client`, is registered on the composed routers with
    /// its stated method and authentication.
    #[tokio::test]
    async fn client_routes_match_registrations() {
        use pp_api::client_contract::{CLIENT_ROUTES, ClientAuth};
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
        let (stop, server) = serve(
            listener,
            reads.unwrap().unwrap().merge(drafts.unwrap().unwrap()),
        );
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let send = |method: &str, path: &str, cookie: Option<&str>, body: Option<Value>| {
            let mut request = client
                .request(method.parse().unwrap(), format!("{origin}{path}"))
                .header("Origin", &origin);
            if let Some(cookie) = cookie {
                request = request.header("Cookie", cookie);
            }
            if let Some(body) = body {
                request = request.json(&body);
            }
            request.send()
        };
        let cookie = session(&client, &origin, "client@example.com").await;
        let created = send(
            "POST",
            "/api/v2/plans",
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
            Some(&cookie),
            None,
        );
        let unregistered = unregistered.await.unwrap();
        assert_eq!(unregistered.status(), 404);
        assert_eq!(unregistered.text().await.unwrap(), "");
        for route in CLIENT_ROUTES {
            let path = route
                .path
                .replace("{id}", &build)
                .replace("{draft_id}", "1");
            let body = route.request.map(sample_body);
            let anonymous = send(route.method, &path, None, body.clone()).await.unwrap();
            let expected = match route.auth {
                ClientAuth::Credential => 401,
                ClientAuth::None => 200,
            };
            assert_eq!(
                anonymous.status(),
                expected,
                "{} {path} without credentials",
                route.method
            );
            let response = send(route.method, &path, Some(&cookie), body)
                .await
                .unwrap();
            let status = response.status().as_u16();
            let body: Value = response.json().await.unwrap_or(Value::Null);
            assert!(
                status == 200 || (status == 404 && body["code"] == "draft_not_found"),
                "{} {path}: {status} {body}",
                route.method
            );
        }
        stop.send(()).unwrap();
        server.await.unwrap();
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
        let (stop, server) = serve(listener, router);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let cookie = session(&client, &origin, "rebuild@example.com").await;
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
        stop.send(()).unwrap();
        server.await.unwrap();
        owner.shutdown().unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }
}
