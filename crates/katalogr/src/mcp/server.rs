//! The MCP server: an rmcp `ServerHandler` over the tool table (ADR-0023, FEAT-067).
//!
//! `tools/call` runs the synchronous facade on a blocking thread, one call at a time behind a
//! fair gate, with progress heartbeats while it waits and runs.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, Implementation, ListToolsResult,
    PaginatedRequestParams, ProgressNotificationParam, ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::Value;
use tokio::time::{Instant, MissedTickBehavior, interval_at};

use crate::mcp::ServeConfig;
use crate::mcp::tools;

/// What the host is told at `initialize`.
pub const INSTRUCTIONS: &str = "katalogr answers from a prebuilt index of this project. Start with find or search, then explain, show, refs or callers. Results are katalogr schema/v1 JSON; each may list next steps.";

/// The MCP server. Cloneable; all state is shared.
#[derive(Clone)]
pub struct KatalogrMcp {
    cfg: Arc<ServeConfig>,
    /// FIFO gate: at most one facade call at a time (store opens are not re-entrant in-process).
    gate: Arc<tokio::sync::Mutex<()>>,
    /// Set when the session has ended; queued calls that get the gate afterwards do not start.
    closing: Arc<AtomicBool>,
}

impl KatalogrMcp {
    pub fn new(cfg: ServeConfig) -> Self {
        KatalogrMcp {
            cfg: Arc::new(cfg),
            gate: Arc::new(tokio::sync::Mutex::new(())),
            closing: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Mark the server closing, then wait until no facade call is running.
    pub async fn drain(&self) {
        self.closing.store(true, Ordering::SeqCst);
        let _idle = self.gate.lock().await;
    }
}

/// The protocol version the client negotiated, as `has_structured` wants it.
fn structured(context: &RequestContext<RoleServer>) -> bool {
    let version = context.protocol_version();
    tools::has_structured(match &version {
        Some(v) => Some(v.as_str()),
        None => None,
    })
}

impl ServerHandler for KatalogrMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("katalogr", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tools::list(
            self.cfg.output_schemas,
            structured(&context),
        )))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let name = request.name.to_string();
        if !tools::is_tool(&name) {
            return Err(ErrorData::invalid_params(
                format!("unknown tool: {name}"),
                None,
            ));
        }
        let structured = structured(&context);
        let token = context.meta.get_progress_token();
        let args = match request.arguments {
            Some(m) => Value::Object(m),
            None => Value::Null,
        };
        // The unit of work: wait for the gate, then run the tool on a blocking thread. `None`
        // when the call was cancelled, or the session ended, while it was queued.
        let job = {
            let server = self.clone();
            let ct = context.ct.clone();
            let name = name.clone();
            async move {
                let guard = server.gate.clone().lock_owned().await;
                if server.closing.load(Ordering::SeqCst) || ct.is_cancelled() {
                    return None;
                }
                let cfg = server.cfg.clone();
                Some(
                    tokio::task::spawn_blocking(move || {
                        // The gate is released only when the facade call returns.
                        let _gate = guard;
                        tools::call(&cfg, &name, &args)
                    })
                    .await,
                )
            }
        };
        tokio::pin!(job);
        let period: Duration = self.cfg.progress;
        let mut tick = interval_at(Instant::now() + period, period);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut beats = 0u32;
        let outcome = loop {
            tokio::select! {
                biased;
                r = &mut job => break r,
                _ = tick.tick() => {
                    if let Some(t) = &token {
                        beats += 1;
                        let note = ProgressNotificationParam::new(t.clone(), f64::from(beats))
                            .with_message(format!("{name}: running"));
                        // A send error means the session is ending.
                        let _ = context.peer.notify_progress(note).await;
                    }
                }
            }
        };
        let answer = match outcome {
            None => return Err(ErrorData::internal_error("request cancelled", None)),
            Some(Err(join)) => match join.try_into_panic() {
                Ok(payload) => tools::panic_answer(payload),
                Err(join) => tools::panic_answer(Box::new(format!("task failed: {join}"))),
            },
            Some(Ok(Ok(answer))) => answer,
            Some(Ok(Err(tools::UnknownTool))) => {
                return Err(ErrorData::invalid_params(
                    format!("unknown tool: {name}"),
                    None,
                ));
            }
        };
        Ok(tools::to_call_result(&answer, structured).into())
    }
}
