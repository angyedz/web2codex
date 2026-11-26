//! Turn-bound bridge between the ChatGPT connector and Codex's native tool loop.
//! This module never executes a tool. Only the outer Codex tool router does that.

use std::collections::{HashMap, VecDeque};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    BridgeCompletion, CallableTool, CallableToolKind, ToolChoice, response_item_for_completion,
};

const MAX_ACTIVE_BROWSER_TURNS: usize = 5;
const MAX_TURNS: usize = 16;
const MAX_PENDING_CALLS: usize = 32;
// A slow native tool can span several short, independent MCP requests.
const MCP_RESULT_WAIT: Duration = Duration::from_secs(12);
const MCP_CALL_RESULT_TTL: Duration = Duration::from_secs(30 * 60);
const ORPHANED_TURN_TTL: Duration = Duration::from_secs(60 * 60);
const COMPLETION_QUIET_PERIOD: Duration = Duration::from_secs(2);

pub(crate) struct TurnBroker {
    state: Mutex<State>,
    changed: Condvar,
}

impl Default for TurnBroker {
    fn default() -> Self {
        Self {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        }
    }
}

#[derive(Default)]
struct State {
    turns: HashMap<String, Turn>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) enum TurnIdentity {
    #[default]
    Anonymous,
    Thread(String),
    SetupSmokeTest,
}

struct Turn {
    request_id: String,
    identity: TurnIdentity,
    orphaned_at: Option<Instant>,
    observer_epoch: u64,
    observer_retired: bool,
    expected_result: Option<String>,
    tools: Vec<CallableTool>,
    tool_choice: ToolChoice,
    queue: VecDeque<String>,
    calls: HashMap<String, Invocation>,
    retrieved_results: VecDeque<RetrievedResult>,
    result: Option<Result<String, String>>,
    text: String,
    text_cursor: usize,
    segment_start: usize,
    reasoning_headings: Vec<String>,
    reasoning_cursor: usize,
    reasoning_segment_start: usize,
    reasoning_item_id: Option<String>,
    revision: u64,
    last_activity: Instant,
    connector_requests: u64,
    tool_invocations: u64,
    completed_tools: u64,
}

struct Invocation {
    wire_name: String,
    item: Value,
    output: Option<Value>,
    delivered: bool,
    created_at: Instant,
    expired: bool,
    last_poll: Option<Instant>,
}

struct RetrievedResult {
    call_id: String,
    output: Value,
    bytes: usize,
    retrieved_at: Instant,
}

impl Turn {
    fn expire_stale_calls(&mut self, now: Instant) {
        let mut undispatched = Vec::new();
        let mut expired = false;
        for (call_id, call) in &mut self.calls {
            let idle_since = call.last_poll.unwrap_or(call.created_at);
            if call.expired || now.saturating_duration_since(idle_since) < MCP_CALL_RESULT_TTL {
                continue;
            }
            call.expired = true;
            expired = true;
            if call.delivered {
                // The native operation may have side effects. Keep its identity
                // and accept a late output, but release the potentially large
                // serialized arguments retained in the Response item.
                call.item = Value::Null;
                call.output = None;
            } else {
                // This queued operation never reached Codex, so it is safe to
                // complete it as expired without running it later.
                let output = call.output.take().unwrap_or_else(|| json!({"isError":true,"content":[{"type":"text",
                    "text":"The native call expired before Codex dispatched it. No operation was run. Refresh the inventory before trying again."
                }]}));
                undispatched.push((call_id.clone(), output));
            }
        }
        for (call_id, output) in undispatched {
            self.queue.retain(|queued| queued != &call_id);
            self.calls.remove(&call_id);
            self.cache_result(&call_id, &output);
        }
        self.retrieved_results
            .retain(|result| result.retrieved_at.elapsed() < MCP_CALL_RESULT_TTL);
        if expired {
            self.revision += 1;
            self.last_activity = Instant::now();
        }
    }

    fn has_recoverable_calls(&self) -> bool {
        self.calls
            .values()
            .any(|call| call.delivered || call.last_poll.is_some() || call.output.is_some())
    }

    fn has_recoverable_result(&self) -> bool {
        !self.retrieved_results.is_empty()
    }

    fn cache_result(&mut self, call_id: &str, output: &Value) {
        // Preserve concurrent result polls within a bounded cache. A single
        // large media result may occupy the whole cache, as before.
        let bytes = output.to_string().len();
        while self.retrieved_results.len() >= MAX_PENDING_CALLS
            || (!self.retrieved_results.is_empty()
                && self
                    .retrieved_results
                    .iter()
                    .map(|result| result.bytes)
                    .sum::<usize>()
                    .saturating_add(bytes)
                    > crate::MAX_REQUEST_BODY_BYTES)
        {
            self.retrieved_results.pop_front();
        }
        self.retrieved_results.push_back(RetrievedResult {
            call_id: call_id.to_owned(),
            output: output.clone(),
            bytes,
            retrieved_at: Instant::now(),
        });
    }
}

fn pending_call_result(call_id: &str) -> Value {
    json!({
        "content":[{"type":"text","text":format!(
            "Native call {call_id} is pending execution or approval. Use codex_tool_result with this call_id and the same turn_token to collect it. Do not submit the native call again."
        )}],
        "structuredContent":{"status":"pending","call_id":call_id},
        "isError":false
    })
}

pub(crate) struct NativeEvent {
    pub(crate) item: Value,
    pub(crate) end_turn: bool,
    pub(crate) text_delta: Option<String>,
    pub(crate) reasoning_heading: Option<ReasoningHeading>,
    pub(crate) segment_text: String,
}

pub(crate) struct ReasoningHeading {
    pub(crate) item_id: String,
    pub(crate) summary_index: usize,
    pub(crate) text: String,
}

impl TurnBroker {
    #[cfg(test)]
    pub(crate) fn begin(
        &self,
        request_id: String,
        tools: Vec<CallableTool>,
        tool_choice: ToolChoice,
    ) -> Result<String, String> {
        self.begin_scoped(request_id, tools, tool_choice, TurnIdentity::default())
    }

    pub(crate) fn begin_scoped(
        &self,
        request_id: String,
        tools: Vec<CallableTool>,
        tool_choice: ToolChoice,
        identity: TurnIdentity,
    ) -> Result<String, String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::prune_expired_turns(&mut state);
        let active_browser_turns = state
            .turns
            .values()
            .filter(|turn| !turn.observer_retired && turn.result.is_none())
            .count();
        if active_browser_turns >= MAX_ACTIVE_BROWSER_TURNS {
            return Err(format!(
                "ChatGPT Web supports at most {MAX_ACTIVE_BROWSER_TURNS} simultaneous browser turns; close or finish a browser tab before starting another"
            ));
        }
        // Completed result-poll caches must not consume the active-turn quota.
        // Evict the oldest cache when the broker needs room for a new request.
        while state.turns.len() >= MAX_TURNS {
            let oldest = state
                .turns
                .iter()
                .filter(|(_, turn)| {
                    turn.observer_retired && turn.result.is_some() && turn.calls.is_empty()
                })
                .min_by_key(|(_, turn)| turn.last_activity)
                .map(|(token, _)| token.clone());
            let Some(token) = oldest else { break };
            state.turns.remove(&token);
        }
        if state.turns.len() >= MAX_TURNS {
            return Err("Too many active ChatGPT turns; finish or cancel an existing turn".into());
        }
        let token = format!("turn_{}", Uuid::new_v4().simple());
        state.turns.insert(
            token.clone(),
            Turn {
                request_id,
                identity,
                orphaned_at: None,
                observer_epoch: 0,
                observer_retired: false,
                expected_result: None,
                tools,
                tool_choice,
                queue: VecDeque::new(),
                calls: HashMap::new(),
                retrieved_results: VecDeque::new(),
                result: None,
                text: String::new(),
                text_cursor: 0,
                segment_start: 0,
                reasoning_headings: Vec::new(),
                reasoning_cursor: 0,
                reasoning_segment_start: 0,
                reasoning_item_id: None,
                revision: 0,
                last_activity: Instant::now(),
                connector_requests: 0,
                tool_invocations: 0,
                completed_tools: 0,
            },
        );
        Ok(token)
    }

    fn prune_expired_turns(state: &mut State) {
        let now = Instant::now();
        for turn in state.turns.values_mut() {
            turn.expire_stale_calls(now);
        }
        state.turns.retain(|_, turn| {
            !turn
                .orphaned_at
                .is_some_and(|time| time.elapsed() >= ORPHANED_TURN_TTL)
        });
    }

    #[cfg(test)]
    pub(crate) fn resume(
        &self,
        outputs: &[(String, Value)],
        tools: &[CallableTool],
    ) -> Result<Option<(String, String, u64)>, String> {
        self.resume_scoped(outputs, tools, &TurnIdentity::default())
    }

    /// Historical outputs are ignored. An output belonging to a live invocation
    /// resumes that exact browser turn only for its original native thread.
    pub(crate) fn resume_scoped(
        &self,
        outputs: &[(String, Value)],
        tools: &[CallableTool],
        identity: &TurnIdentity,
    ) -> Result<Option<(String, String, u64)>, String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::prune_expired_turns(&mut state);
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
        if &turn.identity != identity {
            return Err("Tool output belongs to a different Codex thread or request scope".into());
        }
        // Only the result requested by the previous Responses observer can
        // claim this turn. A replay must not steal its text or dequeue a tool.
        let expected = turn
            .expected_result
            .as_deref()
            .ok_or("A Responses request is already observing this ChatGPT turn")?;
        let mut matching = outputs.iter().filter(|(id, _)| id == expected);
        let (_, output) = matching
            .next()
            .ok_or("Missing the outstanding native call result")?;
        if matching.next().is_some() {
            return Err("The outstanding native call has duplicate result entries".into());
        }
        // Validate the entire batch before publishing any native result.
        if outputs
            .iter()
            .any(|(id, _)| turn.calls.get(id).is_some_and(|call| !call.delivered))
        {
            return Err("Received a result for a tool that Codex has not received".into());
        }
        let call = turn
            .calls
            .get_mut(expected)
            .ok_or("The outstanding native call expired")?;
        call.output = Some(output.clone());
        turn.expected_result = None;
        turn.orphaned_at = None;
        turn.observer_retired = false;
        // A queued invocation was validated against the previous request's
        // registry. Its definition may have been withdrawn on continuation.
        let calls = &mut turn.calls;
        let previous_tools = &turn.tools;
        turn.queue.retain(|id| {
            let Some(call) = calls.get_mut(id) else {
                return false;
            };
            let prior = previous_tools.iter().find(|tool| wire_name(tool) == call.wire_name);
            if prior.is_some_and(|prior| tools.iter().any(|tool| {
                tool.namespace == prior.namespace
                    && tool.name == prior.name
                    && tool.kind == prior.kind
                    && tool.declaration == prior.declaration
            })) {
                return true;
            }
            call.output = Some(json!({"isError":true,"content":[{"type":"text",
                "text":"Codex changed this tool's availability or schema before dispatch. Refresh the inventory before requesting it again."
            }]}));
            false
        });
        turn.tools = tools.to_vec();
        turn.revision += 1;
        turn.last_activity = Instant::now();
        turn.observer_epoch += 1;
        turn.segment_start = turn.text_cursor;
        turn.reasoning_segment_start = turn.reasoning_cursor;
        turn.reasoning_item_id = None;
        let request_id = turn.request_id.clone();
        self.changed.notify_all();
        Ok(Some((token, request_id, turn.observer_epoch)))
    }

    pub(crate) fn append_text(&self, request_id: &str, delta: String) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::prune_expired_turns(&mut state);
        let Some(turn) = state
            .turns
            .values_mut()
            .find(|turn| turn.request_id == request_id)
        else {
            return false;
        };
        if turn.result.is_some() || delta.is_empty() {
            return false;
        }
        if turn.text.len().saturating_add(delta.len()) > crate::MAX_RESPONSE_TEXT_BYTES {
            turn.result = Some(Err("ChatGPT response exceeded the bridge size limit".into()));
            turn.last_activity = Instant::now();
            self.changed.notify_all();
            return false;
        }
        turn.text.push_str(&delta);
        turn.revision += 1;
        turn.last_activity = Instant::now();
        self.changed.notify_all();
        true
    }

    pub(crate) fn append_reasoning_heading(&self, request_id: &str, heading: String) -> bool {
        let heading = heading.split_whitespace().collect::<Vec<_>>().join(" ");
        let heading = heading.trim_matches('*').trim().to_string();
        if heading.is_empty() || heading.chars().count() > 180 {
            return false;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::prune_expired_turns(&mut state);
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
        if turn.reasoning_headings.last() == Some(&heading) {
            return true;
        }
        turn.reasoning_headings.push(heading);
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
        Self::prune_expired_turns(&mut state);
        let turn = state
            .turns
            .get_mut(token)
            .ok_or("Unknown or expired turn_token; copy it from the current request")?;
        if turn.result.as_ref().is_some_and(Result::is_ok)
            || (turn.result.is_some()
                && !turn.has_recoverable_calls()
                && !turn.has_recoverable_result())
        {
            return Err("This ChatGPT turn has already ended".into());
        }
        turn.connector_requests = turn.connector_requests.saturating_add(1);
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
        let terminal_error = turn.result.is_some();
        let matching = turn
            .tools
            .iter()
            .filter(|tool| {
                !terminal_error
                    && (query.is_empty()
                        || format!("{} {}", wire_name(tool), tool.declaration)
                            .to_lowercase()
                            .contains(&query))
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
            "pending_calls": turn.calls.iter().filter(|(_, call)| call.last_poll.is_some())
                .map(|(id, call)| json!({
                    "call_id": id, "wire_name": call.wire_name,
                    "status": if call.output.is_some() { "ready" } else if call.expired { "unknown" } else { "pending" }
                })).chain(turn.retrieved_results.iter().map(|result| json!({
                    "call_id": result.call_id, "status": "collected"
                }))).collect::<Vec<_>>(),
            "discovery_tools": turn.tools.iter().filter(|tool| !terminal_error && tool.kind == CallableToolKind::ToolSearch)
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
        Self::prune_expired_turns(&mut state);
        let turn = state
            .turns
            .get_mut(&token)
            .ok_or("Unknown or expired turn_token; copy it from the current request")?;
        if turn.result.is_some() {
            return Err("This ChatGPT turn has already ended".into());
        }
        if let Some((id, _)) = turn.calls.iter().find(|(_, call)| call.last_poll.is_some()) {
            return Err(format!(
                "A native call is awaiting collection. Use codex_tool_result with call_id {id} before starting another operation."
            ));
        }
        if turn.calls.len() >= MAX_PENDING_CALLS {
            return Err("Too many pending tools in this turn".into());
        }
        let tool = turn.tools.iter().find(|tool| {
            wire_name(tool) == name && tool_is_available(&turn.tool_choice, tool)
        })
            .ok_or_else(|| format!("Tool is not available in this turn: {name}. Discover tools through the MCP inventory."))?;
        let completion = completion_for_call(tool, request)?;
        turn.connector_requests = turn.connector_requests.saturating_add(1);
        turn.tool_invocations = turn.tool_invocations.saturating_add(1);
        let call_id = format!("call_{}", Uuid::new_v4().simple());
        let item_id = format!("tool_{}", Uuid::new_v4().simple());
        let (item, _) = response_item_for_completion("", &item_id, &call_id, completion);
        turn.calls.insert(
            call_id.clone(),
            Invocation {
                wire_name: name.to_owned(),
                item,
                output: None,
                delivered: false,
                created_at: Instant::now(),
                expired: false,
                last_poll: None,
            },
        );
        turn.queue.push_back(call_id.clone());
        turn.revision += 1;
        turn.last_activity = Instant::now();
        self.changed.notify_all();
        self.wait_for_result(state, &token, &call_id, disconnected)
    }

    pub(crate) fn poll_result(
        &self,
        request: &Value,
        disconnected: impl Fn() -> bool,
    ) -> Result<Value, String> {
        let token = required_string(request, "turn_token")?;
        let call_id = required_string(request, "call_id")?;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::prune_expired_turns(&mut state);
        if let Some(call) = state
            .turns
            .get_mut(token)
            .and_then(|turn| turn.calls.get_mut(call_id))
        {
            if call.last_poll.is_none() {
                return Err("This call is still attached to its original connector request".into());
            }
            if !call.expired || call.output.is_some() {
                call.last_poll = Some(Instant::now());
            }
        }
        self.wait_for_result(state, token, call_id, disconnected)
    }

    fn wait_for_result(
        &self,
        mut state: MutexGuard<'_, State>,
        token: &str,
        call_id: &str,
        disconnected: impl Fn() -> bool,
    ) -> Result<Value, String> {
        let deadline = Instant::now() + MCP_RESULT_WAIT;
        loop {
            let turn = state
                .turns
                .get_mut(token)
                .ok_or("The Codex turn was cancelled or disconnected")?;
            if let Some(index) = turn
                .retrieved_results
                .iter()
                .position(|result| result.call_id == call_id)
            {
                let mut result = turn
                    .retrieved_results
                    .remove(index)
                    .expect("cache was checked");
                let output = result.output.clone();
                result.retrieved_at = Instant::now();
                turn.retrieved_results.push_back(result);
                return Ok(output);
            }
            let call = turn.calls.get_mut(call_id).ok_or(
                "Unknown or expired call_id for this turn. Check codex_tool_inventory for pending_calls; do not resubmit the native operation.",
            )?;
            if disconnected() {
                if call.delivered || call.last_poll.is_some() {
                    // Dispatch may already have had side effects. Keep both the
                    // handle and any arriving result reachable by this turn.
                    call.last_poll = Some(Instant::now());
                    if turn.result.is_some() {
                        turn.orphaned_at.get_or_insert_with(Instant::now);
                    }
                } else {
                    // The mutex still excludes next_event, so it is safe to drop
                    // this call before it can reach the native tool router and
                    // retire the now-unobserved turn.
                    turn.queue.retain(|queued| queued != call_id);
                    turn.calls.remove(call_id);
                    turn.revision += 1;
                    turn.result = Some(Err(
                        "The connector disconnected before the native call was dispatched".into(),
                    ));
                }
                self.changed.notify_all();
                return Err("The connector disconnected from its native call".into());
            }
            if call.expired && call.output.is_none() {
                return Err("The native call's 30-minute result window expired. Its side-effect status is unknown; do not submit it again.".into());
            }
            if let Some(output) = call.output.take() {
                let recoverable = call.last_poll.is_some() || turn.orphaned_at.is_some();
                turn.calls.remove(call_id);
                let result = mcp_tool_result(output);
                if recoverable {
                    // Make a repeated result poll idempotent without retaining
                    // the full history of potentially large media responses.
                    turn.cache_result(call_id, &result);
                }
                turn.revision += 1;
                turn.completed_tools += 1;
                turn.last_activity = Instant::now();
                self.changed.notify_all();
                return Ok(result);
            }
            if let Some(Err(error)) = &turn.result {
                let error = error.clone();
                let call = turn.calls.get_mut(call_id).expect("call was checked above");
                if call.delivered {
                    call.last_poll = Some(Instant::now());
                    turn.orphaned_at.get_or_insert_with(Instant::now);
                    self.changed.notify_all();
                    return Ok(pending_call_result(call_id));
                } else {
                    turn.queue.retain(|queued| queued != call_id);
                    turn.calls.remove(call_id);
                }
                self.changed.notify_all();
                return Err(error);
            }
            if turn.result.as_ref().is_some_and(Result::is_ok) {
                return Err("The ChatGPT turn ended before the native tool result arrived".into());
            }
            if Instant::now() >= deadline {
                call.last_poll = Some(Instant::now());
                return Ok(pending_call_result(call_id));
            }
            state = self
                .changed
                .wait_timeout(state, Duration::from_millis(100))
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }

    pub(crate) fn next_event(
        &self,
        token: &str,
        observer_epoch: u64,
    ) -> Result<Option<NativeEvent>, String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::prune_expired_turns(&mut state);
        let turn = state
            .turns
            .get_mut(token)
            .ok_or("The ChatGPT turn has been cancelled")?;
        if turn.observer_epoch != observer_epoch {
            return Err(
                "This Responses observer has been superseded by a newer continuation".into(),
            );
        }
        if turn.reasoning_cursor < turn.reasoning_headings.len() {
            let first = turn.reasoning_item_id.is_none();
            let item_id = turn
                .reasoning_item_id
                .get_or_insert_with(|| format!("rs_{}", Uuid::new_v4().simple()))
                .clone();
            let summary_index = turn.reasoning_cursor - turn.reasoning_segment_start;
            let heading = turn.reasoning_headings[turn.reasoning_cursor].clone();
            turn.reasoning_cursor += 1;
            return Ok(Some(NativeEvent {
                item: if first {
                    json!({
                        "id": item_id,
                        "type": "reasoning",
                        "summary": [],
                        "content": [],
                        "encrypted_content": null
                    })
                } else {
                    Value::Null
                },
                end_turn: false,
                text_delta: None,
                reasoning_heading: Some(ReasoningHeading {
                    item_id,
                    summary_index,
                    text: format!("**{heading}**\n\n<!-- -->"),
                }),
                segment_text: String::new(),
            }));
        }
        if turn.text_cursor < turn.text.len() {
            let delta = turn.text[turn.text_cursor..].to_string();
            turn.text_cursor = turn.text.len();
            return Ok(Some(NativeEvent {
                item: Value::Null,
                end_turn: false,
                text_delta: Some(delta),
                reasoning_heading: None,
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
                reasoning_heading: None,
                segment_text,
            }));
        }
        if turn.expected_result.is_some() {
            return Ok(None);
        }
        while let Some(call_id) = turn.queue.pop_front() {
            if let Some(call) = turn.calls.get_mut(&call_id) {
                call.delivered = true;
                turn.expected_result = Some(call_id);
                let item = std::mem::replace(&mut call.item, Value::Null);
                return Ok(Some(NativeEvent {
                    item,
                    end_turn: false,
                    text_delta: None,
                    reasoning_heading: None,
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
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::prune_expired_turns(&mut state);
        let Some(turn) = state
            .turns
            .values()
            .find(|turn| turn.request_id == request_id)
        else {
            return json!({"active":false});
        };
        json!({
            "active":turn.result.is_none(), "pending_tools":turn.calls.len(),
            "revision":turn.revision, "completed_tools":turn.completed_tools,
            "connector_requests":turn.connector_requests,
            "tool_invocations":turn.tool_invocations,
            "quiet_ms":turn.last_activity.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
        })
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
        Self::prune_expired_turns(&mut state);
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
            if matches!(
                &turn.tool_choice,
                ToolChoice::Required | ToolChoice::Named { .. }
            ) && turn.completed_tools == 0
            {
                turn.result = Some(Err(
                    "tool_choice required a Web2Codex connector call, but ChatGPT answered without invoking one".into(),
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

    pub(crate) fn retire_observer(&self, token: &str, epoch: u64) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::prune_expired_turns(&mut state);
        if state
            .turns
            .get(token)
            .is_some_and(|turn| turn.observer_epoch != epoch)
        {
            return false;
        }
        let preserve = state
            .turns
            .get(token)
            .is_some_and(|turn| turn.has_recoverable_calls() || turn.has_recoverable_result());
        if preserve {
            if let Some(turn) = state.turns.get_mut(token) {
                turn.queue.clear();
                turn.calls.retain(|_, call| {
                    call.delivered || call.last_poll.is_some() || call.output.is_some()
                });
                for call in turn.calls.values_mut() {
                    call.item = Value::Null;
                    if !call.delivered && call.output.is_none() {
                        // A polled but undispatched call cannot receive a native
                        // result after the observer retires. Resolve it without
                        // claiming that an operation may have run.
                        call.output = Some(json!({"isError":true,"content":[{"type":"text",
                            "text":"The Responses observer ended before Codex dispatched this native call. No operation was run."
                        }]}));
                    }
                    if call.delivered || call.output.is_some() {
                        call.last_poll = Some(Instant::now());
                    }
                }
                if turn.result.is_none() {
                    turn.result = Some(Err(
                        "The Responses observer ended before this native call result arrived"
                            .into(),
                    ));
                }
                turn.orphaned_at.get_or_insert_with(Instant::now);
                turn.observer_retired = true;
                turn.observer_epoch = turn.observer_epoch.wrapping_add(1);
                turn.last_activity = Instant::now();
            }
        } else {
            state.turns.remove(token);
        }
        self.changed.notify_all();
        true
    }

    pub(crate) fn stop(&self, message: &str) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::prune_expired_turns(&mut state);
        for turn in state.turns.values_mut() {
            turn.result = Some(Err(message.into()));
        }
        self.changed.notify_all();
    }
}

fn wire_name(tool: &CallableTool) -> String {
    format!("{}.{}", tool.namespace, tool.name)
}

fn tool_is_available(choice: &ToolChoice, tool: &CallableTool) -> bool {
    match choice {
        ToolChoice::Auto | ToolChoice::Required => true,
        ToolChoice::None => false,
        ToolChoice::Named {
            kind,
            namespace,
            name,
        } => tool.kind == *kind && tool.namespace == *namespace && tool.name == *name,
    }
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
    if output.is_object() {
        if output.get("content").is_some_and(Value::is_array) {
            return output;
        }
        if output.get("structuredContent").is_some() {
            let mut result = output;
            result["content"] = json!([]);
            return result;
        }
        let is_error = output.get("isError").and_then(Value::as_bool) == Some(true);
        return json!({
            "content": [],
            "structuredContent": output,
            "isError": is_error,
        });
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
                    } else {
                        content.push(json!({
                            "type": "text",
                            "text": "A media result could not be forwarded because it was not inline base64.",
                        }));
                    }
                }
                Some("input_file") => {
                    let file_url = part.get("file_url").and_then(Value::as_str).unwrap_or_default();
                    let filename = part.get("filename").and_then(Value::as_str).unwrap_or("mcp-resource.bin");
                    if let Some((mime, data)) = inline_file_parts(file_url) {
                        content.push(json!({
                            "type": "resource",
                            "resource": {
                                "uri": attachment_resource_uri(filename),
                                "mimeType": mime,
                                "blob": data,
                            }
                        }));
                    } else {
                        content.push(json!({
                            "type": "text",
                            "text": format!("File result {} is not an inline base64 attachment and could not be forwarded", safe_attachment_name(filename)),
                        }));
                    }
                }
                _ => {
                    let kind = part.get("type").and_then(Value::as_str).unwrap_or("unknown");
                    content.push(json!({
                        "type": "text",
                        "text": format!("Unsupported MCP result block: {kind}"),
                    }));
                }
            }
        }
    } else if let Some(text) = output.as_str() {
        content.push(json!({"type": "text", "text": text}));
    } else if !output.is_null() {
        content.push(json!({"type": "text", "text": output.to_string()}));
    }
    json!({"content": content, "isError": is_error})
}

fn inline_file_parts(data_url: &str) -> Option<(&str, &str)> {
    let (header, data) = data_url.split_once(',')?;
    let header = header.strip_prefix("data:")?.strip_suffix(";base64")?;
    let mime = header.split(';').next()?.trim();
    let (type_, subtype) = mime.split_once('/')?;
    let valid_token = |token: &str| {
        !token.is_empty()
            && token.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(
                        byte,
                        b'!' | b'#'..=b'$'
                            | b'&'..=b'\''
                            | b'*'
                            | b'+'
                            | b'-'
                            | b'.'
                            | b'^'
                            | b'_'
                            | b'`'
                            | b'|'
                            | b'~'
                    )
            })
    };
    if !valid_token(type_) || !valid_token(subtype) || subtype.contains('/') || data.is_empty() {
        return None;
    }
    Some((mime, data))
}

fn safe_attachment_name(filename: &str) -> String {
    let basename = filename.rsplit(['/', '\\']).next().unwrap_or_default();
    let name = basename
        .chars()
        .filter(|character| {
            character.is_alphanumeric() || matches!(character, ' ' | '.' | '-' | '_')
        })
        .take(120)
        .collect::<String>();
    let name = name.trim().trim_matches('.');
    if name.is_empty() {
        "mcp-resource.bin".to_string()
    } else {
        name.to_string()
    }
}

fn attachment_resource_uri(filename: &str) -> String {
    let filename = safe_attachment_name(filename);
    let mut uri = url::Url::parse("web2codex://attachment/")
        .expect("the built-in attachment URI base is valid");
    uri.path_segments_mut()
        .expect("the built-in attachment URI is hierarchical")
        .push(&filename);
    uri.to_string()
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
            .begin("external-mcp-turn".into(), tools.clone(), ToolChoice::Auto)
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

    #[test]
    fn structured_only_mcp_results_remain_native_instead_of_becoming_json_text() {
        let result = mcp_tool_result(json!({
            "structuredContent": {"private_marker": "MCP_NATIVE_ONLY"}
        }));

        assert_eq!(result["content"], json!([]));
        assert_eq!(
            result["structuredContent"],
            json!({"private_marker": "MCP_NATIVE_ONLY"})
        );
        assert!(!result["content"].to_string().contains("MCP_NATIVE_ONLY"));
    }

    #[test]
    fn unsupported_mcp_blocks_do_not_echo_structured_payloads_as_text() {
        let result = mcp_tool_result(json!([{
            "type": "future_video_part",
            "payload": "PRIVATE_PAYLOAD_MARKER"
        }]));

        assert_eq!(
            result["content"],
            json!([{"type":"text", "text":"Unsupported MCP result block: future_video_part"}])
        );
        assert!(
            !result["content"]
                .to_string()
                .contains("PRIVATE_PAYLOAD_MARKER")
        );
    }

    #[test]
    fn non_inline_media_results_do_not_echo_urls_or_payload_fields_as_json() {
        let result = mcp_tool_result(json!([{
            "type": "input_image",
            "image_url": "https://example.invalid/private-image",
            "private_marker": "MEDIA_PAYLOAD_MARKER"
        }]));

        assert_eq!(
            result["content"],
            json!([{
                "type":"text",
                "text":"A media result could not be forwarded because it was not inline base64."
            }])
        );
        assert!(!result["content"].to_string().contains("private-image"));
        assert!(
            !result["content"]
                .to_string()
                .contains("MEDIA_PAYLOAD_MARKER")
        );
    }

    #[test]
    fn connector_returns_mcp_files_as_embedded_resource_blobs() {
        let result = mcp_tool_result(json!([{
            "type": "input_file",
            "file_url": "data:application/pdf;base64,JVBERi0xLjQ=",
            "filename": r"C:\workspace\report final.pdf"
        }]));

        assert_eq!(
            result["content"],
            json!([{
                "type": "resource",
                "resource": {
                    "uri": "web2codex://attachment/report%20final.pdf",
                    "mimeType": "application/pdf",
                    "blob": "JVBERi0xLjQ="
                }
            }])
        );
        assert!(!result.to_string().contains("file_url"));
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
        next_call_at(broker, token, 0)
    }

    fn next_call_at(broker: &TurnBroker, token: &str, epoch: u64) -> NativeEvent {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(event) = broker.next_event(token, epoch).expect("active turn") {
                return event;
            }
            assert!(Instant::now() < deadline, "MCP call was not delivered");
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn collaboration_spawn_payload_is_preserved_by_the_mcp_bridge() {
        let broker = Arc::new(TurnBroker::default());
        let tools = crate::parse_callable_tools(Some(&json!([{
            "type":"namespace", "name":"collaboration", "tools":[
                {"type":"function", "name":"spawn_agent", "parameters":{"type":"object"}}
            ]
        }])))
        .expect("valid collaboration tool registry");
        let token = broker
            .begin("collaboration-spawn-turn".into(), tools.clone())
            .unwrap();
        let arguments = json!({
            "task_name":"mcp_payload_probe",
            "message":"Read the smoke marker and return exactly AGENT_OK.",
            "fork_turns":"none"
        });
        let pending = invocation(
            &broker,
            &token,
            "collaboration.spawn_agent",
            json!({"arguments":arguments.clone()}),
        );

        let event = next_call(&broker, &token);
        assert_eq!(event.item["type"], "function_call");
        assert_eq!(event.item["namespace"], "collaboration");
        assert_eq!(event.item["name"], "spawn_agent");
        assert_eq!(
            serde_json::from_str::<Value>(event.item["arguments"].as_str().unwrap()).unwrap(),
            arguments,
            "the bridge must preserve the complete child task message"
        );

        let call_id = event.item["call_id"].as_str().unwrap().to_owned();
        broker
            .resume(&[(call_id, json!("agent started"))], &tools)
            .unwrap();
        assert_eq!(
            pending.join().unwrap().unwrap()["content"][0]["text"],
            "agent started"
        );
    }

    #[test]
    fn mcp_calls_make_native_round_trips_in_the_same_turn() {
        let broker = Arc::new(TurnBroker::default());
        let tools = registry();
        let token = broker.begin("browser-turn".into(), tools.clone()).unwrap();
        for (index, command) in ["first", "second"].into_iter().enumerate() {
            let pending = invocation(
                &broker,
                &token,
                "functions.exec_command",
                json!({"arguments":{"cmd":command}}),
            );
            let event = next_call_at(&broker, &token, index as u64);
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
                Some((token.clone(), "browser-turn".into(), index as u64 + 1))
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
        let delta = broker.next_event(&token, 2).unwrap().unwrap();
        assert_eq!(
            delta.text_delta.as_deref(),
            Some("finished in the original chat")
        );
        let final_event = broker.next_event(&token, 2).unwrap().unwrap();
        assert!(final_event.end_turn);
        assert_eq!(
            final_event.item["content"][0]["text"],
            "finished in the original chat"
        );
    }

    #[test]
    fn concurrent_native_calls_in_one_turn_keep_results_with_their_call_ids() {
        let broker = Arc::new(TurnBroker::default());
        let tools = registry();
        let request_id = "concurrent-tools";
        let token = broker.begin(request_id.into(), tools.clone()).unwrap();

        let first = invocation(
            &broker,
            &token,
            "functions.exec_command",
            json!({"arguments":{"cmd":"first"}}),
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        while broker
            .state
            .lock()
            .unwrap()
            .turns
            .get(&token)
            .is_none_or(|turn| turn.queue.len() != 1)
        {
            assert!(Instant::now() < deadline, "first MCP call was not queued");
            thread::sleep(Duration::from_millis(1));
        }

        // The first invocation is blocked waiting for its result while the
        // second MCP request arrives, so both requests are live concurrently.
        let second = invocation(
            &broker,
            &token,
            "functions.exec_command",
            json!({"arguments":{"cmd":"second"}}),
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        while broker
            .state
            .lock()
            .unwrap()
            .turns
            .get(&token)
            .is_none_or(|turn| turn.queue.len() != 2)
        {
            assert!(Instant::now() < deadline, "second MCP call was not queued");
            thread::sleep(Duration::from_millis(1));
        }
        // Simulate both connector requests having returned their pending call
        // IDs. Their results must remain individually retrievable afterwards.
        {
            let mut state = broker.state.lock().unwrap();
            for call in state.turns.get_mut(&token).unwrap().calls.values_mut() {
                call.last_poll = Some(Instant::now());
            }
        }

        let first_event = next_call_at(&broker, &token, 0);
        let first_arguments: Value =
            serde_json::from_str(first_event.item["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(first_arguments["cmd"], "first");
        let first_call_id = first_event.item["call_id"].as_str().unwrap().to_owned();
        assert_eq!(
            broker
                .resume(&[(first_call_id.clone(), json!("result: first"))], &tools)
                .unwrap(),
            Some((token.clone(), request_id.into(), 1))
        );
        assert_eq!(
            first.join().unwrap().unwrap()["content"][0]["text"],
            "result: first"
        );

        let second_event = next_call_at(&broker, &token, 1);
        let second_arguments: Value =
            serde_json::from_str(second_event.item["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(second_arguments["cmd"], "second");
        let second_call_id = second_event.item["call_id"].as_str().unwrap().to_owned();
        assert_eq!(
            broker
                .resume(&[(second_call_id.clone(), json!("result: second"))], &tools)
                .unwrap(),
            Some((token.clone(), request_id.into(), 2))
        );
        assert_eq!(
            second.join().unwrap().unwrap()["content"][0]["text"],
            "result: second"
        );
        assert_eq!(broker.snapshot(request_id)["pending_tools"], 0);
        assert_eq!(broker.snapshot(request_id)["completed_tools"], 2);
        for (id, expected) in [
            (first_call_id, "result: first"),
            (second_call_id, "result: second"),
        ] {
            let request = json!({"turn_token":token,"call_id":id});
            assert_eq!(
                broker.poll_result(&request, || false).unwrap()["content"][0]["text"],
                expected
            );
        }
    }

    #[test]
    fn freeform_patch_and_multimodal_result_keep_their_native_payloads() {
        let broker = Arc::new(TurnBroker::default());
        let tools = registry();
        let token = broker
            .begin("patch-turn".into(), tools.clone(), ToolChoice::Auto)
            .unwrap();
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
    fn a_native_result_can_only_resume_its_original_thread() {
        let broker = Arc::new(TurnBroker::default());
        let tools = registry();
        let child = TurnIdentity::Thread("child-thread".into());
        let token = broker
            .begin_scoped(
                "child-request".into(),
                tools.clone(),
                TurnIdentity::Thread("child-thread".into()),
            )
            .unwrap();
        let pending = invocation(
            &broker,
            &token,
            "functions.exec_command",
            json!({"arguments":{"cmd":"child"}}),
        );
        let event = next_call(&broker, &token);
        let call_id = event.item["call_id"].as_str().unwrap().to_owned();
        let outputs = [(call_id.clone(), json!("child output"))];
        for foreign in [
            TurnIdentity::Thread("root-thread".into()),
            TurnIdentity::Thread("sibling-thread".into()),
            TurnIdentity::Anonymous,
            TurnIdentity::SetupSmokeTest,
        ] {
            assert!(broker.resume_scoped(&outputs, &tools, &foreign).is_err());
        }
        assert_eq!(
            broker.resume_scoped(&outputs, &tools, &child).unwrap(),
            Some((token, "child-request".into(), 1))
        );
        assert_eq!(
            pending.join().unwrap().unwrap()["content"][0]["text"],
            "child output"
        );
    }

    #[test]
    fn active_browser_limit_preserves_terminal_results_and_releases_capacity() {
        let broker = TurnBroker::default();
        let token = broker.begin("terminal".into(), Vec::new()).unwrap();
        {
            let mut state = broker.state.lock().unwrap();
            let turn = state.turns.get_mut(&token).unwrap();
            turn.result = Some(Ok("answer".into()));
            turn.text = "answer".into();
            // A connector may leave a recoverable result before HTTP retirement.
            turn.cache_result("call", &json!("native result"));
            turn.orphaned_at = Some(Instant::now());
        }
        let active = (0..MAX_ACTIVE_BROWSER_TURNS)
            .map(|index| broker.begin(format!("active-{index}"), Vec::new()).unwrap())
            .collect::<Vec<_>>();
        let overflow = broker.begin("overflow".into(), Vec::new()).unwrap_err();
        assert!(overflow.contains("at most 5 simultaneous browser turns"));
        assert_eq!(
            broker
                .next_event(&token, 0)
                .unwrap()
                .unwrap()
                .text_delta
                .as_deref(),
            Some("answer")
        );
        assert!(broker.next_event(&token, 0).unwrap().unwrap().end_turn);
        assert!(broker.retire_observer(&token, 0));
        assert!(broker.state.lock().unwrap().turns.contains_key(&token));

        {
            let mut state = broker.state.lock().unwrap();
            state.turns.get_mut(&active[0]).unwrap().result = Some(Ok("done".into()));
        }
        assert!(broker.retire_observer(&active[0], 0));
        assert!(broker.begin("next".into(), Vec::new()).is_ok());
        assert!(broker.state.lock().unwrap().turns.contains_key(&token));
    }

    #[test]
    fn concurrent_turns_are_isolated_and_cancellation_releases_mcp_waiters() {
        let broker = Arc::new(TurnBroker::default());
        let tools = registry();
        let parent = broker
            .begin("parent".into(), tools.clone(), ToolChoice::Auto)
            .unwrap();
        let child = broker
            .begin("child".into(), tools.clone(), ToolChoice::Auto)
            .unwrap();
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
        assert!(broker.retire_observer(&parent, 0));
        let pending = parent_call.join().unwrap().unwrap();
        assert_eq!(pending["structuredContent"]["status"], "pending");
        assert_eq!(pending["structuredContent"]["call_id"], parent_id);
        let inventory = broker.inventory(&json!({"turn_token":parent})).unwrap();
        assert_eq!(inventory["pending_calls"][0]["call_id"], parent_id);
        assert_eq!(
            broker
                .resume(&[(parent_id.clone(), json!("late output"))], &tools)
                .unwrap(),
            Some((parent.clone(), "parent".into(), 2))
        );
        assert!(
            broker
                .resume(&[(parent_id.clone(), json!("duplicate output"))], &tools)
                .is_err()
        );
        let result_request = json!({"turn_token":parent,"call_id":parent_id});
        assert_eq!(
            broker.poll_result(&result_request, || false).unwrap()["content"][0]["text"],
            "late output"
        );
        assert!(broker.poll_result(&result_request, || false).is_ok());
        assert!(broker.next_event(&child, 0).is_err());
        assert!(broker.next_event(&child, 1).unwrap().is_none());
    }

    #[test]
    fn five_parallel_agent_turns_keep_mcp_results_with_their_own_call_ids() {
        let broker = Arc::new(TurnBroker::default());
        let tools = registry();
        let turns = (0..5)
            .map(|index| {
                broker
                    .begin(format!("agent-{index}"), tools.clone())
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let calls = turns
            .iter()
            .enumerate()
            .map(|(index, token)| {
                invocation(
                    &broker,
                    token,
                    "functions.exec_command",
                    json!({"arguments":{"cmd":format!("agent-{index}")}}),
                )
            })
            .collect::<Vec<_>>();
        let call_ids = turns
            .iter()
            .map(|token| {
                next_call(&broker, token).item["call_id"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect::<Vec<_>>();

        // Return results in reverse order to ensure parallel agents cannot
        // receive another agent's output when completions arrive out of order.
        let mut observer_epochs = vec![0; turns.len()];
        for index in (0..turns.len()).rev() {
            let expected = format!("result-for-agent-{index}");
            let (resumed_token, resumed_request_id, observer_epoch) = broker
                .resume(&[(call_ids[index].clone(), json!(expected))], &tools)
                .unwrap()
                .expect("the child's original Responses turn should resume");
            assert_eq!(resumed_token, turns[index]);
            assert_eq!(resumed_request_id, format!("agent-{index}"));
            observer_epochs[index] = observer_epoch;
        }

        for (index, call) in calls.into_iter().enumerate() {
            let result = call.join().unwrap().unwrap();
            assert_eq!(
                result["content"][0]["text"],
                format!("result-for-agent-{index}")
            );
        }

        // Each child can make another tool round in the same turn. Interleave
        // those completions in a different order to catch stale call-id or
        // per-turn state leaking across parallel agents.
        let second_calls = turns
            .iter()
            .enumerate()
            .map(|(index, token)| {
                invocation(
                    &broker,
                    token,
                    "functions.exec_command",
                    json!({"arguments":{"cmd":format!("agent-{index}-second")}}),
                )
            })
            .collect::<Vec<_>>();
        let second_call_ids = turns
            .iter()
            .enumerate()
            .map(|(index, token)| {
                next_call_at(&broker, token, observer_epochs[index]).item["call_id"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(second_call_ids.len(), turns.len());
        assert!(
            call_ids
                .iter()
                .all(|first_id| !second_call_ids.contains(first_id))
        );

        for index in [2, 4, 0, 3, 1] {
            broker
                .resume(
                    &[(
                        second_call_ids[index].clone(),
                        json!(format!("second-result-for-agent-{index}")),
                    )],
                    &tools,
                )
                .unwrap();
        }
        for (index, call) in second_calls.into_iter().enumerate() {
            let result = call.join().unwrap().unwrap();
            assert_eq!(
                result["content"][0]["text"],
                format!("second-result-for-agent-{index}")
            );
        }
    }

    #[test]
    fn discovery_updates_the_turn_registry_and_rejects_unavailable_tools() {
        let broker = Arc::new(TurnBroker::default());
        let mut tools = registry();
        let token = broker
            .begin("search".into(), tools.clone(), ToolChoice::Auto)
            .unwrap();
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
    fn connector_inventory_traffic_is_visible_in_the_turn_snapshot() {
        let broker = TurnBroker::default();
        let token = broker.begin("inventory-only".into(), Vec::new()).unwrap();
        assert_eq!(broker.snapshot("inventory-only")["connector_requests"], 0);
        assert_eq!(broker.snapshot("inventory-only")["tool_invocations"], 0);

        broker
            .inventory(&json!({"turn_token":token}))
            .expect("valid connector inventory request");
        assert_eq!(broker.snapshot("inventory-only")["connector_requests"], 1);
        assert_eq!(broker.snapshot("inventory-only")["tool_invocations"], 0);

        assert!(broker.inventory(&json!({"turn_token":"expired"})).is_err());
        assert_eq!(broker.snapshot("inventory-only")["connector_requests"], 1);
    }

    #[test]
    fn turn_snapshot_distinguishes_inventory_from_mcp_tool_invocations() {
        let broker = Arc::new(TurnBroker::default());
        let tools = registry();
        let token = broker
            .begin("inventory-and-action".into(), tools.clone())
            .unwrap();
        broker
            .inventory(&json!({"turn_token":token}))
            .expect("valid connector discovery");
        assert_eq!(
            broker.snapshot("inventory-and-action")["tool_invocations"],
            0
        );

        let pending = invocation(
            &broker,
            &token,
            "functions.exec_command",
            json!({"arguments":{"cmd":"inspect"}}),
        );
        let event = next_call(&broker, &token);
        let call_id = event.item["call_id"].as_str().unwrap().to_owned();
        assert_eq!(
            broker.snapshot("inventory-and-action")["tool_invocations"],
            1
        );
        broker
            .resume(&[(call_id, json!("done"))], &tools)
            .expect("matching native result");
        pending.join().unwrap().unwrap();
        assert_eq!(
            broker.snapshot("inventory-and-action")["completed_tools"],
            1
        );
    }

    #[test]
    fn streamed_text_is_append_only_and_resumed_responses_start_at_the_new_segment() {
        let broker = Arc::new(TurnBroker::default());
        let tools = registry();
        let token = broker.begin("streamed-turn".into(), tools.clone()).unwrap();
        assert!(broker.append_text("streamed-turn", "before tool ".into()));
        let first_delta = broker.next_event(&token, 0).unwrap().unwrap();
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

        assert!(broker.append_text("streamed-turn", "after tool".into()));
        let second_delta = broker.next_event(&token, 1).unwrap().unwrap();
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
        let final_event = broker.next_event(&token, 1).unwrap().unwrap();
        assert_eq!(final_event.segment_text, "after tool");
        assert!(final_event.end_turn);
    }

    #[test]
    fn reasoning_headings_are_forwarded_as_summary_parts_and_not_replayed_after_resume() {
        let broker = Arc::new(TurnBroker::default());
        let tools = registry();
        let token = broker
            .begin("reasoning-turn".into(), tools.clone())
            .unwrap();
        assert!(broker.append_reasoning_heading("reasoning-turn", "Thought for 4 seconds".into()));
        let first = broker.next_event(&token, 0).unwrap().unwrap();
        assert_eq!(first.item["type"], "reasoning");
        assert_eq!(first.item["summary"], json!([]));
        let heading = first.reasoning_heading.unwrap();
        assert_eq!(heading.summary_index, 0);
        assert_eq!(heading.text, "**Thought for 4 seconds**\n\n<!-- -->");

        let pending = invocation(
            &broker,
            &token,
            "functions.exec_command",
            json!({"arguments":{"cmd":"inspect"}}),
        );
        let tool_call = next_call(&broker, &token);
        let call_id = tool_call.item["call_id"].as_str().unwrap().to_owned();
        assert!(
            broker.append_reasoning_heading("reasoning-turn", "Checking the tool result".into())
        );
        broker
            .resume(&[(call_id, json!("tool output"))], &tools)
            .unwrap();
        pending.join().unwrap().unwrap();

        let continued = broker.next_event(&token, 1).unwrap().unwrap();
        let heading = continued.reasoning_heading.unwrap();
        assert_eq!(heading.summary_index, 0);
        assert_eq!(heading.text, "**Checking the tool result**\n\n<!-- -->");
        assert_ne!(heading.item_id, first.item["id"].as_str().unwrap());
    }

    #[test]
    fn retirement_resolves_polled_queued_calls_without_claiming_dispatch() {
        let broker = TurnBroker::default();
        let token = broker.begin("queued-cancel".into(), registry()).unwrap();
        let call_id = "call_queued";
        {
            let mut state = broker.state.lock().unwrap();
            let turn = state.turns.get_mut(&token).unwrap();
            turn.queue.push_back(call_id.into());
            turn.calls.insert(
                call_id.into(),
                Invocation {
                    wire_name: "functions.exec_command".into(),
                    item: json!({"type":"function_call","call_id":call_id}),
                    output: None,
                    delivered: false,
                    created_at: Instant::now(),
                    expired: false,
                    last_poll: Some(Instant::now()),
                },
            );
        }
        assert!(broker.retire_observer(&token, 0));
        let request = json!({"turn_token":token,"call_id":call_id});
        let result = broker.poll_result(&request, || false).unwrap();
        assert_eq!(result["isError"], true);
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("No operation was run")
        );
        assert_eq!(broker.poll_result(&request, || false).unwrap(), result);
        assert_eq!(broker.snapshot("queued-cancel")["pending_tools"], 0);
    }

    #[test]
    fn expired_queued_call_has_a_recoverable_definitive_result() {
        let broker = TurnBroker::default();
        let token = broker.begin("queued-expiry".into(), registry()).unwrap();
        let call_id = "call_expired";
        {
            let mut state = broker.state.lock().unwrap();
            let turn = state.turns.get_mut(&token).unwrap();
            let now = Instant::now();
            turn.queue.push_back(call_id.into());
            turn.calls.insert(
                call_id.into(),
                Invocation {
                    wire_name: "functions.exec_command".into(),
                    item: json!({"type":"function_call","call_id":call_id}),
                    output: None,
                    delivered: false,
                    created_at: now,
                    expired: false,
                    last_poll: Some(now),
                },
            );
            turn.expire_stale_calls(now + MCP_CALL_RESULT_TTL);
        }
        let request = json!({"turn_token":token,"call_id":call_id});
        let result = broker.poll_result(&request, || false).unwrap();
        assert_eq!(result["isError"], true);
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("No operation was run")
        );
        assert!(broker.next_event(&token, 0).unwrap().is_none());
        assert_eq!(broker.snapshot("queued-expiry")["pending_tools"], 0);
    }

    #[test]
    fn transport_cancellation_stops_the_turn_without_executing_anything() {
        let broker = TurnBroker::default();
        let token = broker
            .begin("cancel".into(), registry(), ToolChoice::Auto)
            .unwrap();
        assert!(broker.invoke(&json!({"turn_token":token,"wire_name":"functions.exec_command","arguments":{"cmd":"never executed"}}), || true).is_err());
        assert!(broker.next_event(&token, 0).is_err());
        assert_eq!(broker.snapshot("cancel")["active"], false);
    }
}
