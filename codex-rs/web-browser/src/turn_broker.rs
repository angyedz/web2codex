//! Turn-bound bridge between the ChatGPT connector and Codex's native tool loop.
//! This module never executes a tool. Only the outer Codex tool router does that.

use std::collections::{HashMap, VecDeque};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use uuid::Uuid;

use crate::{BridgeCompletion, CallableTool, CallableToolKind, response_item_for_completion};

const MAX_TURNS: usize = 16;
const MAX_PENDING_CALLS: usize = 32;
const MCP_CALL_TIMEOUT: Duration = Duration::from_secs(90);
const COMPLETION_QUIET_PERIOD: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(crate) struct TurnBroker {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    turns: HashMap<String, Turn>,
}

struct Turn {
    request_id: String,
    tools: Vec<CallableTool>,
    queue: VecDeque<String>,
    calls: HashMap<String, Invocation>,
    result: Option<Result<String, String>>,
    text: String,
    text_cursor: usize,
    segment_start: usize,
    revision: u64,
    last_activity: Instant,
    completed_tools: u64,
}

struct Invocation {
    item: Value,
    output: Option<Value>,
    delivered: bool,
}

pub(crate) struct NativeEvent {
    pub(crate) item: Value,
    pub(crate) end_turn: bool,
    pub(crate) text_delta: Option<String>,
    pub(crate) segment_text: String,
}

impl TurnBroker {
    pub(crate) fn begin(
        &self,
        request_id: String,
        tools: Vec<CallableTool>,
    ) -> Result<String, String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A cancelled connector may have no remaining Responses observer.
        // Its WebView is released by the browser's state observer.
        state
            .turns
            .retain(|_, turn| !matches!(turn.result, Some(Err(_))));
        if state.turns.len() >= MAX_TURNS {
            return Err("Too many active ChatGPT turns; finish or cancel an existing turn".into());
        }
        let token = format!("turn_{}", Uuid::new_v4().simple());
        state.turns.insert(
            token.clone(),
            Turn {
                request_id,
                tools,
                queue: VecDeque::new(),
                calls: HashMap::new(),
                result: None,
                text: String::new(),
                text_cursor: 0,
                segment_start: 0,
                revision: 0,
                last_activity: Instant::now(),
                completed_tools: 0,
            },
        );
        Ok(token)
    }

    /// Historical outputs are ignored. An output belonging to a live invocation
    /// resumes that exact browser turn, regardless of other concurrent agents.
    pub(crate) fn resume(
        &self,
        outputs: &[(String, Value)],
        tools: &[CallableTool],
    ) -> Result<Option<(String, String)>, String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut owner: Option<String> = None;
        for (call_id, _) in outputs {
            if let Some((token, _)) = state
                .turns
                .iter()
                .find(|(_, turn)| turn.calls.contains_key(call_id))
            {
                if owner.as_ref().is_some_and(|owner| owner != token) {
                    return Err("Tool outputs belong to different ChatGPT turns".into());
                }
                owner = Some(token.clone());
            }
        }
        let Some(token) = owner else { return Ok(None) };
        let turn = state
            .turns
            .get_mut(&token)
            .ok_or("ChatGPT turn is no longer active")?;
        if turn.result.is_some() {
            return Err("The ChatGPT turn ended before the tool result arrived".into());
        }
        turn.tools = tools.to_vec();
        for (call_id, output) in outputs {
            if let Some(call) = turn.calls.get_mut(call_id) {
                if !call.delivered {
                    return Err("Received a result for a tool that Codex has not received".into());
                }
                if call.output.is_none() {
                    call.output = Some(output.clone());
                    turn.revision += 1;
                    turn.last_activity = Instant::now();
                }
            }
        }
        let request_id = turn.request_id.clone();
        self.changed.notify_all();
        Ok(Some((token, request_id)))
    }

    pub(crate) fn begin_response(&self, token: &str) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let turn = state
            .turns
            .get_mut(token)
            .ok_or("The ChatGPT turn has been cancelled")?;
        turn.segment_start = turn.text_cursor;
        Ok(())
    }

    pub(crate) fn append_text(&self, request_id: &str, delta: String) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(turn) = state
            .turns
            .values_mut()
            .find(|turn| turn.request_id == request_id)
        else {
            return false;
        };
        if turn.result.is_some()
            || delta.is_empty()
            || turn.text.len().saturating_add(delta.len()) > crate::MAX_RESPONSE_TEXT_BYTES
        {
            return false;
        }
        turn.text.push_str(&delta);
        turn.revision += 1;
        turn.last_activity = Instant::now();
        self.changed.notify_all();
        true
    }

    pub(crate) fn inventory(&self, request: &Value) -> Result<Value, String> {
        let token = required_string(request, "turn_token")?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let turn = state
            .turns
            .get_mut(token)
            .ok_or("Unknown or expired turn_token; copy it from the current request")?;
        if turn.result.is_some() {
            return Err("This ChatGPT turn has already ended".into());
        }
        turn.revision += 1;
        turn.last_activity = Instant::now();
        let query = request
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let offset = request
            .get("offset")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(100_000) as usize;
        let limit = request
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .clamp(1, 50) as usize;
        let include_schema = request
            .get("include_schema")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let matching = turn
            .tools
            .iter()
            .filter(|tool| {
                query.is_empty()
                    || format!("{} {}", wire_name(tool), tool.declaration)
                        .to_lowercase()
                        .contains(&query)
            })
            .collect::<Vec<_>>();
        let tools = matching
            .iter()
            .skip(offset)
            .take(limit)
            .map(|tool| {
                let mut declaration = tool.declaration.clone();
                declaration["wire_name"] = json!(wire_name(tool));
                if !include_schema && let Some(object) = declaration.as_object_mut() {
                    object.remove("parameters");
                    object.remove("format");
                }
                declaration
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "tools": tools,
            "total": matching.len(),
            "next_offset": (offset + tools.len() < matching.len()).then_some(offset + tools.len()),
            "discovery_tools": turn.tools.iter().filter(|tool| tool.kind == CallableToolKind::ToolSearch)
                .map(|tool| {
                    let mut declaration = tool.declaration.clone();
                    declaration["wire_name"] = json!(wire_name(tool));
                    declaration
                }).collect::<Vec<_>>()
        }))
    }

    pub(crate) fn invoke(
        &self,
        request: &Value,
        disconnected: impl Fn() -> bool,
    ) -> Result<Value, String> {
        let token = required_string(request, "turn_token")?.to_string();
        let name = required_string(request, "wire_name")?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let turn = state
            .turns
            .get_mut(&token)
            .ok_or("Unknown or expired turn_token; copy it from the current request")?;
        if turn.result.is_some() {
            return Err("This ChatGPT turn has already ended".into());
        }
        if turn.calls.len() >= MAX_PENDING_CALLS {
            return Err("Too many pending tools in this turn".into());
        }
        let tool = turn.tools.iter().find(|tool| wire_name(tool) == name)
            .ok_or_else(|| format!("Tool is not available in this turn: {name}. Discover tools through the MCP inventory."))?;
        let completion = completion_for_call(tool, request)?;
        let call_id = format!("call_{}", Uuid::new_v4().simple());
        let item_id = format!("tool_{}", Uuid::new_v4().simple());
        let (item, _) = response_item_for_completion("", &item_id, &call_id, completion);
        turn.calls.insert(
            call_id.clone(),
            Invocation {
                item,
                output: None,
                delivered: false,
            },
        );
        turn.queue.push_back(call_id.clone());
        turn.revision += 1;
        turn.last_activity = Instant::now();
        self.changed.notify_all();
        let deadline = Instant::now() + MCP_CALL_TIMEOUT;
        loop {
            let turn = state
                .turns
                .get_mut(&token)
                .ok_or("The Codex turn was cancelled or disconnected")?;
            if let Some(result) = turn
                .calls
                .get_mut(&call_id)
                .and_then(|call| call.output.take())
            {
                turn.calls.remove(&call_id);
                turn.revision += 1;
                turn.completed_tools += 1;
                turn.last_activity = Instant::now();
                self.changed.notify_all();
                return Ok(mcp_tool_result(result));
            }
            if let Some(Err(error)) = &turn.result {
                return Err(error.clone());
            }
            let error = if disconnected() {
                Some("The connector cancelled its tool request")
            } else if Instant::now() >= deadline {
                Some(
                    "Native tool did not settle before the MCP deadline; check Codex for a pending approval or running operation. The call was not retried.",
                )
            } else {
                None
            };
            if let Some(error) = error {
                // Retire the complete turn: a late native result must never start
                // another ChatGPT request or cause the operation to run twice.
                turn.result = Some(Err(error.into()));
                turn.last_activity = Instant::now();
                self.changed.notify_all();
                return Err(error.into());
            }
            state = self
                .changed
                .wait_timeout(state, Duration::from_millis(100))
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }

    pub(crate) fn next_event(&self, token: &str) -> Result<Option<NativeEvent>, String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let turn = state
            .turns
            .get_mut(token)
            .ok_or("The ChatGPT turn has been cancelled")?;
        if turn.text_cursor < turn.text.len() {
            let delta = turn.text[turn.text_cursor..].to_string();
            turn.text_cursor = turn.text.len();
            return Ok(Some(NativeEvent {
                item: Value::Null,
                end_turn: false,
                text_delta: Some(delta),
                segment_text: String::new(),
            }));
        }
        if let Some(result) = &turn.result {
            let text = result.clone()?;
            let segment_text = turn
                .text
                .get(turn.segment_start..)
                .unwrap_or(text.as_str())
                .to_string();
            let id = format!("msg_{}", Uuid::new_v4().simple());
            let (item, end_turn) = response_item_for_completion(
                &id,
                "",
                "",
                BridgeCompletion::Message(segment_text.clone()),
            );
            return Ok(Some(NativeEvent {
                item,
                end_turn,
                text_delta: None,
                segment_text,
            }));
        }
        while let Some(call_id) = turn.queue.pop_front() {
            if let Some(call) = turn.calls.get_mut(&call_id) {
                call.delivered = true;
                return Ok(Some(NativeEvent {
                    item: call.item.clone(),
                    end_turn: false,
                    text_delta: None,
                    segment_text: turn
                        .text
                        .get(turn.segment_start..)
                        .unwrap_or_default()
                        .to_string(),
                }));
            }
        }
        Ok(None)
    }

    pub(crate) fn snapshot(&self, request_id: &str) -> Value {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.turns.values().find(|turn| turn.request_id == request_id).map_or(
            json!({"active": false}), |turn| json!({
                    "active": turn.result.is_none(), "pending_tools": turn.calls.len(), "revision": turn.revision,
                    "completed_tools": turn.completed_tools,
                "quiet_ms": turn.last_activity.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
            }))
    }

    /// Fence browser completion against a concurrent connector invocation.
    pub(crate) fn complete(
        &self,
        request_id: &str,
        revision: Option<u64>,
        result: Result<String, String>,
    ) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(turn) = state
            .turns
            .values_mut()
            .find(|turn| turn.request_id == request_id)
        else {
            return false;
        };
        if turn.result.is_some() {
            return false;
        }
        if result.is_ok()
            && (revision != Some(turn.revision)
                || !turn.calls.is_empty()
                || turn.last_activity.elapsed() < COMPLETION_QUIET_PERIOD)
        {
            return false;
        }
        if let Ok(text) = &result {
            if text.len() > crate::MAX_RESPONSE_TEXT_BYTES {
                turn.result = Some(Err("ChatGPT response exceeded the bridge size limit".into()));
                turn.last_activity = Instant::now();
                self.changed.notify_all();
                return true;
            }
            if !text.starts_with(&turn.text) {
                turn.result = Some(Err(
                    "ChatGPT revised text that was already streamed; the response was stopped to avoid returning inconsistent output".into(),
                ));
                turn.last_activity = Instant::now();
                self.changed.notify_all();
                return true;
            }
            turn.text = text.clone();
        }
        turn.result = Some(result);
        turn.last_activity = Instant::now();
        self.changed.notify_all();
        true
    }

    pub(crate) fn retire(&self, token: &str) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.turns.remove(token);
        self.changed.notify_all();
    }

    pub(crate) fn stop(&self, message: &str) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for turn in state.turns.values_mut() {
            turn.result = Some(Err(message.into()));
        }
        self.changed.notify_all();
    }
}

fn wire_name(tool: &CallableTool) -> String {
    format!("{}.{}", tool.namespace, tool.name)
}

fn required_string<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 1000)
        .ok_or_else(|| format!("{key} must be a non-empty string"))
}

fn completion_for_call(tool: &CallableTool, request: &Value) -> Result<BridgeCompletion, String> {
    match tool.kind {
        CallableToolKind::Custom => {
            if request.get("arguments").is_some_and(|args| {
                !args.is_null() && args.as_object().is_none_or(|args| !args.is_empty())
            }) {
                return Err("A freeform tool accepts input, not structured arguments".into());
            }
            let input = request
                .get("input")
                .and_then(Value::as_str)
                .ok_or("A freeform tool requires input")?;
            if input.len() > crate::MAX_TOOL_CALL_INPUT_BYTES {
                return Err("Tool input is too large".into());
            }
            Ok(BridgeCompletion::CustomToolCall {
                namespace: tool.namespace.clone(),
                name: tool.name.clone(),
                input: input.into(),
            })
        }
        CallableToolKind::Function | CallableToolKind::ToolSearch => {
            if request.get("input").is_some_and(|input| !input.is_null()) {
                return Err("This tool requires structured arguments, not freeform input".into());
            }
            let arguments = request
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if !arguments.is_object() {
                return Err("Tool arguments must be an object".into());
            }
            if serde_json::to_vec(&arguments)
                .map_err(|error| error.to_string())?
                .len()
                > crate::MAX_TOOL_CALL_INPUT_BYTES
            {
                return Err("Tool arguments are too large".into());
            }
            if tool.namespace == "functions"
                && ["write_stdin", "exec_command"].contains(&tool.name.as_str())
                && arguments
                    .get("yield_time_ms")
                    .and_then(Value::as_u64)
                    .is_some_and(|value| value > 60_000)
            {
                return Err(
                    "Use yield_time_ms <= 60000 and poll a running command in a separate MCP call"
                        .into(),
                );
            }
            if tool.namespace == "functions"
                && ["wait_agent", "wait"].contains(&tool.name.as_str())
                && arguments
                    .get("timeout_ms")
                    .and_then(Value::as_u64)
                    .is_some_and(|value| value > 30_000)
            {
                return Err("Use timeout_ms <= 30000 and poll agents in separate MCP calls".into());
            }
            if tool.kind == CallableToolKind::ToolSearch {
                Ok(BridgeCompletion::ToolSearchCall { arguments })
            } else {
                Ok(BridgeCompletion::FunctionCall {
                    namespace: tool.namespace.clone(),
                    name: tool.name.clone(),
                    arguments,
                })
            }
        }
    }
}

/// Preserve native MCP structured content and inline media without making network
/// requests for image/audio URLs or silently discarding a tool's result.
fn mcp_tool_result(output: Value) -> Value {
    const FAILED_MARKER: &str = "web2codex://tool-result/failed/v1";
    if output.is_object() && output.get("content").is_some_and(Value::is_array) {
        return output;
    }
    let mut content = Vec::new();
    let mut is_error = false;
    if let Some(parts) = output.as_array() {
        for (index, part) in parts.iter().enumerate() {
            if index == 0
                && part.get("type").and_then(Value::as_str) == Some("input_text")
                && part.get("text").and_then(Value::as_str) == Some(FAILED_MARKER)
            {
                is_error = true;
                continue;
            }
            match part.get("type").and_then(Value::as_str) {
                Some("input_text" | "output_text" | "text") => content.push(json!({"type":"text", "text": part.get("text").and_then(Value::as_str).unwrap_or("")})),
                Some("input_image" | "input_audio") => {
                    let url = part.get("image_url").or_else(|| part.get("audio_url")).and_then(Value::as_str).unwrap_or("");
                    if let Some((mime, data)) = url.strip_prefix("data:").and_then(|data| data.split_once(";base64,")) {
                        content.push(json!({"type": if mime.starts_with("audio/") {"audio"} else {"image"}, "mimeType": mime, "data": data}));
                    } else { content.push(json!({"type":"text", "text": format!("Media result: {part}")})); }
                }
                _ => content.push(json!({"type":"text", "text": part.to_string()})),
            }
        }
    } else {
        content.push(json!({"type":"text", "text": output.as_str().map(str::to_owned).unwrap_or_else(|| output.to_string())}));
    }
    json!({"content": content, "isError": is_error})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    fn registry() -> Vec<CallableTool> {
        crate::parse_callable_tools(Some(&json!([
            {"type":"function", "name":"exec_command", "description":"native terminal schema", "parameters":{"type":"object", "properties":{"cmd":{"type":"string"}}}},
            {"type":"custom", "name":"apply_patch", "description":"native patch grammar", "format":{"type":"text"}},
            {"type":"tool_search", "execution":"client", "description":"native discovery", "parameters":{"type":"object", "properties":{"query":{"type":"string"}}}}
        ]))).expect("valid native registry")
    }

    #[test]
    fn external_mcp_tools_keep_their_own_wait_arguments() {
        let tools = crate::parse_callable_tools(Some(&json!([
            {"type":"namespace", "name":"mcp__external", "tools":[
                {"type":"function", "name":"wait", "parameters":{"type":"object"}},
                {"type":"function", "name":"exec_command", "parameters":{"type":"object"}}
            ]},
            {"type":"function", "name":"wait", "parameters":{"type":"object"}}
        ])))
        .unwrap();
        for tool in &tools[..2] {
            assert!(
                completion_for_call(
                    tool,
                    &json!({"arguments":{
                        "timeout_ms": 120_000, "yield_time_ms": 120_000
                    }})
                )
                .is_ok()
            );
        }
        assert!(
            completion_for_call(&tools[2], &json!({"arguments":{"timeout_ms":120_000}})).is_err()
        );
    }

    #[test]
    fn external_mcp_call_returns_text_and_images_to_the_connector() {
        let broker = Arc::new(TurnBroker::default());
        let tools = crate::parse_callable_tools(Some(&json!([
            {"type":"namespace", "name":"mcp__photos", "tools":[
                {"type":"function", "name":"inspect", "parameters":{"type":"object"}}
            ]}
        ])))
        .unwrap();
        let token = broker
            .begin("external-mcp-turn".into(), tools.clone())
            .unwrap();
        let pending = invocation(
            &broker,
            &token,
            "mcp__photos.inspect",
            json!({"arguments":{"id":7}}),
        );
        let event = next_call(&broker, &token);
        assert_eq!(event.item["namespace"], "mcp__photos");
        assert_eq!(event.item["name"], "inspect");
        broker
            .resume(
                &[(
                    event.item["call_id"].as_str().unwrap().into(),
                    json!([
                        {"type":"input_text", "text":"photo found"},
                        {"type":"input_image", "image_url":"data:image/png;base64,aGVsbG8="}
                    ]),
                )],
                &tools,
            )
            .unwrap();
        let result = pending.join().unwrap().unwrap();
        assert_eq!(result["content"][0]["text"], "photo found");
        assert_eq!(
            result["content"][1],
            json!({"type":"image","mimeType":"image/png","data":"aGVsbG8="})
        );
    }

    #[test]
    fn connector_keeps_structured_mcp_errors_intact() {
        let error = json!({"isError":true,"content":[{"type":"text","text":"External server failed"}],
            "structuredContent":{"code":"unavailable"}});
        assert_eq!(mcp_tool_result(error.clone()), error);
        let native_error = mcp_tool_result(json!([
            {"type":"input_text","text":"web2codex://tool-result/failed/v1"},
            {"type":"input_text","text":"External server failed"}
        ]));
        assert_eq!(native_error["isError"], true);
        assert_eq!(
            native_error["content"],
            json!([{"type":"text","text":"External server failed"}])
        );
    }

    fn invocation(
        broker: &Arc<TurnBroker>,
        token: &str,
        name: &str,
        payload: Value,
    ) -> thread::JoinHandle<Result<Value, String>> {
        let broker = Arc::clone(broker);
        let mut request = payload;
        request["turn_token"] = json!(token);
        request["wire_name"] = json!(name);
        thread::spawn(move || broker.invoke(&request, || false))
    }

    fn next_call(broker: &TurnBroker, token: &str) -> NativeEvent {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(event) = broker.next_event(token).expect("active turn") {
                return event;
            }
            assert!(Instant::now() < deadline, "MCP call was not delivered");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn mcp_calls_make_native_round_trips_in_the_same_turn() {
        let broker = Arc::new(TurnBroker::default());
        let tools = registry();
        let token = broker.begin("browser-turn".into(), tools.clone()).unwrap();
        for command in ["first", "second"] {
            let pending = invocation(
                &broker,
                &token,
                "functions.exec_command",
                json!({"arguments":{"cmd":command}}),
            );
            let event = next_call(&broker, &token);
            assert!(!event.end_turn);
            assert_eq!(event.item["type"], "function_call");
            assert_eq!(event.item["namespace"], "functions");
            assert_eq!(event.item["name"], "exec_command");
            assert_eq!(
                serde_json::from_str::<Value>(event.item["arguments"].as_str().unwrap()).unwrap(),
                json!({"cmd":command})
            );
            let call_id = event.item["call_id"].as_str().unwrap().to_owned();
            assert!(!broker.complete("browser-turn", Some(0), Ok("too early".into())));
            assert_eq!(
                broker
                    .resume(&[(call_id, json!(format!("result: {command}")))], &tools)
                    .unwrap(),
                Some((token.clone(), "browser-turn".into()))
            );
            let returned = pending.join().unwrap().unwrap();
            assert_eq!(returned["content"][0]["text"], format!("result: {command}"));
        }
        let revision = {
            let mut state = broker.state.lock().unwrap();
            let turn = state.turns.get_mut(&token).unwrap();
            assert_eq!(turn.completed_tools, 2);
            turn.last_activity = Instant::now() - Duration::from_secs(3);
            turn.revision
        };
        assert!(!broker.complete(
            "browser-turn",
            Some(revision - 1),
            Ok("stale snapshot".into())
        ));
        assert!(broker.complete(
            "browser-turn",
            Some(revision),
            Ok("finished in the original chat".into())
        ));
        let final_event = broker.next_event(&token).unwrap().unwrap();
        assert!(final_event.end_turn);
        assert_eq!(
            final_event.item["content"][0]["text"],
            "finished in the original chat"
        );
    }

    #[test]
    fn freeform_patch_and_multimodal_result_keep_their_native_payloads() {
        let broker = Arc::new(TurnBroker::default());
        let tools = registry();
        let token = broker.begin("patch-turn".into(), tools.clone()).unwrap();
        let patch = "*** Begin Patch\n*** Add File: hello.txt\n+hello\n*** End Patch";
        let pending = invocation(
            &broker,
            &token,
            "functions.apply_patch",
            json!({"input":patch}),
        );
        let event = next_call(&broker, &token);
        assert_eq!(event.item["type"], "custom_tool_call");
        assert_eq!(event.item["input"], patch);
        broker
            .resume(
                &[(
                    event.item["call_id"].as_str().unwrap().into(),
                    json!([
                        {"type":"input_text", "text":"patched"},
                        {"type":"input_image", "image_url":"data:image/png;base64,aGVsbG8="}
                    ]),
                )],
                &tools,
            )
            .unwrap();
        let result = pending.join().unwrap().unwrap();
        assert_eq!(
            result["content"][0],
            json!({"type":"text","text":"patched"})
        );
        assert_eq!(
            result["content"][1],
            json!({"type":"image","mimeType":"image/png","data":"aGVsbG8="})
        );
    }

    #[test]
    fn concurrent_turns_are_isolated_and_cancellation_releases_mcp_waiters() {
        let broker = Arc::new(TurnBroker::default());
        let tools = registry();
        let parent = broker.begin("parent".into(), tools.clone()).unwrap();
        let child = broker.begin("child".into(), tools.clone()).unwrap();
        let parent_call = invocation(
            &broker,
            &parent,
            "functions.exec_command",
            json!({"arguments":{"cmd":"parent"}}),
        );
        let child_call = invocation(
            &broker,
            &child,
            "functions.exec_command",
            json!({"arguments":{"cmd":"child"}}),
        );
        let parent_event = next_call(&broker, &parent);
        let child_event = next_call(&broker, &child);
        let parent_id = parent_event.item["call_id"].as_str().unwrap().to_owned();
        let child_id = child_event.item["call_id"].as_str().unwrap().to_owned();
        assert!(
            broker
                .resume(
                    &[
                        (parent_id.clone(), json!("wrong")),
                        (child_id.clone(), json!("wrong"))
                    ],
                    &tools
                )
                .is_err()
        );
        broker
            .resume(&[(child_id, json!("child output"))], &tools)
            .unwrap();
        assert_eq!(
            child_call.join().unwrap().unwrap()["content"][0]["text"],
            "child output"
        );
        assert_eq!(broker.snapshot("parent")["pending_tools"], 1);
        broker.retire(&parent);
        assert!(parent_call.join().unwrap().is_err());
        assert!(broker.inventory(&json!({"turn_token":parent})).is_err());
        assert!(broker.next_event(&child).unwrap().is_none());
    }

    #[test]
    fn discovery_updates_the_turn_registry_and_rejects_unavailable_tools() {
        let broker = Arc::new(TurnBroker::default());
        let mut tools = registry();
        let token = broker.begin("search".into(), tools.clone()).unwrap();
        let inventory = broker
            .inventory(&json!({"turn_token":token, "query":"configured MCP"}))
            .unwrap();
        assert_eq!(
            inventory["discovery_tools"][0]["wire_name"],
            "functions.tool_search"
        );
        assert_eq!(
            inventory["discovery_tools"][0]["description"],
            "native discovery"
        );
        assert!(
            broker
                .invoke(
                    &json!({"turn_token":token, "wire_name":"unknown.tool", "arguments":{}}),
                    || false
                )
                .is_err()
        );
        assert!(broker.invoke(&json!({"turn_token":"expired", "wire_name":"functions.exec_command", "arguments":{}}), || false).is_err());
        let pending = invocation(
            &broker,
            &token,
            "functions.tool_search",
            json!({"arguments":{"query":"configured MCP"}}),
        );
        let event = next_call(&broker, &token);
        assert_eq!(event.item["type"], "tool_search_call");
        let specs = json!([{"type":"namespace", "name":"configured_mcp", "tools":[{"type":"function", "name":"read", "description":"discovered schema", "parameters":{"type":"object"}}]}]);
        tools.extend(crate::parse_callable_tools(Some(&specs)).unwrap());
        broker
            .resume(
                &[(
                    event.item["call_id"].as_str().unwrap().into(),
                    json!({"tools":specs}),
                )],
                &tools,
            )
            .unwrap();
        pending.join().unwrap().unwrap();
        let inventory = broker
            .inventory(&json!({"turn_token":token,"query":"configured_mcp"}))
            .unwrap();
        assert_eq!(inventory["total"], 1);
        assert_eq!(inventory["tools"][0]["wire_name"], "configured_mcp.read");
        assert_eq!(
            inventory["tools"][0]["parameters"],
            json!({"type":"object"})
        );
    }

    #[test]
    fn streamed_text_is_append_only_and_resumed_responses_start_at_the_new_segment() {
        let broker = Arc::new(TurnBroker::default());
        let tools = registry();
        let token = broker.begin("streamed-turn".into(), tools.clone()).unwrap();
        broker.begin_response(&token).unwrap();
        assert!(broker.append_text("streamed-turn", "before tool ".into()));
        let first_delta = broker.next_event(&token).unwrap().unwrap();
        assert_eq!(first_delta.text_delta.as_deref(), Some("before tool "));

        let pending = invocation(
            &broker,
            &token,
            "functions.exec_command",
            json!({"arguments":{"cmd":"inspect"}}),
        );
        let tool_call = next_call(&broker, &token);
        assert_eq!(tool_call.segment_text, "before tool ");
        let call_id = tool_call.item["call_id"].as_str().unwrap().to_owned();
        broker
            .resume(&[(call_id, json!("native result"))], &tools)
            .unwrap();
        pending.join().unwrap().unwrap();

        broker.begin_response(&token).unwrap();
        assert!(broker.append_text("streamed-turn", "after tool".into()));
        let second_delta = broker.next_event(&token).unwrap().unwrap();
        assert_eq!(second_delta.text_delta.as_deref(), Some("after tool"));
        let revision = {
            let mut state = broker.state.lock().unwrap();
            let turn = state.turns.get_mut(&token).unwrap();
            turn.last_activity = Instant::now() - Duration::from_secs(3);
            turn.revision
        };
        assert!(broker.complete(
            "streamed-turn",
            Some(revision),
            Ok("before tool after tool".into())
        ));
        let final_event = broker.next_event(&token).unwrap().unwrap();
        assert_eq!(final_event.segment_text, "after tool");
        assert!(final_event.end_turn);
    }

    #[test]
    fn transport_cancellation_stops_the_turn_without_executing_anything() {
        let broker = TurnBroker::default();
        let token = broker.begin("cancel".into(), registry()).unwrap();
        assert!(broker.invoke(&json!({"turn_token":token,"wire_name":"functions.exec_command","arguments":{"cmd":"never executed"}}), || true).is_err());
        assert!(broker.next_event(&token).is_err());
        assert_eq!(broker.snapshot("cancel")["active"], false);
    }
}
