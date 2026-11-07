//! Streamable HTTP MCP on the rebuild loopback listener (T-16, GRE-348).
//!
//! `/mcp` and `/api/v1/mcp` serve the pp-services product catalog through
//! rmcp. Every request needs a valid API key for the routed tenant: a session
//! cookie alone is refused, and so is a key the auth worker does not resolve
//! to that tenant (unknown, revoked or expired). Host and Origin must match
//! the listener exactly. Write tools only propose; `confirm_apply` applies.
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
use pp_services::mcp_domain::{McpSession, ToolResult, UnportedDomain, server_info};
use pp_storage::build_graph::PositiveId;
use pp_storage::{
    auth::{self, KeyPrincipal, Secret, rebuild::RebuildAuthClient},
    read_model::{
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
        })
    }
}

#[derive(Clone)]
struct Guard {
    config: McpHttpConfig,
    auth: RebuildAuthClient,
    admission: Arc<Semaphore>,
}

/// The clients ported tools use: `get_build_workflow` reads the same client as
/// `GET /plans/{id}/workflow`, and Plan Accept uses the draft routes' client
/// and checks the Build planning brief with the planning routes' client.
#[derive(Clone)]
struct Reads {
    workflow: WorkflowClient,
    drafts: WorkingDraftClient,
    planning: PlanningClient,
    wait: Duration,
}

/// Mounts the MCP transport at `/mcp` and `/api/v1/mcp` when the rebuild flag
/// was on at startup. Otherwise nothing is mounted. `get_build_workflow` reads
/// `workflow` and Plan Accept uses `drafts` and `planning`, each with the
/// request's API key.
pub fn mcp_router(
    config: McpHttpConfig,
    auth: RebuildAuthClient,
    workflow: WorkflowClient,
    drafts: WorkingDraftClient,
    planning: PlanningClient,
) -> Router {
    if !config.enabled {
        return Router::new();
    }
    // `admit` in the guard is the authoritative Host and Origin check. rmcp's
    // own allowlists repeat the same exact listener values.
    let transport = StreamableHttpServerConfig::default()
        .with_allowed_hosts([config.host.clone()])
        .with_allowed_origins([config.origin.clone()]);
    let reads = Reads {
        workflow,
        drafts,
        planning,
        wait: config.wait,
    };
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
                config,
                auth,
                admission: Arc::new(Semaphore::new(ADMISSION_PERMITS)),
            },
            guard,
        ))
}

async fn guard(
    axum::extract::State(Guard {
        config,
        auth,
        admission,
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
struct ProductMcp {
    session: McpSession<UnportedDomain>,
    reads: Reads,
}

impl ProductMcp {
    fn new(reads: Reads) -> Self {
        Self {
            session: McpSession::new(UnportedDomain, None),
            reads,
        }
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
        match read {
            Ok(Ok(Outcome::Workspace(workspace))) => match serde_json::to_string(&workspace) {
                Ok(text) => ToolResult {
                    text,
                    is_error: false,
                },
                Err(_) => error("Build workflow is unavailable"),
            },
            Ok(Ok(Outcome::MissingBuild)) => error("Build not found"),
            _ => error("Build workflow is unavailable"),
        }
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
        let presented = parts.and_then(|parts| parts.extensions.get::<PresentedCredential>());
        let confirming = (request.name == "confirm_apply")
            .then(|| arguments.get("action_id").and_then(Value::as_str))
            .flatten()
            .and_then(|id| self.session.pending(id))
            .filter(|action| action.action_type == plan_accept::PROPOSE);
        let result = match presented {
            Some(presented) if request.name == "get_build_workflow" => {
                self.build_workflow(&arguments, presented.clone()).await
            }
            Some(presented) if request.name == plan_accept::PROPOSE => {
                self.propose_accept(&arguments, presented.clone()).await
            }
            Some(presented) if confirming.is_some() => {
                // Taken before Accept so a concurrent confirm cannot apply it twice.
                match confirming.and_then(|action| self.session.take_pending(&action.id)) {
                    Some(action) => self.confirm_accept(action, presented.clone()).await,
                    None => self
                        .session
                        .call_tool(&request.name, &arguments)
                        .map_err(|error| internal(error.0))?,
                }
            }
            _ => self
                .session
                .call_tool(&request.name, &arguments)
                .map_err(|error| internal(error.0))?,
        };
        let result: CallToolResult = serde_json::from_value(result.to_json()).map_err(internal)?;
        Ok(result.into())
    }
}

#[path = "mcp/plan_accept.rs"]
mod plan_accept;

#[cfg(test)]
#[path = "mcp/tests.rs"]
mod tests;
