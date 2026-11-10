//! stdio ⇄ Streamable HTTP MCP proxy.
//!
//! Serves MCP on stdin/stdout and forwards `tools/list` and `tools/call` to
//! the app's `/mcp` endpoint with `Authorization: Bearer $X3_TOKEN`.

use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, ClientConfig, Implementation, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerConfig,
    },
    service::{RequestContext, RunningService},
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, StreamableHttpClientTransport,
    },
    ErrorData as McpError, RoleClient, RoleServer, ServerHandler, ServiceExt,
};

pub struct Shim {
    upstream: RunningService<RoleClient, ClientConfig>,
    instructions: Option<String>,
}

fn upstream_error(e: impl std::fmt::Display) -> McpError {
    McpError::internal_error(format!("Voice Studio app: {e}"), None)
}

impl ServerHandler for Shim {
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "voice-studio",
                env!("CARGO_PKG_VERSION"),
            ));
        if let Some(i) = &self.instructions {
            info = info.with_instructions(i.clone());
        }
        info
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        self.upstream
            .list_tools(request)
            .await
            .map_err(upstream_error)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        self.upstream
            .call_tool_once(request)
            .await
            .map_err(upstream_error)
    }
}

pub async fn run(url: &str, token: &str) -> anyhow::Result<()> {
    if !token.starts_with("x3_live_") {
        anyhow::bail!("X3_TOKEN must be a Voice Studio token (x3_live_…)");
    }
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(url.to_string()).auth_header(token),
    );
    let upstream = ClientConfig::default()
        .serve(transport)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "could not connect to Voice Studio at {url} ({e}). \
             Is the app running with the server enabled, and is X3_TOKEN valid?"
            )
        })?;
    let instructions = upstream.peer_info().and_then(|i| i.instructions.clone());
    tracing::info!("connected to {url}");

    let shim = Shim {
        upstream,
        instructions,
    };
    let server = shim.serve(rmcp::transport::stdio()).await?;
    server.waiting().await?;
    Ok(())
}
