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

use crate::turn_broker::{NativeEvent, TurnBroker};
use crate::{
    BrowserEvent, INFERENCE_TIMEOUT, ParsedHttpRequest, client_connection_closed,
    completed_response_object, json_error, parse_text_only_responses_request,
    response_item_for_added, write_json_response, write_sse_error, write_sse_event,
    write_sse_header,
};

struct Observer<'a> {
    token: String,
    request_id: String,
    broker: &'a TurnBroker,
    event_proxy: &'a EventLoopProxy<BrowserEvent>,
    keep_turn: bool,
}

impl Drop for Observer<'_> {
    fn drop(&mut self) {
        if !self.keep_turn {
            self.broker.retire(&self.token);
            let _ = self
                .event_proxy
                .send_event(BrowserEvent::ReleaseResponses(self.request_id.clone()));
        }
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
    let result = if request.path == "/bridge/tools" {
        broker.inventory(&body)
    } else {
        broker.invoke(&body, || client_connection_closed(stream))
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
                &json_error("invalid_request_error", code, message),
            );
            return;
        }
    };
    if client_connection_closed(stream) {
        return;
    }
    let resumed = match broker.resume(&parsed.outputs, &parsed.tools) {
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
    let (token, request_id, fresh) = match resumed {
        Some((token, request_id)) => (token, request_id, false),
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
            match broker.begin(request_id.clone(), parsed.tools) {
                Ok(token) => (token, request_id, true),
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
        request_id,
        broker,
        event_proxy,
        keep_turn: false,
    };
    if fresh {
        // Only connector selection and a turn capability are added to the text.
        // Tool definitions are available exclusively through the MCP connector.
        let prompt = format!(
            "You are continuing a Web2Codex coding-agent conversation. Use the selected Web2Codex connector when tools are needed. Current harness turn_token: {}. Copy it unchanged in connector calls. Return user-facing answers in ordinary prose.\n\n{}",
            observer.token, parsed.prompt
        );
        if event_proxy
            .send_event(BrowserEvent::ResponsesRequest {
                request_id: observer.request_id.clone(),
                model: parsed.model.clone(),
                prompt,
            })
            .is_err()
        {
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
        match broker.next_event(&observer.token) {
            Ok(Some(event)) => {
                let end_turn = event.end_turn;
                let result = if parsed.stream {
                    write_completion(stream, &response_id, &parsed.model, event)
                } else {
                    write_json_response(
                        stream,
                        "200 OK",
                        &completed_response_object(
                            &response_id,
                            &parsed.model,
                            event.item,
                            event.end_turn,
                        ),
                    )
                };
                // A fully delivered native call transfers observation to Codex's
                // next request. A disconnect/cancel retires the browser and MCP waiters.
                observer.keep_turn = result.is_ok() && !end_turn;
                return;
            }
            Err(message) => {
                if parsed.stream {
                    let _ = write_sse_error(stream, "chatgpt_web_request_failed", &message);
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
            if parsed.stream {
                let _ = write_sse_error(
                    stream,
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
            if stream
                .write_all(b"E\r\n: keep-alive\n\n\r\n")
                .and_then(|()| stream.flush())
                .is_err()
            {
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
    event: NativeEvent,
) -> std::io::Result<()> {
    let item = event.item;
    write_sse_event(
        stream,
        &json!({"type":"response.output_item.added", "output_index":0, "item": response_item_for_added(&item)}),
    )?;
    if let Some(text) = item.pointer("/content/0/text").and_then(Value::as_str) {
        write_sse_event(
            stream,
            &json!({"type":"response.content_part.added", "item_id":item["id"], "output_index":0, "content_index":0, "part":{"type":"output_text", "text":"", "annotations":[]}}),
        )?;
        write_sse_event(
            stream,
            &json!({"type":"response.output_text.delta", "item_id":item["id"], "output_index":0, "content_index":0, "delta":text}),
        )?;
        write_sse_event(
            stream,
            &json!({"type":"response.output_text.done", "item_id":item["id"], "output_index":0, "content_index":0, "text":text}),
        )?;
        write_sse_event(
            stream,
            &json!({"type":"response.content_part.done", "item_id":item["id"], "output_index":0, "content_index":0, "part":item["content"][0]}),
        )?;
    }
    write_sse_event(
        stream,
        &json!({"type":"response.output_item.done", "output_index":0, "item":item}),
    )?;
    write_sse_event(
        stream,
        &json!({"type":"response.completed", "response":completed_response_object(response_id, model, item, event.end_turn)}),
    )?;
    finish_sse(stream)
}

fn finish_sse(stream: &mut TcpStream) -> std::io::Result<()> {
    stream.write_all(b"0\r\n\r\n")?;
    stream.flush()
}
