//! Local authenticated Responses transport. A tool result resumes the original
//! ChatGPT response; only a new user turn creates a new Temporary Chat.

use std::io::Write;
use std::net::TcpStream;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tao::event_loop::EventLoopProxy;
use uuid::Uuid;

use crate::turn_broker::{NativeEvent, ReasoningHeading, TurnBroker, TurnIdentity};
use crate::{
    BrowserEvent, INFERENCE_TIMEOUT, ParsedHttpRequest, client_connection_closed,
    completed_response_object, json_error, parse_text_only_responses_request,
    response_item_for_added, write_json_response, write_sse_error, write_sse_event,
    write_sse_header,
};

struct Observer<'a> {
    token: String,
    epoch: u64,
    request_id: String,
    broker: &'a TurnBroker,
    dispatch: &'a dyn Fn(BrowserEvent) -> bool,
    keep_turn: bool,
    cancel_browser: bool,
    retired: bool,
}

#[derive(Default)]
struct ReasoningStreamState {
    item_id: Option<String>,
    output_index: Option<usize>,
    summary: Vec<String>,
}

impl ReasoningStreamState {
    fn append(&mut self, heading: &ReasoningHeading, message_started: bool) {
        if self.item_id.is_none() {
            self.item_id = Some(heading.item_id.clone());
            self.output_index = Some(message_started as usize);
        }
        self.summary.push(heading.text.clone());
    }

    fn message_output_index(&self) -> usize {
        (self.output_index == Some(0)) as usize
    }

    fn output_item(&self) -> Option<Value> {
        let id = self.item_id.as_ref()?;
        Some(json!({
            "id": id,
            "type": "reasoning",
            "status": "completed",
            "summary": self.summary.iter().map(|text| json!({"type":"summary_text", "text":text})).collect::<Vec<_>>(),
            "content": [],
            "encrypted_content": null
        }))
    }
}

impl Observer<'_> {
    fn retire(&mut self) {
        if self.retired || self.keep_turn {
            return;
        }
        self.retired = true;
        if self.broker.retire_observer(&self.token, self.epoch) {
            if self.cancel_browser {
                (self.dispatch)(BrowserEvent::CancelResponses(self.request_id.clone()));
            }
            (self.dispatch)(BrowserEvent::ReleaseResponses(self.request_id.clone()));
        }
    }
}

impl Drop for Observer<'_> {
    fn drop(&mut self) {
        self.retire();
    }
}

fn json_content_type(request: &ParsedHttpRequest) -> bool {
    request.content_type.as_deref().is_some_and(|value| {
        value
            .split(';')
            .next()
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
    })
}

pub(super) fn handle_connector(
    stream: &mut TcpStream,
    request: &ParsedHttpRequest,
    broker: &TurnBroker,
) {
    if !json_content_type(request) {
        let _ = write_json_response(
            stream,
            "415 Unsupported Media Type",
            &json_error(
                "invalid_request_error",
                "unsupported_content_type",
                "Content-Type must be application/json",
            ),
        );
        return;
    }
    let body: Value = match serde_json::from_slice(&request.body) {
        Ok(value) => value,
        Err(_) => {
            let _ = write_json_response(
                stream,
                "400 Bad Request",
                &json_error(
                    "invalid_request_error",
                    "invalid_json",
                    "Invalid connector request",
                ),
            );
            return;
        }
    };
    let result = match request.path.as_str() {
        "/bridge/tools" => broker.inventory(&body),
        "/bridge/tool-result" => broker.poll_result(&body, || client_connection_closed(stream)),
        "/bridge/invoke" => broker.invoke(&body, || client_connection_closed(stream)),
        _ => return,
    };
    match result {
        Ok(value) => {
            let _ = write_json_response(stream, "200 OK", &value);
        }
        Err(message) => {
            let _ = write_json_response(
                stream,
                "409 Conflict",
                &json_error("bridge_error", "connector_request_failed", &message),
            );
        }
    }
}

pub(super) fn handle(
    stream: &mut TcpStream,
    request: &ParsedHttpRequest,
    event_proxy: &EventLoopProxy<BrowserEvent>,
    broker: &Arc<TurnBroker>,
) {
    handle_with_dispatch(
        stream,
        request,
        &|event| event_proxy.send_event(event).is_ok(),
        broker,
    );
}

fn handle_with_dispatch(
    stream: &mut TcpStream,
    request: &ParsedHttpRequest,
    dispatch: &dyn Fn(BrowserEvent) -> bool,
    broker: &TurnBroker,
) {
    if !json_content_type(request) {
        let _ = write_json_response(
            stream,
            "415 Unsupported Media Type",
            &json_error(
                "invalid_request_error",
                "unsupported_content_type",
                "Content-Type must be application/json",
            ),
        );
        return;
    }
    let parsed = match parse_text_only_responses_request(&request.body) {
        Ok(parsed) => parsed,
        Err((status, code, message)) => {
            let _ = write_json_response(
                stream,
                status,
                &json_error("invalid_request_error", code, &message),
            );
            return;
        }
    };
    if client_connection_closed(stream) {
        return;
    }
    let identity = if let Some(agent) = &parsed.agent {
        TurnIdentity::Thread(agent.thread_id.clone())
    } else if parsed.setup_smoke_test {
        TurnIdentity::SetupSmokeTest
    } else if let Some(thread_id) = &parsed.conversation_id {
        TurnIdentity::Thread(thread_id.clone())
    } else {
        TurnIdentity::Anonymous
    };
    let resumed = match broker.resume_scoped(&parsed.outputs, &parsed.tools, &identity) {
        Ok(resumed) => resumed,
        Err(message) => {
            let _ = write_json_response(
                stream,
                "409 Conflict",
                &json_error("bridge_error", "turn_resume_failed", &message),
            );
            return;
        }
    };
    let (token, request_id, epoch, fresh) = match resumed {
        Some((token, request_id, epoch)) => (token, request_id, epoch, false),
        None if parsed.expects_continuation => {
            let _ = write_json_response(
                stream,
                "409 Conflict",
                &json_error(
                    "bridge_error",
                    "turn_expired",
                    "The original ChatGPT turn is no longer active; its tool result was not resent into a new chat",
                ),
            );
            return;
        }
        None => {
            let request_id = Uuid::new_v4().to_string();
            match broker.begin_scoped(
                request_id.clone(),
                parsed.tools,
                parsed.connector_tool_choice,
                identity,
            ) {
                Ok(token) => (token, request_id, 0, true),
                Err(message) => {
                    let _ = write_json_response(
                        stream,
                        "429 Too Many Requests",
                        &json_error("bridge_error", "too_many_active_turns", &message),
                    );
                    return;
                }
            }
        }
    };
    let mut observer = Observer {
        token,
        epoch,
        request_id,
        broker,
        dispatch,
        keep_turn: false,
        cancel_browser: true,
        retired: false,
    };
    if fresh {
        // Only connector selection and a turn capability are added to the text.
        // Tool definitions are available exclusively through the MCP connector.
        let wrap_prompt = |content: &str| {
            if !parsed.compaction_stages.is_empty() {
                return content.to_owned();
            }
            if parsed.connector_required {
                format!(
                    "{} turn_token: {}. Copy it unchanged in connector calls.\n\n{}",
                    parsed.tool_choice.instructions(),
                    observer.token,
                    content
                )
            } else {
                format!(
                    "{}\n\n{}",
                    parsed.tool_choice.instructions(),
                    content
                )
            }
        };
        let prompt = wrap_prompt(&parsed.prompt);
        let resume_prompt = wrap_prompt(&parsed.resume_prompt);
        let stages = parsed.compaction_stages.iter().map(|stage| {
            json!({"prompt":stage.prompt,"acknowledgement":stage.acknowledgement})
        }).collect::<Vec<_>>();
        if !dispatch(BrowserEvent::ResponsesRequest {
            request_id: observer.request_id.clone(),
            conversation_id: parsed.conversation_id.clone(),
            conversation_epoch: parsed.conversation_epoch.clone(),
            agent: parsed.agent.clone(),
            setup_smoke_test: parsed.setup_smoke_test,
            model: parsed.model.clone(),
            reasoning_effort: parsed.reasoning_effort.clone(),
            connector_required: parsed.connector_required,
            mcp_required: parsed.mcp_required,
            mcp_action_required: parsed.mcp_action_required,
            tool_work_expected: parsed.tool_work_expected,
            prompt,
            stages,
            resume_prompt,
            attachments: parsed.attachments.clone(),
            resume_attachments: parsed.resume_attachments.clone(),
        }) {
            let _ = write_json_response(
                stream,
                "503 Service Unavailable",
                &json_error(
                    "server_error",
                    "browser_stopped",
                    "ChatGPT Web background process stopped",
                ),
            );
            return;
        }
    }
    let response_id = format!("resp_{}", Uuid::new_v4().simple());
    let message_item_id = format!("msg_{}", Uuid::new_v4().simple());
    let mut message_started = false;
    let mut streamed_text = String::new();
    let mut reasoning = ReasoningStreamState::default();
    if parsed.stream {
        if write_sse_header(stream).is_err() {
            return;
        }
        if write_sse_event(stream, &json!({"type":"response.created", "response":{"id": response_id, "object":"response", "status":"in_progress", "model":parsed.model, "output":[]}})).is_err() { return; }
    }
    let deadline = Instant::now() + INFERENCE_TIMEOUT;
    let mut last_heartbeat = Instant::now();
    loop {
        if client_connection_closed(stream) {
            return;
        }
        match broker.next_event(&observer.token, observer.epoch) {
            Ok(Some(event)) => {
                if let Some(heading) = event.reasoning_heading.as_ref() {
                    reasoning.append(heading, message_started);
                    if parsed.stream
                        && write_reasoning_heading(stream, &mut reasoning, heading).is_err()
                    {
                        return;
                    }
                    continue;
                }
                if let Some(delta) = event.text_delta.as_deref() {
                    if parsed.stream {
                        if write_text_delta(
                            stream,
                            &message_item_id,
                            delta,
                            reasoning.message_output_index(),
                            &mut message_started,
                            &mut streamed_text,
                        )
                        .is_err()
                        {
                            return;
                        }
                    } else if !delta.is_empty() {
                        // The first observed text fixes the output order for
                        // both streamed and non-streamed Responses.
                        message_started = true;
                    }
                    continue;
                }
                let end_turn = event.end_turn;
                if let Err(message) = parsed.tool_choice.check_output(&event.item, end_turn) {
                    observer.retire();
                    if parsed.stream {
                        let _ = write_sse_error(
                            stream,
                            &response_id,
                            "tool_choice_unsatisfied",
                            message,
                        );
                        let _ = finish_sse(stream);
                    } else {
                        let _ = write_json_response(
                            stream,
                            "502 Bad Gateway",
                            &json_error("bridge_error", "tool_choice_unsatisfied", message),
                        );
                    }
                    return;
                }
                if end_turn {
                    // Queue release before exposing terminal completion to the client.
                    // A new request can arrive immediately after that completion.
                    observer.cancel_browser = false;
                    observer.retire();
                }
                let result = if parsed.stream {
                    write_completion(
                        stream,
                        &response_id,
                        &parsed.model,
                        &event,
                        &message_item_id,
                        &mut reasoning,
                        &mut message_started,
                        &mut streamed_text,
                    )
                } else {
                    write_json_response(
                        stream,
                        "200 OK",
                        &completed_response_object(
                            &response_id,
                            &parsed.model,
                            response_output_items(&event, &message_item_id, &reasoning),
                            event.end_turn,
                        ),
                    )
                };
                // A fully delivered native call transfers observation to Codex's
                // next request. A disconnect/cancel retires the browser and MCP waiters.
                observer.keep_turn = result.is_ok() && !end_turn;
                observer.cancel_browser = !result.is_ok();
                return;
            }
            Err(message) => {
                observer.retire();
                if parsed.stream {
                    let _ = write_sse_error(
                        stream,
                        &response_id,
                        "chatgpt_web_request_failed",
                        &message,
                    );
                    let _ = finish_sse(stream);
                } else {
                    let _ = write_json_response(
                        stream,
                        "502 Bad Gateway",
                        &json_error("server_error", "chatgpt_web_request_failed", &message),
                    );
                }
                return;
            }
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            observer.retire();
            if parsed.stream {
                let _ = write_sse_error(
                    stream,
                    &response_id,
                    "chatgpt_web_timeout",
                    "ChatGPT did not settle this request; it was cancelled without retry",
                );
                let _ = finish_sse(stream);
            } else {
                let _ = write_json_response(
                    stream,
                    "504 Gateway Timeout",
                    &json_error(
                        "server_error",
                        "chatgpt_web_timeout",
                        "ChatGPT did not settle this request; it was cancelled without retry",
                    ),
                );
            }
            return;
        }
        if parsed.stream && last_heartbeat.elapsed() >= Duration::from_secs(10) {
            // The Responses client resets its idle deadline only after a full
            // SSE data event; comment heartbeats are discarded by EventSource.
            // Send an ignorable Responses event so long subagent/tool waits keep
            // the stream alive until ChatGPT produces its final answer.
            if write_sse_event(stream, &json!({"type":"codex.response.keepalive"})).is_err() {
                return;
            }
            last_heartbeat = Instant::now();
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn write_completion(
    stream: &mut TcpStream,
    response_id: &str,
    model: &str,
    event: &NativeEvent,
    message_item_id: &str,
    reasoning: &ReasoningStreamState,
    message_started: &mut bool,
    streamed_text: &mut String,
) -> std::io::Result<()> {
    let output_items = response_output_items(event, message_item_id, reasoning);
    for (output_index, item) in output_items.iter().enumerate() {
        if item["type"] == "reasoning" {
            let item_id = item["id"].as_str().unwrap_or_default();
            for (summary_index, summary) in
                item["summary"].as_array().into_iter().flatten().enumerate()
            {
                let text = summary["text"].as_str().unwrap_or_default();
                write_sse_event(
                    stream,
                    &json!({"type":"response.reasoning_summary_text.done", "item_id":item_id, "output_index":output_index, "summary_index":summary_index, "text":text}),
                )?;
                write_sse_event(
                    stream,
                    &json!({"type":"response.reasoning_summary_part.done", "item_id":item_id, "output_index":output_index, "summary_index":summary_index, "part":{"type":"summary_text", "text":text}}),
                )?;
            }
        } else if item["type"] == "message" {
            if !*message_started {
                write_sse_event(
                    stream,
                    &json!({"type":"response.output_item.added", "output_index":output_index, "item":response_item_for_added(item)}),
                )?;
                write_sse_event(
                    stream,
                    &json!({"type":"response.content_part.added", "item_id":item["id"], "output_index":output_index, "content_index":0, "part":{"type":"output_text", "text":"", "annotations":[]}}),
                )?;
                *message_started = true;
            }
            let text = item
                .pointer("/content/0/text")
                .and_then(Value::as_str)
                .unwrap_or("");
            let Some(suffix) = text.strip_prefix(streamed_text.as_str()) else {
                let _ = write_sse_error(
                    stream,
                    response_id,
                    "chatgpt_stream_revised",
                    "ChatGPT changed text after it was sent to the client",
                );
                let _ = finish_sse(stream);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "ChatGPT revised streamed response text",
                ));
            };
            if !suffix.is_empty() {
                write_text_delta(
                    stream,
                    message_item_id,
                    suffix,
                    output_index,
                    message_started,
                    streamed_text,
                )?;
            }
            write_sse_event(
                stream,
                &json!({"type":"response.output_text.done", "item_id":item["id"], "output_index":output_index, "content_index":0, "text":text}),
            )?;
            write_sse_event(
                stream,
                &json!({"type":"response.content_part.done", "item_id":item["id"], "output_index":output_index, "content_index":0, "part":item["content"][0]}),
            )?;
        } else {
            write_sse_event(
                stream,
                &json!({"type":"response.output_item.added", "output_index":output_index, "item":response_item_for_added(item)}),
            )?;
        }
        write_sse_event(
            stream,
            &json!({"type":"response.output_item.done", "output_index":output_index, "item":item}),
        )?;
    }
    write_sse_event(
        stream,
        &json!({"type":"response.completed", "response":completed_response_object(response_id, model, output_items, event.end_turn)}),
    )?;
    finish_sse(stream)
}

fn write_text_delta(
    stream: &mut TcpStream,
    message_item_id: &str,
    delta: &str,
    output_index: usize,
    message_started: &mut bool,
    streamed_text: &mut String,
) -> std::io::Result<()> {
    if delta.is_empty() {
        return Ok(());
    }
    if !*message_started {
        let item = message_item(message_item_id, "");
        write_sse_event(
            stream,
            &json!({"type":"response.output_item.added", "output_index":output_index, "item":response_item_for_added(&item)}),
        )?;
        write_sse_event(
            stream,
            &json!({"type":"response.content_part.added", "item_id":message_item_id, "output_index":output_index, "content_index":0, "part":{"type":"output_text", "text":"", "annotations":[]}}),
        )?;
        *message_started = true;
    }
    write_sse_event(
        stream,
        &json!({"type":"response.output_text.delta", "item_id":message_item_id, "output_index":output_index, "content_index":0, "delta":delta}),
    )?;
    streamed_text.push_str(delta);
    Ok(())
}

fn message_item(message_item_id: &str, text: &str) -> Value {
    json!({
        "id": message_item_id,
        "type": "message",
        "status": "completed",
        "role": "assistant",
        "content": [{ "type": "output_text", "text": text, "annotations": [] }]
    })
}

fn write_reasoning_heading(
    stream: &mut TcpStream,
    reasoning: &ReasoningStreamState,
    heading: &ReasoningHeading,
) -> std::io::Result<()> {
    let output_index = reasoning.output_index.unwrap_or(0);
    if heading.summary_index == 0 {
        let item = json!({
            "id": heading.item_id,
            "type": "reasoning",
            "summary": [],
            "content": [],
            "encrypted_content": null,
            "status": "in_progress"
        });
        write_sse_event(
            stream,
            &json!({"type":"response.output_item.added", "output_index":output_index, "item":item}),
        )?;
    } else {
        write_sse_event(
            stream,
            &json!({"type":"response.reasoning_summary_part.added", "item_id":heading.item_id, "output_index":output_index, "summary_index":heading.summary_index}),
        )?;
    }
    write_sse_event(
        stream,
        &json!({"type":"response.reasoning_summary_text.delta", "item_id":heading.item_id, "output_index":output_index, "summary_index":heading.summary_index, "delta":heading.text}),
    )
}

fn response_output_items(
    event: &NativeEvent,
    message_item_id: &str,
    reasoning: &ReasoningStreamState,
) -> Vec<Value> {
    let has_message = event.item["type"] == "message" || !event.segment_text.is_empty();
    let mut indexed = Vec::with_capacity(3);
    if let Some(item) = reasoning.output_item() {
        indexed.push((reasoning.output_index.unwrap_or(0), item));
    }
    if has_message {
        indexed.push((
            reasoning.message_output_index(),
            message_item(message_item_id, &event.segment_text),
        ));
    }
    if !event.item.is_null() && event.item["type"] != "message" {
        indexed.push((indexed.len(), event.item.clone()));
    }
    indexed.sort_by_key(|(index, _)| *index);
    indexed.into_iter().map(|(_, item)| item).collect()
}

fn finish_sse(stream: &mut TcpStream) -> std::io::Result<()> {
    stream.write_all(b"0\r\n\r\n")?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::{
        ReasoningStreamState, finish_sse, response_output_items, write_completion, write_sse_error,
        write_sse_header, write_text_delta,
    };
    use crate::turn_broker::{NativeEvent, ReasoningHeading};
    use serde_json::json;
    use std::io::Read;
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    #[test]
    fn parallel_agent_http_streams_complete_and_release_independently() {
        use super::handle_with_dispatch;
        use crate::{BrowserEvent, ParsedHttpRequest, turn_broker::TurnBroker};
        use std::collections::{HashMap, HashSet};
        use std::net::Shutdown;
        use std::sync::{Arc, mpsc};
        use std::time::{Duration, Instant};

        let broker = Arc::new(TurnBroker::default());
        let (sender, receiver) = mpsc::channel();
        // The actual HTTP handler receives overlapping parent and child turns,
        // with only the platform-specific WebView event dispatcher replaced.
        for round in [
            vec!["parent", "architecture", "purpose", "stream"],
            vec!["parent"],
        ] {
            let mut workers = Vec::new();
            for name in &round {
                let name = *name;
                let mut metadata = json!({"thread_id":name});
                if name != "parent" {
                    metadata["x-openai-subagent"] = json!("collab_spawn");
                    metadata["x-codex-turn-metadata"] =
                        json!(json!({"agent_name":format!("/root/{name}")}).to_string());
                }
                let request = ParsedHttpRequest {
                    method: "POST".into(),
                    path: "/v1/responses".into(),
                    authorization: String::new(),
                    content_type: Some("application/json".into()),
                    body: serde_json::to_vec(&json!({
                        "model":"test-model", "input":format!("request-{name}"),
                        "stream":true, "client_metadata":metadata,
                    }))
                    .unwrap(),
                };
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
                client
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                client.shutdown(Shutdown::Write).unwrap();
                let (mut server, _) = listener.accept().unwrap();
                let broker = Arc::clone(&broker);
                let sender = sender.clone();
                workers.push((
                    name,
                    client,
                    thread::spawn(move || {
                        handle_with_dispatch(
                            &mut server,
                            &request,
                            &|event| sender.send(event).is_ok(),
                            &broker,
                        );
                    }),
                ));
            }

            let mut requests = HashMap::new();
            for _ in &round {
                let event = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
                let BrowserEvent::ResponsesRequest {
                    request_id,
                    conversation_id,
                    agent,
                    ..
                } = event
                else {
                    panic!("expected independent browser request");
                };
                let name = conversation_id.unwrap();
                assert!(round.contains(&name.as_str()));
                if name == "parent" {
                    assert!(agent.is_none());
                } else {
                    assert_eq!(agent.unwrap().thread_id, name);
                }
                assert!(requests.insert(name, request_id).is_none());
            }
            for name in &round {
                assert!(broker.append_text(&requests[*name], format!("reply-{name}")));
            }
            // Complete in reverse order while every observer remains active.
            let deadline = Instant::now() + Duration::from_secs(8);
            for name in round.iter().rev() {
                let id = &requests[*name];
                let revision = broker.snapshot(id)["revision"].as_u64().unwrap();
                while !broker.complete(id, Some(revision), Ok(format!("reply-{name}-done"))) {
                    assert!(Instant::now() < deadline, "turn did not settle: {name}");
                    thread::sleep(Duration::from_millis(20));
                }
            }
            let mut released = HashSet::new();
            for _ in &round {
                let BrowserEvent::ReleaseResponses(id) =
                    receiver.recv_timeout(Duration::from_secs(5)).unwrap()
                else {
                    panic!("successful completion must release the browser without cancelling it");
                };
                assert!(released.insert(id));
            }
            assert_eq!(released, requests.into_values().collect());
            for (name, mut client, worker) in workers {
                let mut output = String::new();
                client.read_to_string(&mut output).unwrap();
                worker.join().unwrap();
                assert!(output.ends_with("0\r\n\r\n"));
                let events: Vec<serde_json::Value> = output
                    .lines()
                    .filter_map(|line| line.strip_prefix("data: "))
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect();
                let deltas: String = events
                    .iter()
                    .filter(|event| event["type"] == "response.output_text.delta")
                    .map(|event| event["delta"].as_str().unwrap())
                    .collect();
                assert_eq!(deltas, format!("reply-{name}-done"));
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| event["type"] == "response.completed")
                        .count(),
                    1
                );
                assert_eq!(
                    events.last().unwrap()["response"]["output"][0]["content"][0]["text"],
                    format!("reply-{name}-done")
                );
            }
            assert!(receiver.try_recv().is_err());
        }
    }

    #[test]
    fn short_streamed_reply_is_completed_exactly_once() {
        let reply = "Понял))";
        for prefix in ["", "Понял", reply] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let reader = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut output = String::new();
                stream.read_to_string(&mut output).unwrap();
                output
            });
            let mut stream = TcpStream::connect(address).unwrap();
            write_sse_header(&mut stream).unwrap();
            let mut message_started = false;
            let mut streamed_text = String::new();
            let mut reasoning = ReasoningStreamState::default();
            write_text_delta(
                &mut stream,
                "msg_short",
                prefix,
                0,
                &mut message_started,
                &mut streamed_text,
            )
            .unwrap();
            let event = NativeEvent {
                item: json!({"type":"message"}),
                end_turn: true,
                text_delta: None,
                reasoning_heading: None,
                segment_text: reply.into(),
            };
            write_completion(
                &mut stream,
                "resp_short",
                "test-model",
                &event,
                "msg_short",
                &mut reasoning,
                &mut message_started,
                &mut streamed_text,
            )
            .unwrap();
            drop(stream);

            let output = reader.join().unwrap();
            assert!(output.ends_with("0\r\n\r\n"));
            let events: Vec<serde_json::Value> = output
                .lines()
                .filter_map(|line| line.strip_prefix("data: "))
                .map(|data| serde_json::from_str(data).unwrap())
                .collect();
            let deltas: String = events
                .iter()
                .filter(|event| event["type"] == "response.output_text.delta")
                .map(|event| event["delta"].as_str().unwrap())
                .collect();
            assert_eq!(deltas, reply);
            assert_eq!(streamed_text, reply);
            for event_type in [
                "response.output_item.added",
                "response.output_text.done",
                "response.output_item.done",
                "response.completed",
            ] {
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| event["type"] == event_type)
                        .count(),
                    1,
                    "prefix {prefix:?}, event {event_type}"
                );
            }
            let completed = events.last().unwrap();
            assert_eq!(completed["type"], "response.completed");
            assert_eq!(
                completed["response"]["output"][0]["content"][0]["text"],
                reply
            );
        }
    }

    #[test]
    fn reasoning_summary_is_a_native_item_and_keeps_event_output_order() {
        let heading = ReasoningHeading {
            item_id: "rs_visible_heading".into(),
            summary_index: 0,
            text: "**Thought for 4 seconds**\n\n<!-- -->".into(),
        };
        let mut reasoning = ReasoningStreamState::default();
        reasoning.append(&heading, false);
        let event = NativeEvent {
            item: json!({"type":"message"}),
            end_turn: true,
            text_delta: None,
            reasoning_heading: None,
            segment_text: "Answer".into(),
        };

        let output = response_output_items(&event, "msg_answer", &reasoning);
        assert_eq!(output[0]["type"], "reasoning");
        assert_eq!(output[0]["summary"][0]["text"], heading.text);
        assert_eq!(output[1]["type"], "message");
    }

    #[test]
    fn reasoning_heading_arriving_after_text_uses_a_distinct_output_index() {
        let heading = ReasoningHeading {
            item_id: "rs_late_heading".into(),
            summary_index: 0,
            text: "**Planning the response**\n\n<!-- -->".into(),
        };
        let mut reasoning = ReasoningStreamState::default();
        reasoning.append(&heading, true);
        assert_eq!(reasoning.output_index, Some(1));
        assert_eq!(reasoning.message_output_index(), 0);
    }

    #[test]
    fn streamed_bridge_errors_are_failed_events_not_text_output() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let reader = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).unwrap();
            bytes
        });

        let mut stream = TcpStream::connect(address).unwrap();
        write_sse_header(&mut stream).unwrap();
        write_sse_error(
            &mut stream,
            "resp_test_error",
            "chatgpt_web_request_failed",
            "ChatGPT changed previously streamed text",
        )
        .unwrap();
        finish_sse(&mut stream).unwrap();
        drop(stream);

        let output = String::from_utf8(reader.join().unwrap()).unwrap();
        assert!(output.contains("event: response.failed\n"));
        assert!(output.contains("\"type\":\"response.failed\""));
        assert!(output.contains("\"code\":\"chatgpt_web_request_failed\""));
        assert!(output.contains("\"message\":\"ChatGPT changed previously streamed text\""));
        assert!(!output.contains("\"type\":\"message\""));
        assert!(!output.contains("\"output_text\""));
        assert!(!output.contains("\"delta\""));
    }
}
