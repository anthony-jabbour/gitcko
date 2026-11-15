// SPDX-License-Identifier: AGPL-3.0-or-later

//! Local MCP server (MCP_SPEC §1–2, Server A).
//!
//! Stdio only. There is no HTTP listener and this crate does not depend on an HTTP
//! client. The protocol framing is the official SDK
//! (<https://docs.rs/rmcp/3.10.0/rmcp/transport/io/fn.stdio.html>).
//!
//! stdout carries protocol messages only. Anything a person should read goes to stderr.

use std::path::Path;
use std::time::Duration;

use rmcp::ServiceExt;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler};

/// Speak MCP on the process stdin and stdout until the client disconnects.
///
/// `cwd` is the authorized repository root (MCP_SPEC §2). It is canonicalized when
/// a tool needs it. A client that closes the pipe is a normal exit.
pub fn serve_stdio(cwd: &Path, _env: &std::collections::BTreeMap<String, String>) -> u8 {
    let server = GitckoServer {
        _cwd: cwd.to_path_buf(),
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            eprintln!("could not start the runtime");
            return 1;
        }
    };
    let result = runtime.block_on(async move {
        // https://docs.rs/rmcp/3.10.0/rmcp/transport/io/fn.stdio.html
        let running = server.serve(rmcp::transport::io::stdio()).await?;
        running.waiting().await.map_err(|_| ServeFail::Closed)
    });
    // Tokio's stdin read is not cancelled. A client that already closed the pipe
    // has unblocked it; don't wait out a stuck shutdown.
    // https://docs.rs/tokio/1.54.1/tokio/io/fn.stdin.html
    runtime.shutdown_timeout(Duration::from_millis(50));
    match result {
        Ok(_) | Err(ServeFail::Closed) => 0,
        Err(err) => {
            eprintln!("{}", err.message());
            1
        }
    }
}

/// The local server. Tests build one and drive it over an in-memory pipe.
pub struct GitckoServer {
    _cwd: std::path::PathBuf,
}

/// A server rooted at the process directory, for tests that only check the handshake.
pub fn test_server() -> GitckoServer {
    GitckoServer {
        _cwd: std::env::current_dir().unwrap_or_else(|_| Path::new(".").into()),
    }
}

enum ServeFail {
    Init,
    Closed,
}

impl From<rmcp::service::ServerInitializeError> for ServeFail {
    fn from(err: rmcp::service::ServerInitializeError) -> Self {
        use rmcp::service::ServerInitializeError::{Cancelled, ConnectionClosed};
        match err {
            ConnectionClosed(_) | Cancelled => ServeFail::Closed,
            _ => ServeFail::Init,
        }
    }
}

impl ServeFail {
    fn message(&self) -> &'static str {
        match self {
            ServeFail::Closed => "mcp client disconnected",
            ServeFail::Init => "mcp server stopped",
        }
    }
}

impl ServerHandler for GitckoServer {
    fn get_info(&self) -> ServerConfig {
        // Tools only. No resources, prompts, sampling, or listChanged
        // (MCP_SPEC §2). `enable_tools` leaves `listChanged` unset.
        // https://docs.rs/rmcp/3.10.0/rmcp/model/struct.ServerCapabilities.html
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("gitcko", env!("CARGO_PKG_VERSION")))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::default())
    }

    async fn call_tool(
        &self,
        _request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        Err(McpError::method_not_found::<
            rmcp::model::CallToolRequestMethod,
        >())
    }
}
