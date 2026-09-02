// MCP server boundary: exposes glean's read and cursor ops as typed tools over
// stdio so MCP-native agents can drive the changed-file set directly. The rest
// of the binary is synchronous; the async runtime lives only behind `serve`, and
// the blocking git work runs on the runtime's blocking pool.

use anyhow::{Context, Result};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::transport::stdio;
use rmcp::{schemars, tool, tool_handler, tool_router, ErrorData, Json, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Repo;

const DEFAULT_CONSUMER: &str = "default";

#[derive(Deserialize, JsonSchema)]
struct ListRequest {
    #[serde(default)]
    #[schemars(description = "Consumer baseline to read (defaults to \"default\").")]
    consumer: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct MarkRequest {
    #[serde(default)]
    #[schemars(description = "Consumer baseline to advance (defaults to \"default\").")]
    consumer: Option<String>,
    #[serde(default)]
    #[schemars(description = "Paths to mark; omit or leave empty to mark the whole changed set.")]
    paths: Option<Vec<String>>,
}

#[derive(Deserialize, JsonSchema)]
struct StatusRequest {
    #[serde(default)]
    #[schemars(description = "Consumer to report; omit to report every consumer.")]
    consumer: Option<String>,
}

// MCP requires a tool's output schema to have an object root, so the list and
// status results are objects wrapping their arrays rather than bare arrays.
#[derive(Serialize, JsonSchema)]
struct ListResult {
    paths: Vec<String>,
}

#[derive(Serialize, JsonSchema)]
struct MarkResult {
    marked: usize,
}

#[derive(Serialize, JsonSchema)]
struct StatusResult {
    consumers: Vec<StatusEntry>,
}

#[derive(Serialize, JsonSchema)]
struct StatusEntry {
    consumer: String,
    tracked: usize,
    changed: usize,
}

#[derive(Clone)]
pub struct GleanServer {
    tool_router: ToolRouter<Self>,
}

impl GleanServer {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl GleanServer {
    #[tool(description = "List the paths changed since a consumer last marked them.")]
    async fn glean_list(
        &self,
        Parameters(req): Parameters<ListRequest>,
    ) -> Result<Json<ListResult>, ErrorData> {
        let consumer = req.consumer.unwrap_or_else(|| DEFAULT_CONSUMER.to_string());
        let paths = blocking(move || Repo::discover(&consumer)?.changed()).await?;
        Ok(Json(ListResult { paths }))
    }

    #[tool(
        description = "Mark paths as processed for a consumer; returns how many baselines moved."
    )]
    async fn glean_mark(
        &self,
        Parameters(req): Parameters<MarkRequest>,
    ) -> Result<Json<MarkResult>, ErrorData> {
        let consumer = req.consumer.unwrap_or_else(|| DEFAULT_CONSUMER.to_string());
        let paths = req.paths.unwrap_or_default();
        let marked = blocking(move || Repo::discover(&consumer)?.mark_paths(&paths)).await?;
        Ok(Json(MarkResult { marked }))
    }

    #[tool(
        description = "Report tracked and changed counts per consumer (all consumers if none given)."
    )]
    async fn glean_status(
        &self,
        Parameters(req): Parameters<StatusRequest>,
    ) -> Result<Json<StatusResult>, ErrorData> {
        let consumers = blocking(move || {
            let repo = Repo::discover(req.consumer.as_deref().unwrap_or(DEFAULT_CONSUMER))?;
            let names = match &req.consumer {
                Some(name) => vec![name.clone()],
                None => repo.consumers()?,
            };
            Ok(repo
                .status_for(&names)?
                .into_iter()
                .map(|s| StatusEntry {
                    consumer: s.consumer,
                    tracked: s.tracked,
                    changed: s.changed,
                })
                .collect())
        })
        .await?;
        Ok(Json(StatusResult { consumers }))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for GleanServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "Per-repo incremental change tracker. Each consumer keeps its own baseline; \
                 list surfaces files changed since it last marked, mark advances it.",
            );
        info.server_info = Implementation::new("glean", crate::VERSION);
        info
    }
}

pub fn serve() -> Result<i32> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("building the tokio runtime")?;
    runtime.block_on(async {
        let service = GleanServer::new()
            .serve(stdio())
            .await
            // ServerInitializeError carries non-Send transport state, so format it
            // here rather than propagating the type into anyhow's Send + Sync bound.
            .map_err(|e| anyhow::anyhow!("starting mcp server: {e}"))?;
        service.waiting().await.context("running mcp server")?;
        Ok::<_, anyhow::Error>(())
    })?;
    Ok(0)
}

// Runs blocking git work off the async runtime and flattens the join + op errors
// into the MCP error type the tool handlers return.
async fn blocking<T, F>(f: F) -> Result<T, ErrorData>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ErrorData::internal_error(format!("background task failed: {e}"), None))?
        .map_err(|e| ErrorData::internal_error(format!("{e:#}"), None))
}
