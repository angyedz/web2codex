//! Stdio MCP connector hosted by OpenAI Tunnel. Execution belongs exclusively to
//! the active Codex turn; this process has no filesystem or shell tools of its own.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData, ServerHandler, ServiceExt};
use serde_json::{Value, json};

pub(crate) fn run(_workspace: PathBuf) -> anyhow::Result<()> {
    // Retain the internal command's workspace argument for tunnel profile
    // compatibility. A tool's actual cwd and permissions come from its Codex turn.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to initialize MCP runtime")?;
    runtime.block_on(async {
        let service = WebMcpServer {
            client: reqwest::Client::builder()
                .no_proxy()
                .connect_timeout(Duration::from_secs(2))
                .timeout(Duration::from_secs(95))
                .build()?,
        }
        .serve(rmcp::transport::stdio())
        .await
        .context("failed to start Web2Codex MCP server")?;
        service
            .waiting()
            .await
            .context("Web2Codex MCP server stopped")?;
        Ok(())
    })
}

struct WebMcpServer {
    client: reqwest::Client,
}

impl ServerHandler for WebMcpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("web2codex", env!("CARGO_PKG_VERSION")))
            .with_instructions("Use the turn_token from the current Web2Codex request. Discover the active Codex tools with codex_tool_inventory, then invoke their exact wire_name with codex_tool_call. Codex executes tools with its normal approvals and sandbox. Tool results return to this same ChatGPT response. Use short command/agent polls; never retry an operation after an uncertain timeout. Reply to the user in ordinary prose.")
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        if request.is_some_and(|request| request.cursor.is_some()) {
            return Err(ErrorData::invalid_params(
                "Web2Codex tools do not use pagination cursors",
                None,
            ));
        }
        Ok(ListToolsResult::with_all_items(connector_tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let path = match request.name.as_ref() {
            "codex_tool_inventory" => "/bridge/tools",
            "codex_tool_call" => "/bridge/invoke",
            _ => {
                return Err(ErrorData::invalid_params(
                    "Unknown Web2Codex connector tool",
                    None,
                ));
            }
        };
        let arguments = Value::Object(request.arguments.unwrap_or_default());
        let result = tokio::select! {
            _ = context.ct.cancelled() => Err(anyhow::anyhow!("ChatGPT cancelled the MCP request; no retry was sent")),
            result = self.call_bridge(path, arguments) => result,
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => CallToolResult::error(vec![ContentBlock::text(format!("{error:#}"))]),
        };
        Ok(result.into())
    }
}

impl WebMcpServer {
    async fn call_bridge(&self, path: &str, arguments: Value) -> anyhow::Result<CallToolResult> {
        let (address, token) = crate::web_browser::bridge_credentials()?;
        let response = self
            .client
            .post(format!("http://{address}{path}"))
            .bearer_auth(token)
            .json(&arguments)
            .send()
            .await
            .context("The local Codex bridge could not be reached; the call was not retried")?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .context("The local Codex bridge returned an invalid response")?;
        anyhow::ensure!(
            status.is_success(),
            "{}",
            body.pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("The Codex bridge rejected the connector request")
        );
        if path == "/bridge/tools" {
            Ok(CallToolResult::structured(body))
        } else {
            serde_json::from_value(body).context("Invalid native Codex tool result")
        }
    }
}

fn connector_tools() -> Vec<Tool> {
    let capability = json!({
        "type":"string", "pattern":"^turn_[a-f0-9]{32}$",
        "description":"Copy turn_token exactly from the current Web2Codex request. Do not guess, alter, or reuse it."
    });
    let definitions = [
        (
            "codex_tool_inventory",
            "Discover the exact tools available in the active Codex harness, including terminal sessions, apply_patch, plans, subagents, and configured MCP tools. Schemas are returned through this connector. Use query and pagination to find tools; refresh after native tool search.",
            json!({
                "type":"object", "properties": {
                    "turn_token":capability.clone(), "query":{"type":"string","maxLength":500},
                    "offset":{"type":"integer","minimum":0,"maximum":100000},
                    "limit":{"type":"integer","minimum":1,"maximum":50},
                    "include_schema":{"type":"boolean"}
                }, "required":["turn_token"], "additionalProperties":false
            }),
        ),
        (
            "codex_tool_call",
            "Invoke an exact wire_name returned by codex_tool_inventory. The outer Codex harness performs the native call, approval, sandboxing, and UI events. For function/tools use arguments; for freeform/custom tools use input. Poll command sessions for at most 60000 ms and agents for at most 30000 ms per call. Results, including inline images, return here in the same ChatGPT response.",
            json!({
                "type":"object", "properties": {
                    "turn_token":capability, "wire_name":{"type":"string","minLength":1,"maxLength":1000},
                    "arguments":{"type":"object","additionalProperties":true},
                    "input":{"type":"string","maxLength":524288}
                }, "required":["turn_token","wire_name"], "additionalProperties":false
            }),
        ),
    ];
    definitions
        .into_iter()
        .map(|(name, description, schema)| {
            Tool::new(
                name,
                description,
                Arc::new(
                    schema
                        .as_object()
                        .expect("static MCP schema is an object")
                        .clone(),
                ),
            )
        })
        .collect()
}
