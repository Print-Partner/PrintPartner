//! Streamable HTTP MCP on the rebuild loopback listener (T-16, GRE-348).
//!
//! `/mcp` and `/api/v1/mcp` serve the pp-services product catalog through
//! rmcp. Every request needs a valid API key for the routed tenant: a session
//! cookie alone is refused, and so is a key the auth worker does not resolve
//! to that tenant (unknown, revoked or expired). Host and Origin must match
//! the listener exactly. A request with a valid key then counts against
//! MCP's shared budget of 1000 a minute, Node's limit; one without counts
//! against its peer's own bucket, and a peer that used up its bucket is
//! refused before its key is checked (`mcp/rate_limit.rs`). A session answers
//! only the key that opened it; any other key gets `404` for it
//! (`mcp/session_keys.rs`). Write tools only propose; `confirm_apply` applies.
//! `propose_verify_printer_checkoff` applies as a Checkoff burst on storage
//! (GRE-1170, `mcp/checkoff.rs`). `list_plans`, `get_plan_decisions` and
//! `list_plan_snapshots` read storage with the caller's key (`mcp/reads.rs`),
//! and so do `get_remaining` and `get_printer_checkoff`
//! (`mcp/progress_reads.rs`). `propose_create_build` creates a Build with
//! its planning brief (`mcp/create_build.rs`), and
//! `propose_set_build_source_roles` sets the Build's own Source roles in that
//! brief (`mcp/source_roles.rs`), and `propose_import_build_inputs` attaches
//! Library Sources to it (`mcp/import_inputs.rs`). `get_build_planning_state` and
//! `get_plan_draft` read the brief and draft (`mcp/planning_reads.rs`).
//! Refusals of these tools carry a code from `mcp/write_result.rs`.
//! On the headless explicit listener (`PP_LISTEN_ADDR`), the same key guard
//! admits any peer the listener admits; Host and Origin are then checked
//! against the listener's validated authority (`NetworkAdmission`), and no
//! peer is trusted by its address.

use crate::loopback::{
    LoopbackOrigin, PresentedCredential, admit, principal, rebuild_flag_enabled,
    require_api_key_tenant,
};
use anyhow::Result;
use axum::{
    Json, Router,
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use checkoff::CheckoffDomain;
use pp_services::mcp_domain::{McpTools, ToolResult, server_info};
use pp_storage::build_graph::{BuildGraphClient, PositiveId};
use pp_storage::checkoff_progress::ProgressClient;
use pp_storage::read_model::farm::FarmClient;
use pp_storage::{
    auth::{self, KeyPrincipal, Secret, rebuild::RebuildAuthClient},
    read_model::{
        ReadClient,
        build_workflow::{Outcome, Request, WorkflowClient},
        planning::PlanningClient,
    },
    working_drafts::WorkingDraftClient,
};
use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerConfig,
    },
    service::RequestContext,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use tokio::sync::Semaphore;

const KEY_REQUIRED: &str = "Valid API key required";
/// Concurrent key checks; the auth runtime's own lease pool bounds the work.
const ADMISSION_PERMITS: usize = 64;

#[derive(Clone)]
pub struct McpHttpConfig {
    origin: String,
    host: String,
    api_key_tenant: String,
    enabled: bool,
    explicit_listener: bool,
    wait: Duration,
    limit: u32,
    window: Duration,
    #[cfg(test)]
    confirm_gate: Option<Arc<plan_accept::ConfirmGate>>,
    #[cfg(test)]
    clock: Option<Arc<rate_limit::ManualClock>>,
}

impl McpHttpConfig {
    pub fn from_startup(origin: &str, api_key_tenant: &str, flag: Option<&str>) -> Result<Self> {
        let listener = LoopbackOrigin::parse(origin)?;
        require_api_key_tenant(api_key_tenant)?;
        Ok(Self {
            origin: listener.origin,
            host: listener.host,
            api_key_tenant: api_key_tenant.into(),
            enabled: rebuild_flag_enabled(flag),
            explicit_listener: false,
            wait: Duration::from_millis(100),
            limit: rate_limit::LIMIT,
            window: rate_limit::WINDOW,
            #[cfg(test)]
            confirm_gate: None,
            #[cfg(test)]
            clock: None,
        })
    }

    /// Serve the headless explicit listener. Its Host is whatever literal IP
    /// and published port the client used, so rmcp's fixed loopback allowlists
    /// are off and the guard's `admit` checks the validated `NetworkAdmission`
    /// authority instead. Requests without one stay loopback-only.
    pub fn explicit_listener(mut self, on: bool) -> Self {
        self.explicit_listener = on;
        self
    }

    /// A smaller budget on a clock the test advances, so a test can exhaust
    /// and reset it without waiting.
    #[cfg(test)]
    pub(crate) fn with_budget(
        mut self,
        limit: u32,
        window: Duration,
        clock: Arc<rate_limit::ManualClock>,
    ) -> Self {
        self.limit = limit;
        self.window = window;
        self.clock = Some(clock);
        self
    }

    /// Holds each Plan Accept confirm just before it applies.
    #[cfg(test)]
    pub(crate) fn with_confirm_gate(mut self, gate: Arc<plan_accept::ConfirmGate>) -> Self {
        self.confirm_gate = Some(gate);
        self
    }
}

#[derive(Clone)]
struct Guard {
    config: McpHttpConfig,
    auth: RebuildAuthClient,
    admission: Arc<Semaphore>,
    budget: Arc<rate_limit::Budget>,
    failures: Arc<rate_limit::PeerBudgets>,
    sessions: Arc<session_keys::SessionKeys>,
}

/// The clients ported tools use: `get_build_workflow` reads the same client as
/// `GET /plans/{id}/workflow`, and Plan Accept uses the draft routes' client
/// and checks the Build planning brief with the planning routes' client.
/// `progress` applies confirmed Checkoff proposals and reads printer Checkoff.
/// `builds` lists Builds, `planning` also reads decisions and snapshots, and
/// `accepted` reads Accepted Plan progress for the read tools.
#[derive(Clone)]
struct Reads {
    builds: BuildGraphClient,
    workflow: WorkflowClient,
    drafts: WorkingDraftClient,
    planning: PlanningClient,
    progress: ProgressClient,
    accepted: ReadClient,
    review_observer: Option<Arc<dyn McpReviewObserver>>,
    farm: McpFarm,
    wait: Duration,
    #[cfg(test)]
    confirm_gate: Option<Arc<plan_accept::ConfirmGate>>,
}

/// The storage clients MCP tools read and write, each used with the
/// request's API key.
pub struct McpClients {
    pub builds: BuildGraphClient,
    pub workflow: WorkflowClient,
    pub drafts: WorkingDraftClient,
    pub planning: PlanningClient,
    pub progress: ProgressClient,
    pub accepted: ReadClient,
    pub review_observer: Option<Arc<dyn McpReviewObserver>>,
    pub farm: McpFarm,
}

/// Local observations only; MCP Review does not need live filament inventory.
pub trait McpReviewObserver: Send + Sync {
    fn observe(
        &self,
        snapshot: &pp_storage::read_model::Snapshot,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> anyhow::Result<pp_storage::read_model::views::ReviewObservations>;
}

/// What `get_farm_status` and the farm tools' Module gate read: the farm
/// projection `GET /api/v2/printers` serves, the printer watchers'
/// observations, and the Module settings.
#[derive(Clone)]
pub struct McpFarm {
    pub reads: FarmClient,
    pub observations: Arc<dyn crate::farm::FarmObservations>,
    pub modules: pp_storage::SettingsClient,
    /// `get_print_stats` counts the plates sent in its window here.
    pub activity: pp_storage::print_activity::PrintActivityClient,
    /// Spoolman inventory, which `propose_assign_role_filament` checks a
    /// Spoolman filament and spool against at confirm. `None` reads as
    /// unreachable, so a Spoolman assignment is refused.
    pub inventory: Option<Arc<dyn crate::accepted::InventoryReader>>,
}

/// Mounts the MCP transport at `/mcp` and `/api/v1/mcp` when the rebuild flag
/// was on at startup. Otherwise nothing is mounted. `get_build_workflow` reads
/// `workflow` and Plan Accept uses `drafts` and `planning`, each with the
/// request's API key. Checkoff proposals apply through `progress`; `builds`
/// serves `list_plans` and `accepted` serves `get_remaining`.
pub fn mcp_router(config: McpHttpConfig, auth: RebuildAuthClient, clients: McpClients) -> Router {
    if !config.enabled {
        return Router::new();
    }
    // `admit` in the guard is the authoritative Host and Origin check. rmcp's
    // own allowlists repeat the same exact listener values, except on the
    // explicit listener, whose authority varies with the client's address.
    let transport = if config.explicit_listener {
        StreamableHttpServerConfig::default()
            .disable_allowed_hosts()
            .disable_allowed_origins()
    } else {
        StreamableHttpServerConfig::default()
            .with_allowed_hosts([config.host.clone()])
            .with_allowed_origins([config.origin.clone()])
    };
    let McpClients {
        builds,
        workflow,
        drafts,
        planning,
        progress,
        accepted,
        review_observer,
        farm,
    } = clients;
    let reads = Reads {
        builds,
        workflow,
        drafts,
        planning,
        progress,
        accepted,
        review_observer,
        farm,
        wait: config.wait,
        #[cfg(test)]
        confirm_gate: config.confirm_gate.clone(),
    };
    let rule = rate_limit::Rule::new(config.limit, config.window);
    #[cfg(test)]
    let rule = rule.with_clock(config.clock.clone());
    let sessions = Arc::new(LocalSessionManager::default());
    let service = StreamableHttpService::new(
        move || Ok(ProductMcp::new(reads.clone())),
        sessions.clone(),
        transport,
    );
    Router::new()
        .route_service("/mcp", service.clone())
        .route_service("/api/v1/mcp", service)
        .layer(middleware::from_fn_with_state(
            Guard {
                budget: Arc::new(rate_limit::Budget::new(rule.clone())),
                failures: Arc::new(rate_limit::PeerBudgets::new(rule)),
                sessions: Arc::new(session_keys::SessionKeys::new(sessions)),
                config,
                auth,
                admission: Arc::new(Semaphore::new(ADMISSION_PERMITS)),
            },
            guard,
        ))
}

/// The router over every client MCP reads, each opened on `owner` with
/// `policy`, and the auth client that issues its keys. No printer is
/// watched.
#[cfg(test)]
pub(crate) fn test_router(
    owner: &pp_storage::WriterOwner,
    config: McpHttpConfig,
    policy: auth::AuthPolicy,
) -> (Router, RebuildAuthClient) {
    test_router_watching(owner, config, policy, Arc::new(crate::farm::Unwatched))
}

/// The Spoolman stock every test router reads: integration `sm-1` holds
/// filament 3 (`#112233`) with spool 7, and spool 8 of filament 4. Any other
/// integration is unreachable. Nothing is contacted.
#[cfg(test)]
struct TestInventory;

#[cfg(test)]
impl crate::accepted::InventoryReader for TestInventory {
    fn read(
        &self,
        integration: pp_storage::read_model::stock::SpoolmanIntegration,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Option<pp_storage::read_model::stock::Inventory>>
                + Send,
        >,
    > {
        use pp_storage::read_model::stock::{Inventory, InventoryFilament, InventorySpool};
        let stock = (integration.id == "sm-1").then(|| Inventory {
            filaments: [(
                3,
                InventoryFilament {
                    label: "Acme PLA · Night".into(),
                    hex: Some("#112233".into()),
                },
            )]
            .into(),
            spools: [(7, 3), (8, 4)]
                .map(|(id, filament_id)| InventorySpool {
                    id,
                    filament_id,
                    remaining_g: Some(500.0),
                })
                .into(),
        });
        Box::pin(std::future::ready(stock))
    }
}

/// [`test_router`] with the printer watchers' `observations`.
#[cfg(test)]
pub(crate) fn test_router_watching(
    owner: &pp_storage::WriterOwner,
    config: McpHttpConfig,
    policy: auth::AuthPolicy,
    observations: Arc<dyn crate::farm::FarmObservations>,
) -> (Router, RebuildAuthClient) {
    let inventory: Option<Arc<dyn crate::accepted::InventoryReader>> =
        Some(Arc::new(TestInventory));
    let auth = owner
        .rebuild_auth_with_policy(policy, Default::default())
        .unwrap();
    let workflow = owner
        .build_workflow(
            policy,
            Arc::new(pp_storage::read_model::build_workflow::PendingInputs),
        )
        .unwrap();
    let reads = Arc::new(owner.source_reads().unwrap());
    let drafts = owner
        .working_drafts_with_observations(policy, reads, Default::default())
        .unwrap();
    let planning = owner
        .build_planning(policy, owner.source_reads().unwrap())
        .unwrap();
    let progress = owner.checkoff_progress().unwrap();
    let builds = owner
        .build_graph_with_observations(
            policy,
            Arc::new(owner.source_reads().unwrap()),
            Default::default(),
        )
        .unwrap();
    let accepted = owner.accepted_reads_with_policy(policy).unwrap();
    let router = mcp_router(
        config,
        auth.clone(),
        McpClients {
            builds,
            workflow,
            drafts,
            planning,
            progress,
            accepted,
            review_observer: Some(Arc::new(FixtureReviewObserver)),
            farm: McpFarm {
                reads: owner.farm_reads(policy).unwrap(),
                observations,
                modules: owner.client(),
                activity: owner.print_activity(policy).unwrap(),
                inventory,
            },
        },
    );
    (router, auth)
}

#[cfg(test)]
struct FixtureReviewObserver;

#[cfg(test)]
impl McpReviewObserver for FixtureReviewObserver {
    fn observe(
        &self,
        snapshot: &pp_storage::read_model::Snapshot,
        _: &std::sync::atomic::AtomicBool,
    ) -> anyhow::Result<pp_storage::read_model::views::ReviewObservations> {
        use pp_storage::read_model::{
            Provenance,
            views::{MediaObservation, ReviewObservations},
        };
        let available_input_roots = match &snapshot.provenance {
            Provenance::Tracked { inputs, .. } => {
                inputs.iter().map(|input| input.input_id).collect()
            }
            Provenance::Legacy => Default::default(),
        };
        let media_by_part_id = snapshot
            .parts
            .iter()
            .filter(|part| part.included)
            .map(|part| {
                (
                    part.projection_part_id,
                    MediaObservation {
                        artifact_missing: false,
                        thumb_empty: true,
                    },
                )
            })
            .collect();
        Ok(ReviewObservations {
            available_input_roots,
            media_by_part_id,
        })
    }
}

async fn guard(
    axum::extract::State(Guard {
        config,
        auth,
        admission,
        budget,
        failures,
        sessions,
    }): axum::extract::State<Guard>,
    mut request: axum::extract::Request,
    next: Next,
) -> Response {
    if let Err((status, detail)) = admit(
        &config.origin,
        &config.host,
        &config.api_key_tenant,
        &request,
    ) {
        return failure(status, detail);
    }
    let peer = match crate::NetworkAdmission::peer(&request) {
        Ok(peer) => peer.ip(),
        Err((status, detail)) => return failure(status, detail),
    };
    // A peer whose bucket is spent costs no key check.
    if let Some(refused) = failures.check(peer).rejection() {
        return refused;
    }
    let credential = principal(request.headers(), &config.api_key_tenant);
    match resolve(credential, config.wait, auth, admission).await {
        Ok((key, presented)) => {
            let quota = budget.consume();
            if let Some(refused) = quota.rejection() {
                return refused;
            }
            let named = session_keys::named(request.headers());
            let deleting = request.method() == axum::http::Method::DELETE;
            let key_id = key.key_id.clone();
            let mut response = match &named {
                Some(id) if !sessions.opened_by(id, &key_id) => session_keys::not_found(),
                _ => {
                    // rmcp copies the request parts into each handler's context.
                    request.extensions_mut().insert(key);
                    request.extensions_mut().insert(presented);
                    next.run(request).await
                }
            };
            match (named, session_keys::named(response.headers())) {
                (None, Some(opened)) => sessions.bind(opened, key_id).await,
                (Some(id), _) if deleting && response.status().is_success() => sessions.forget(&id),
                _ => {}
            }
            quota.apply(&mut response);
            response
        }
        Err((status, detail)) => {
            let quota = failures.consume(peer);
            if let Some(limited) = quota.as_ref().and_then(rate_limit::Quota::rejection) {
                return limited;
            }
            let mut refused = failure(status, detail);
            if let Some(quota) = quota {
                quota.apply(&mut refused);
            }
            refused
        }
    }
}

/// Resolves the request's API key for the routed tenant, or the refusal's
/// status and detail.
async fn resolve(
    credential: Option<PresentedCredential>,
    wait: Duration,
    auth: RebuildAuthClient,
    admission: Arc<Semaphore>,
) -> Result<(KeyPrincipal, PresentedCredential), (StatusCode, &'static str)> {
    let Some(PresentedCredential::ApiKey { tenant, secret }) = credential else {
        return Err((StatusCode::UNAUTHORIZED, KEY_REQUIRED));
    };
    let presented = PresentedCredential::ApiKey {
        tenant: tenant.clone(),
        secret: secret.clone(),
    };
    let unavailable = (
        StatusCode::SERVICE_UNAVAILABLE,
        "API key check temporarily unavailable",
    );
    let Ok(permit) = admission.try_acquire_owned() else {
        return Err(unavailable);
    };
    let resolved = tokio::task::spawn_blocking(move || {
        let outcome = auth
            .submit(
                auth::Request::ResolveKey {
                    tenant_id: tenant.clone(),
                    key: Secret::new(secret),
                },
                Arc::new(AtomicBool::new(false)),
                wait,
            )
            .ok()?
            .recv()
            .ok()?
            .ok()?;
        match outcome {
            auth::Outcome::KeyResolved { principal, .. } => {
                Some(principal.filter(|key| key.tenant_id == tenant))
            }
            _ => None,
        }
    })
    .await;
    drop(permit);
    match resolved {
        Ok(Some(Some(key))) => Ok((key, presented)),
        Ok(Some(None)) => Err((StatusCode::UNAUTHORIZED, KEY_REQUIRED)),
        _ => Err(unavailable),
    }
}

fn failure(status: StatusCode, detail: &str) -> Response {
    (status, Json(json!({ "detail": detail }))).into_response()
}

/// Session handler. Each tool call uses its own credential; proposals belong
/// to the installation's queue, not to this handler.
#[derive(Clone)]
struct ProductMcp {
    reads: Reads,
}

impl ProductMcp {
    fn new(reads: Reads) -> Self {
        Self { reads }
    }

    async fn in_tools<T: Send + 'static>(
        &self,
        presented: Option<PresentedCredential>,
        work: impl FnOnce(&McpTools<CheckoffDomain>) -> T + Send + 'static,
    ) -> Result<T, ErrorData> {
        let Reads {
            progress,
            builds,
            drafts,
            farm,
            wait,
            ..
        } = self.reads.clone();
        tokio::task::spawn_blocking(move || {
            let domain = CheckoffDomain::new(progress, builds, drafts, farm, wait, presented);
            work(&McpTools::new(domain, None))
        })
        .await
        .map_err(internal)
    }

    /// `get_build_workflow`: the Build's workflow, read with the caller's key
    /// in one storage transaction, or `{"error": ...}` as Node answers.
    async fn build_workflow(
        &self,
        arguments: &Value,
        presented: PresentedCredential,
    ) -> ToolResult {
        let error = |message: &str| ToolResult {
            text: json!({ "error": message }).to_string(),
            is_error: true,
        };
        let Some(id) = positive_id(arguments, "plan_id") else {
            return error("plan_id required");
        };
        match self.workflow(id, presented).await {
            Some(Outcome::Workspace(workspace)) => match serde_json::to_string(&workspace) {
                Ok(text) => ToolResult {
                    text,
                    is_error: false,
                },
                Err(_) => error("Build workflow is unavailable"),
            },
            Some(Outcome::MissingBuild) => error("Build not found"),
            _ => error("Build workflow is unavailable"),
        }
    }

    /// The Build's workflow read with the caller's key, or `None` when the
    /// read failed.
    async fn workflow(&self, id: PositiveId, presented: PresentedCredential) -> Option<Outcome> {
        let Reads { workflow, wait, .. } = self.reads.clone();
        let read = tokio::task::spawn_blocking(move || {
            workflow.read(
                presented.into_credential(),
                Request::Build(Some(id)),
                Arc::new(AtomicBool::new(false)),
                wait,
            )
        })
        .await;
        read.ok()?.ok()
    }
}

/// `arguments[field]` as a whole number from 1 to 2^53 - 1, as Node reads it.
fn positive_id(arguments: &Value, field: &str) -> Option<PositiveId> {
    arguments
        .get(field)
        .and_then(|id| {
            id.as_u64().or_else(|| {
                id.as_f64()
                    .filter(|id| id.fract() == 0.0 && *id >= 1.0 && *id <= 9_007_199_254_740_991.0)
                    .map(|id| id as u64)
            })
        })
        .and_then(|id| PositiveId::new(id).ok())
}

fn internal(message: impl ToString) -> ErrorData {
    ErrorData::internal_error(message.to_string(), None)
}

impl ServerHandler for ProductMcp {
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::new(ServerCapabilities::builder().enable_tools().build());
        if let Ok(server) =
            serde_json::from_value::<Implementation>(server_info()["serverInfo"].clone())
        {
            info = info.with_server_info(server);
        }
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        // The farm tools are listed only while the caller's Farm Module is on.
        let presented = context
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<PresentedCredential>())
            .cloned();
        let mut tools = self
            .in_tools(presented, |tools| tools.list_tools())
            .await?
            .map_err(|error| internal(error.0))?;
        plan_history::describe_inputs(&mut tools);
        let mut tools: ListToolsResult = serde_json::from_value(tools).map_err(internal)?;
        if let Some(list) = tools
            .tools
            .iter_mut()
            .find(|tool| tool.name == "list_pending_actions")
        {
            list.description = Some("List the installation's open proposals, including proposals from other authenticated sessions.".into());
        }
        Ok(tools)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        // The guard attaches the resolved key; tool calls never run without it.
        // GRE-441 scopes ported tools to this principal's tenant.
        let parts = context.extensions.get::<axum::http::request::Parts>();
        let _caller = parts
            .and_then(|parts| parts.extensions.get::<KeyPrincipal>())
            .ok_or_else(|| internal("MCP request has no resolved API key"))?;
        let arguments = request.arguments.map(Value::Object).unwrap_or(Value::Null);
        let presented = parts
            .and_then(|parts| parts.extensions.get::<PresentedCredential>())
            .cloned()
            .ok_or_else(|| internal("MCP request has no credential"))?;
        let peer = context.peer.peer_info();
        let staged_by = pp_storage::pending_actions::StagedBy::label(
            peer.as_ref()
                .map(|info| info.client_info.name.as_str())
                .unwrap_or_default(),
        );
        let result = match request.name.as_ref() {
            "get_build_workflow" => self.build_workflow(&arguments, presented).await,
            name if planning_reads::TOOLS.contains(&name) => {
                self.planning_read(&request.name, &arguments, presented)
                    .await
            }
            name if plan_history::TOOLS.contains(&name) => {
                self.plan_history_tool(&request.name, &arguments, presented)
                    .await
            }
            name if reads::TOOLS.contains(&name) => {
                self.read_tool(&request.name, &arguments, presented).await
            }
            name if progress_reads::TOOLS.contains(&name) => {
                self.progress_tool(&request.name, &arguments, presented)
                    .await
            }
            plan_accept::PROPOSE => self.propose_accept(&arguments, presented, staged_by).await,
            "confirm_apply" => {
                #[cfg(test)]
                if let Some(gate) = &self.reads.confirm_gate {
                    gate.watch(context.ct.clone().cancelled_owned());
                }
                self.confirm_proposal(arguments, presented).await
            }
            "list_pending_actions" => self.list_proposals(presented).await,
            "dismiss_proposed_action" => self.dismiss_proposal(arguments, presented).await,
            _ => {
                self.product_tool(request.name.to_string(), arguments, presented, staged_by)
                    .await?
            }
        };
        let result: CallToolResult = serde_json::from_value(result.to_json()).map_err(internal)?;
        Ok(result.into())
    }
}

#[path = "mcp/checkoff.rs"]
mod checkoff;
#[path = "mcp/create_build.rs"]
mod create_build;
#[path = "mcp/draft_parts.rs"]
mod draft_parts;
#[path = "mcp/farm_status.rs"]
mod farm_status;
#[path = "mcp/import_inputs.rs"]
mod import_inputs;
#[path = "mcp/pending_actions.rs"]
mod pending_actions;
#[path = "mcp/plan_accept.rs"]
pub(crate) mod plan_accept;
#[path = "mcp/plan_history.rs"]
mod plan_history;
#[path = "mcp/planning.rs"]
mod planning;
#[path = "mcp/planning_reads.rs"]
mod planning_reads;
#[path = "mcp/print_stats.rs"]
mod print_stats;
#[path = "mcp/progress_reads.rs"]
mod progress_reads;
#[path = "mcp/rate_limit.rs"]
mod rate_limit;
#[path = "mcp/reads.rs"]
mod reads;
#[path = "mcp/rebuild_plan.rs"]
mod rebuild_plan;
#[path = "mcp/role_filament.rs"]
mod role_filament;
#[path = "mcp/session_keys.rs"]
mod session_keys;
#[path = "mcp/source_roles.rs"]
mod source_roles;
#[path = "mcp/write_result.rs"]
mod write_result;

#[cfg(test)]
#[path = "mcp/tests.rs"]
mod tests;
