//! Streamable HTTP MCP on the rebuild loopback listener (T-16, GRE-348).
//!
//! `/mcp` and `/api/v1/mcp` serve the pp-services product catalog through
//! rmcp. Every request needs a valid API key for the routed tenant: a session
//! cookie alone is refused, and so is a key the auth worker does not resolve
//! to that tenant (unknown, revoked or expired). Host and Origin must match
//! the listener exactly; then the request counts against MCP's budget of
//! 1000 a minute, Node's limit, before its key is checked
//! (`mcp/rate_limit.rs`). Write tools only propose; `confirm_apply` applies.
//! `propose_verify_printer_checkoff` applies as a Checkoff burst on storage
//! (GRE-1170, `mcp/checkoff.rs`). `list_plans`, `get_plan_decisions` and
//! `list_plan_snapshots` read storage with the caller's key (`mcp/reads.rs`),
//! and so do `get_remaining` and `get_printer_checkoff`
//! (`mcp/progress_reads.rs`).
//! LAN peers stay out until the T-14 trust classes land.

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
use pp_services::mcp_domain::{McpSession, ToolResult, server_info};
use pp_storage::build_graph::{BuildGraphClient, PositiveId};
use pp_storage::checkoff_progress::ProgressClient;
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
    wait: Duration,
    limit: u32,
    window: Duration,
    filament_reader: Option<Arc<dyn crate::filaments::CatalogReader>>,
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
            wait: Duration::from_millis(100),
            limit: rate_limit::LIMIT,
            window: rate_limit::WINDOW,
            filament_reader: None,
            #[cfg(test)]
            confirm_gate: None,
            #[cfg(test)]
            clock: None,
        })
    }

    /// Checked inventory reads for confirmed role-filament assignments.
    pub fn with_filament_reader(
        mut self,
        reader: Arc<dyn crate::filaments::CatalogReader>,
    ) -> Self {
        self.filament_reader = Some(reader);
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
    wait: Duration,
    filament_reader: Option<Arc<dyn crate::filaments::CatalogReader>>,
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
    // own allowlists repeat the same exact listener values.
    let transport = StreamableHttpServerConfig::default()
        .with_allowed_hosts([config.host.clone()])
        .with_allowed_origins([config.origin.clone()]);
    let McpClients {
        builds,
        workflow,
        drafts,
        planning,
        progress,
        accepted,
    } = clients;
    let reads = Reads {
        builds,
        workflow,
        drafts,
        planning,
        progress,
        accepted,
        wait: config.wait,
        filament_reader: config.filament_reader.clone(),
        #[cfg(test)]
        confirm_gate: config.confirm_gate.clone(),
    };
    let budget = rate_limit::Budget::new(config.limit, config.window);
    #[cfg(test)]
    let budget = budget.with_clock(config.clock.clone());
    let service = StreamableHttpService::new(
        move || Ok(ProductMcp::new(reads.clone())),
        Arc::new(LocalSessionManager::default()),
        transport,
    );
    Router::new()
        .route_service("/mcp", service.clone())
        .route_service("/api/v1/mcp", service)
        .layer(middleware::from_fn_with_state(
            Guard {
                budget: Arc::new(budget),
                config,
                auth,
                admission: Arc::new(Semaphore::new(ADMISSION_PERMITS)),
            },
            guard,
        ))
}

/// The router over every client MCP reads, each opened on `owner` with
/// `policy`, and the auth client that issues its keys.
#[cfg(test)]
pub(crate) fn test_router(
    owner: &pp_storage::WriterOwner,
    config: McpHttpConfig,
    policy: auth::AuthPolicy,
) -> (Router, RebuildAuthClient) {
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
        },
    );
    (router, auth)
}

async fn guard(
    axum::extract::State(Guard {
        config,
        auth,
        admission,
        budget,
    }): axum::extract::State<Guard>,
    request: axum::extract::Request,
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
    let quota = budget.consume();
    if let Some(refused) = quota.rejection() {
        return refused;
    }
    let mut response = authenticated(config, auth, admission, request, next).await;
    quota.apply(&mut response);
    response
}

/// Resolves the request's API key for the routed tenant and runs the request
/// with it, or refuses.
async fn authenticated(
    config: McpHttpConfig,
    auth: RebuildAuthClient,
    admission: Arc<Semaphore>,
    mut request: axum::extract::Request,
    next: Next,
) -> Response {
    let Some(PresentedCredential::ApiKey { tenant, secret }) =
        principal(request.headers(), &config.api_key_tenant)
    else {
        return failure(StatusCode::UNAUTHORIZED, KEY_REQUIRED);
    };
    let presented = PresentedCredential::ApiKey {
        tenant: tenant.clone(),
        secret: secret.clone(),
    };
    let Ok(permit) = admission.try_acquire_owned() else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "API key check temporarily unavailable",
        );
    };
    let wait = config.wait;
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
        Ok(Some(Some(key))) => {
            // rmcp copies the request parts into each handler's context.
            request.extensions_mut().insert(key);
            request.extensions_mut().insert(presented);
            next.run(request).await
        }
        Ok(Some(None)) => failure(StatusCode::UNAUTHORIZED, KEY_REQUIRED),
        _ => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "API key check temporarily unavailable",
        ),
    }
}

fn failure(status: StatusCode, detail: &str) -> Response {
    (status, Json(json!({ "detail": detail }))).into_response()
}

/// One MCP session: its own pending proposals, product tools from the catalog.
/// `turn` runs one session tool call at a time, so the caller's key set on
/// the domain belongs to the call that uses it. The blocking task holds it, so
/// a call whose request was cancelled still finishes before the next starts.
struct ProductMcp {
    session: Arc<McpSession<CheckoffDomain>>,
    turn: Arc<tokio::sync::Mutex<()>>,
    reads: Reads,
}

impl ProductMcp {
    fn new(reads: Reads) -> Self {
        Self {
            session: Arc::new(McpSession::new(
                CheckoffDomain::new(reads.progress.clone(), reads.builds.clone(), reads.wait),
                None,
            )),
            turn: Arc::new(tokio::sync::Mutex::new(())),
            reads,
        }
    }

    /// A catalog or session tool, on a blocking thread with the caller's key.
    async fn session_tool(
        &self,
        name: String,
        arguments: Value,
        presented: Option<PresentedCredential>,
    ) -> Result<ToolResult, ErrorData> {
        let turn = self.turn.clone().lock_owned().await;
        let session = self.session.clone();
        let result = tokio::task::spawn_blocking(move || {
            let _turn = turn;
            session.port().set_caller(presented);
            let result = session.call_tool(&name, &arguments);
            session.port().set_caller(None);
            result
        })
        .await;
        result.map_err(internal)?.map_err(|error| internal(error.0))
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
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools = self
            .session
            .list_tools()
            .map_err(|error| internal(error.0))?;
        serde_json::from_value(tools).map_err(internal)
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
            .cloned();
        let confirming = (request.name == "confirm_apply")
            .then(|| arguments.get("action_id").and_then(Value::as_str))
            .flatten()
            .and_then(|id| self.session.pending(id))
            .filter(|action| {
                action.action_type == plan_accept::PROPOSE
                    || draft_planning::TOOLS.contains(&action.action_type.as_str())
            });
        let result = match presented {
            Some(presented) if request.name == "get_build_workflow" => {
                self.build_workflow(&arguments, presented).await
            }
            Some(presented) if reads::TOOLS.contains(&request.name.as_ref()) => {
                self.read_tool(&request.name, &arguments, presented).await
            }
            Some(presented) if progress_reads::TOOLS.contains(&request.name.as_ref()) => {
                self.progress_tool(&request.name, &arguments, presented)
                    .await
            }
            Some(presented) if draft_planning::TOOLS.contains(&request.name.as_ref()) => {
                self.propose_draft_planning(&request.name, &arguments, presented)
                    .await
            }
            Some(presented) if request.name == plan_accept::PROPOSE => {
                self.propose_accept(&arguments, presented).await
            }
            Some(presented) if confirming.is_some() => {
                #[cfg(test)]
                if let Some(gate) = &self.reads.confirm_gate {
                    gate.watch(context.ct.clone().cancelled_owned());
                }
                // Taken before Accept so a concurrent confirm cannot apply it twice.
                match confirming.and_then(|action| self.session.take_pending(&action.id)) {
                    Some(action) if action.action_type == plan_accept::PROPOSE => {
                        self.confirm_accept(action, presented).await
                    }
                    Some(action) => self.confirm_draft_planning(action, presented).await,
                    None => {
                        self.session_tool(request.name.to_string(), arguments, Some(presented))
                            .await?
                    }
                }
            }
            presented => {
                self.session_tool(request.name.to_string(), arguments, presented)
                    .await?
            }
        };
        let result: CallToolResult = serde_json::from_value(result.to_json()).map_err(internal)?;
        Ok(result.into())
    }
}

#[path = "mcp/checkoff.rs"]
mod checkoff;
#[path = "mcp/draft_parts.rs"]
mod draft_parts;
#[path = "mcp/draft_planning.rs"]
mod draft_planning;
#[path = "mcp/plan_accept.rs"]
pub(crate) mod plan_accept;
#[path = "mcp/planning.rs"]
mod planning;
#[path = "mcp/progress_reads.rs"]
mod progress_reads;
#[path = "mcp/rate_limit.rs"]
mod rate_limit;
#[path = "mcp/reads.rs"]
mod reads;

#[cfg(test)]
#[path = "mcp/tests.rs"]
mod tests;
