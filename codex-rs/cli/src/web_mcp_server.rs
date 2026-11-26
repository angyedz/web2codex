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
    std::thread::scope(|scope| {
        scope
            .spawn(run_server)
            .join()
            .map_err(|_| anyhow::anyhow!("MCP server thread panicked"))?
    })
}

fn run_server() -> anyhow::Result<()> {
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
                .timeout(Duration::from_secs(60))
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
            .with_instructions("Use the turn_token from the current Web2Codex request. Discover tools with codex_tool_inventory, then submit their exact wire_name through codex_tool_call. Web2Codex executes tools with its normal approvals and sandbox. Only when a skill is relevant, discover skills.list and skills.read through MCP, list as needed, then read only the selected skill. Do not add the whole skill catalog to every request. Preserve project instructions supplied through AGENTS.md. External MCP tools and Web2Codex MCP management commands must also be discovered and invoked through this connector. If a tool result contains an MCP resource link, use the native read_mcp_resource tool from the inventory with the exact server and URI returned by that call; do not treat the link itself as resource contents or guess a URI. If a call is pending, use codex_tool_result with its call_id until the result arrives. After a lost connection, check pending_calls in the inventory before taking another action. Never repeat an operation with an uncertain outcome. Reply to the user in ordinary prose.")
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
            "codex_tool_result" => "/bridge/tool-result",
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
            .context("The local Web2Codex bridge could not be reached; the call was not retried")?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .context("The local Web2Codex bridge returned an invalid response")?;
        anyhow::ensure!(
            status.is_success(),
            "{}",
            body.pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("The Web2Codex bridge rejected the connector request")
        );
        if path == "/bridge/tools" {
            Ok(CallToolResult::structured(body))
        } else {
            serde_json::from_value(body).context("Invalid native Web2Codex tool result")
        }
    }
}

fn decode_native_tool_result(body: Value) -> anyhow::Result<CallToolResult> {
    serde_json::from_value(body).context("Invalid native Codex tool result")
}

fn connector_tools() -> Vec<Tool> {
    let capability = json!({
        "type":"string", "pattern":"^turn_[a-f0-9]{32}$",
        "description":"Copy turn_token exactly from the current Web2Codex request. Do not guess, alter, or reuse it."
    });
    let definitions = [
        (
            "codex_tool_inventory",
            "Discover tools available in the active harness, including configured external MCP tools and native MCP resource helpers when MCP servers are configured. Schemas are returned through this connector. Use query and pagination. If an expected tool is absent, invoke an available discovery_tools entry using codex_tool_call, then query the inventory again for the discovered tools.",
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
            "Invoke an exact wire_name returned by codex_tool_inventory. The outer Web2Codex harness performs the native call, approval, sandboxing, and UI events. For function/tools use arguments; for freeform/custom tools use input. Poll command sessions for at most 60000 ms and agents for at most 30000 ms per call. Results, including inline images, return here in the same ChatGPT response.",
            json!({
                "type":"object", "properties": {
                    "turn_token":capability.clone(), "wire_name":{"type":"string","minLength":1,"maxLength":1000},
                    "arguments":{"type":"object","additionalProperties":true},
                    "input":{"type":"string","maxLength":524288}
                }, "required":["turn_token","wire_name"], "additionalProperties":false
            }),
        ),
        (
            "codex_tool_result",
            "Collect a pending native call in this turn. Poll the same call_id while pending. Polling never executes the tool again.",
            json!({
                "type":"object", "properties":{
                    "turn_token":capability, "call_id":{"type":"string","pattern":"^call_[a-f0-9]{32}$"}
                }, "required":["turn_token","call_id"], "additionalProperties":false
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_tool_results_keep_typed_resources_and_structured_content() {
        let resource = json!({
            "type":"resource",
            "resource":{"uri":"file:///workspace/notes.md","mimeType":"text/markdown","text":"RESOURCE_BODY"}
        });
        let resource_link = json!({
            "type":"resource_link",
            "name":"design notes",
            "uri":"web2codex://workspace/design-notes",
            "description":"RESOURCE_LINK_DESCRIPTION",
            "mimeType":"text/markdown"
        });
        let decoded = decode_native_tool_result(json!({
            "content":[resource, resource_link],
            "structuredContent":{"marker":"STRUCTURED_BODY"},
            "isError":false,
            "_meta":{"trace":"TRACE_ID"}
        }))
        .unwrap();
        let encoded = serde_json::to_value(decoded).unwrap();

        assert_eq!(encoded["content"][0]["type"], "resource");
        assert_eq!(encoded["content"][0]["resource"]["text"], "RESOURCE_BODY");
        assert_eq!(encoded["content"][1]["type"], "resource_link");
        assert_eq!(
            encoded["content"][1]["uri"],
            "web2codex://workspace/design-notes"
        );
        assert_eq!(encoded["structuredContent"]["marker"], "STRUCTURED_BODY");
        assert_eq!(encoded["_meta"]["trace"], "TRACE_ID");
    }

    #[test]
    fn native_tool_result_rejects_unknown_content_types_without_dropping_them() {
        let error = decode_native_tool_result(json!({
            "content":[{"type":"unknown_extension","payload":"KEEP_ME"}]
        }))
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Invalid native Codex tool result")
        );
    }
}
