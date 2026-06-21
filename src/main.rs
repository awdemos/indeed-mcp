mod indeed;
mod oauth;

use anyhow::Result;
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{
        router::tool::ToolRouter,
        wrapper::Parameters,
    },
    model::{ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;

// ── Tool argument structs ──────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchJobsArgs {
    #[schemars(description = "Job title, keywords, or skill to search for")]
    pub keyword: String,
    #[schemars(description = "City, state, or region to search in")]
    pub location: Option<String>,
    #[schemars(description = "Search radius in miles from the location (default: 25)")]
    pub radius: Option<u32>,
    #[schemars(description = "Max results (default: 10, max: 50)")]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct JobDetailArgs {
    #[schemars(description = "Indeed job ID (the `key` from search results)")]
    pub job_id: String,
}

// ── MCP Server ─────────────────────────────────────────────────────────

pub struct IndeedMcpServer {
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
    indeed: indeed::IndeedClient,
    token_mgr: Arc<Mutex<oauth::TokenManager>>,
}

#[tool_router]
impl IndeedMcpServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
            indeed: indeed::IndeedClient::new(),
            token_mgr: Arc::new(Mutex::new(oauth::TokenManager::new())),
        }
    }

    /// Block on an async operation using the current tokio runtime.
    fn block_on<F, T>(&self, f: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        let handle = tokio::runtime::Handle::current();
        tokio::task::block_in_place(|| handle.block_on(f))
    }

    /// Get a valid OAuth token (blocking).
    fn ensure_token(&self) -> Result<String> {
        self.block_on(async {
            let mut mgr = self.token_mgr.lock().await;
            mgr.ensure_authenticated().await?;
            let token = mgr.access_token()?.to_string();
            Ok::<_, anyhow::Error>(token)
        })
    }

    #[tool(description = "Search for jobs on Indeed by keyword and location.")]
    fn jobs_search(
        &self,
        Parameters(args): Parameters<SearchJobsArgs>,
    ) -> String {
        let token = match self.ensure_token() {
            Ok(t) => t,
            Err(e) => return format!("Authentication failed: {}", e),
        };

        match self.block_on(self.indeed.search_jobs(
            &token,
            &args.keyword,
            args.location.as_deref(),
            args.radius,
            args.limit,
        )) {
            Ok(result) => {
                serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Result error: {}", e))
            }
            Err(e) => format!("Job search failed: {}", e),
        }
    }

    #[tool(description = "Get detailed information about a specific job posting on Indeed.")]
    fn job_detail(
        &self,
        Parameters(args): Parameters<JobDetailArgs>,
    ) -> String {
        let token = match self.ensure_token() {
            Ok(t) => t,
            Err(e) => return format!("Authentication failed: {}", e),
        };

        match self.block_on(self.indeed.get_job_detail(&token, &args.job_id)) {
            Ok(result) => {
                serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Result error: {}", e))
            }
            Err(e) => format!("Job detail lookup failed: {}", e),
        }
    }
}

#[tool_handler]
impl ServerHandler for IndeedMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_instructions(
            "Indeed Job Search MCP Server - Search for jobs, get detailed job postings. \
             Use jobs_search to find jobs by keyword and location, \
             and job_detail to get comprehensive information about a specific listing.",
        )
    }
}

// ── Entry Point ─────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_env("INDEED_MCP_LOG")
                .add_directive("indeed_mcp=info".parse()?),
        )
        .init();

    info!("Starting Indeed MCP Server...");

    let server = IndeedMcpServer::new();
    let server = server.serve(stdio()).await?;
    server.waiting().await?;

    Ok(())
}
