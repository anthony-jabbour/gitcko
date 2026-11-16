// SPDX-License-Identifier: AGPL-3.0-or-later

//! Local MCP server (MCP_SPEC §1–2, Server A).
//!
//! Stdio only. There is no HTTP listener and this crate does not name an HTTP client
//! of its own. The protocol framing is the official SDK
//! (<https://docs.rs/rmcp/3.10.0/rmcp/transport/io/fn.stdio.html>).
//!
//! stdout carries protocol messages only. Anything a person should read goes to stderr.

mod args;
mod clock;
mod paths;
mod present;
mod run;
mod schema;
mod scope;

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gitcko_protocol::event::RunEvent;
use rmcp::ServiceExt;
use rmcp::model::{
    CallToolRequestMethod, CallToolRequestParams, CallToolResponse, Implementation,
    ListToolsResult, PaginatedRequestParams, ProgressNotificationParam, ServerCapabilities,
    ServerConfig,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler};
use serde_json::Value;

pub use args::{ArgError, TOOLS, check_mcp_args, validate};
pub use paths::root_id;
pub use run::{Engine, ProviderMap, ReviewJob, Script};

use paths::startup_root;
use present::tool_error;
use run::Fail;

/// Speak MCP on the process stdin and stdout until the client disconnects.
///
/// `cwd` is the authorized repository root (MCP_SPEC §2). A client that closes the
/// pipe is a normal exit.
pub fn serve_stdio(cwd: &Path, env: &BTreeMap<String, String>) -> u8 {
    let server = GitckoServer::live(cwd.to_path_buf(), env.clone());
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
    root: PathBuf,
    env: BTreeMap<String, String>,
    engine: Engine,
    stderr: Stderr,
}

enum Stderr {
    Process,
    Buffer(Arc<Mutex<Vec<u8>>>),
}

impl GitckoServer {
    /// The process server: real model adapters from the user config.
    pub fn live(cwd: PathBuf, env: BTreeMap<String, String>) -> GitckoServer {
        GitckoServer::build(cwd, env, Engine::Live(None), Stderr::Process)
    }

    /// Like [`live`], but each binding name maps to a test provider.
    pub fn with_providers(
        cwd: PathBuf,
        env: BTreeMap<String, String>,
        providers: ProviderMap,
        stderr: Arc<Mutex<Vec<u8>>>,
    ) -> GitckoServer {
        GitckoServer::build(
            cwd,
            env,
            Engine::Live(Some(providers)),
            Stderr::Buffer(stderr),
        )
    }

    /// Skip the engine and return the scripted outcome. Setup checks still run.
    pub fn with_script(
        cwd: PathBuf,
        env: BTreeMap<String, String>,
        script: Script,
        stderr: Arc<Mutex<Vec<u8>>>,
    ) -> GitckoServer {
        GitckoServer::build(cwd, env, Engine::Script(script), Stderr::Buffer(stderr))
    }

    fn build(
        cwd: PathBuf,
        env: BTreeMap<String, String>,
        engine: Engine,
        stderr: Stderr,
    ) -> GitckoServer {
        GitckoServer {
            root: startup_root(&cwd),
            env,
            engine,
            stderr,
        }
    }
}

/// Parse tool arguments without a repository root (path symlink checks skipped).
pub fn parse_call(name: &str, value: &Value) -> Result<args::ToolArgs, ArgError> {
    let map = value.as_object().ok_or(ArgError {
        code: "INVALID_REQUEST",
        message: "the arguments do not match the tool schema",
        retryable: false,
    })?;
    validate(name, map, None)
}

/// A server rooted at the process directory, for tests that only check the handshake.
pub fn test_server() -> GitckoServer {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    GitckoServer::live(cwd, BTreeMap::new())
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

    fn get_tool(&self, name: &str) -> Option<rmcp::model::Tool> {
        schema::tool_list()
            .into_iter()
            .find(|tool| tool.name == name)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(schema::tool_list()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        if !TOOLS.contains(&request.name.as_ref()) {
            return Err(McpError::method_not_found::<CallToolRequestMethod>());
        }
        let arguments = request.arguments.unwrap_or_default();
        let parsed = match validate(&request.name, &arguments, Some(&self.root)) {
            Ok(parsed) => parsed,
            Err(err) => {
                return Ok(tool_error(err.code, err.message, err.retryable).into());
            }
        };
        // Events leave the blocking review on a channel. Notifications are sent on
        // this runtime, which stays free while the review is in `spawn_blocking`.
        // A missing progress token sends nothing (MCP progress spec). After
        // `notifications/cancelled` the shared token is dropped and no further
        // stage is reported.
        let cancel = context.ct.clone();
        let stop_notes = cancel.clone();
        let progress_token = context.meta.get_progress_token();
        let peer = context.peer.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let forward = tokio::spawn(async move {
            let mut step = 0.0;
            while let Some(event) = rx.recv().await {
                if stop_notes.is_cancelled() {
                    continue;
                }
                let Some(token) = progress_token.as_ref() else {
                    continue;
                };
                let Some(message) = present::progress_note(&event) else {
                    continue;
                };
                step += 1.0;
                let _ = peer
                    .notify_progress(
                        ProgressNotificationParam::new(token.clone(), step).with_message(message),
                    )
                    .await;
            }
        });
        let root = self.root.clone();
        let env = self.env.clone();
        let engine = self.engine.clone();
        let buffer = match &self.stderr {
            Stderr::Process => None,
            Stderr::Buffer(buffer) => Some(Arc::clone(buffer)),
        };
        let result =
            tokio::task::spawn_blocking(move || -> Result<rmcp::model::CallToolResult, ()> {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| ())?;
                runtime.block_on(async move {
                    let mut log = |line: &str| match &buffer {
                        None => eprintln!("{line}"),
                        Some(buffer) => {
                            let mut guard = buffer.lock().unwrap_or_else(|err| err.into_inner());
                            let _ = writeln!(guard, "{line}");
                        }
                    };
                    let on_event: run::Report = Arc::new(move |event: &RunEvent| {
                        let _ = tx.send(event.clone());
                    });
                    Ok(
                        match run::dispatch(
                            &root, &env, &engine, parsed, &cancel, &on_event, &mut log,
                        )
                        .await
                        {
                            Ok(result) => result,
                            Err(Fail::Tool(result)) => result,
                        },
                    )
                })
            })
            .await
            .map_err(|_| McpError::internal_error("tool call failed", None))?
            .map_err(|_| McpError::internal_error("tool call failed", None))?;
        let _ = forward.await;
        Ok(result.into())
    }
}
