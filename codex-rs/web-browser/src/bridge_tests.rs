use super::*;
use serde_json::json;

#[test]
fn accepts_web_extended_effort_values() {
    for effort in ["xhigh", "max"] {
        let body = serde_json::to_vec(&json!({
            "model": "Web picker choice", "input": "Hello",
            "reasoning": {"effort": effort}
        }))
        .unwrap();
        let parsed = parse_text_only_responses_request(&body).unwrap();
        assert_eq!(parsed.reasoning_effort.as_deref(), Some(effort));
    }
}

#[test]
fn background_probe_preserves_other_models_levels_for_the_same_picker() {
    let path = std::env::temp_dir().join(format!("web2codex-capabilities-{}.json", Uuid::new_v4()));
    let snapshot = |reasoning_by_model| AccountCapabilities {
        probe_version: 4,
        picker_options: vec!["GPT-6".into(), "GPT-5.6".into()],
        reasoning_by_model,
        detected_at_unix_seconds: 0,
    };
    store_account_capabilities(
        &path,
        snapshot(BTreeMap::from([
            ("GPT-6".into(), vec!["medium".into()]),
            ("GPT-5.6".into(), vec!["high".into()]),
        ])),
        false,
    )
    .unwrap();
    store_account_capabilities(
        &path,
        snapshot(BTreeMap::from([("GPT-6".into(), vec!["low".into()])])),
        true,
    )
    .unwrap();
    let stored: AccountCapabilities =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    std::fs::remove_file(path).unwrap();
    assert_eq!(stored.reasoning_by_model["GPT-6"], ["low"]);
    assert_eq!(stored.reasoning_by_model["GPT-5.6"], ["high"]);
}

#[test]
fn detected_web_models_accept_image_and_audio_input() {
    let snapshot = std::env::temp_dir().join(format!("web2codex-models-{}.json", Uuid::new_v4()));
    std::fs::write(
        &snapshot,
        serde_json::to_vec(&json!({
            "probe_version": 4,
            "picker_options": ["GPT-6", "Other Web Model"],
            "reasoning_by_model": {"GPT-6": ["low", "medium", "xhigh", "max"]},
            "detected_at_unix_seconds": 0
        }))
        .unwrap(),
    )
    .unwrap();
    let catalog = model_catalog_from_picker_snapshot(&snapshot);
    std::fs::remove_file(snapshot).unwrap();
    assert_eq!(
        catalog["models"][0]["input_modalities"],
        json!(["text", "image", "audio"])
    );
    assert_eq!(
        catalog["models"][0]["include_skills_usage_instructions"],
        true
    );
    assert!(
        catalog["models"][0]["base_instructions"]
            .as_str()
            .unwrap()
            .contains("never return a bare JSON response")
    );
    assert_eq!(catalog["models"][0]["default_reasoning_level"], "medium");
    assert_eq!(catalog["models"][0]["context_window"], 200_000);
    assert_eq!(catalog["models"][0]["max_context_window"], 400_000);
    assert_eq!(
        catalog["models"][0]["effective_context_window_percent"],
        100
    );
    assert_eq!(
        catalog["models"][0]["supported_reasoning_levels"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert_eq!(
        catalog["models"][1]["supported_reasoning_levels"],
        json!([])
    );
    assert!(catalog["models"][1]["default_reasoning_level"].is_null());
}

#[test]
fn inline_user_images_are_attached_and_not_inserted_as_base64_prompt_text() {
    let parsed = parse_text_only_responses_request(
        &serde_json::to_vec(&json!({
            "model": "Web picker choice",
            "input": [{"type":"message", "role":"user", "content":[
                {"type":"input_text", "text":"Look at this"},
                {"type":"input_image", "image_url":"data:image/png;base64,aGVsbG8="}
            ]}]
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(parsed.attachments.len(), 1);
    assert_eq!(parsed.attachments[0].name, "web2codex-image-1.png");
    assert_eq!(parsed.attachments[0].mime, "image/png");
    assert_eq!(
        parsed.attachments[0].data_url,
        "data:image/png;base64,aGVsbG8="
    );
    assert!(
        parsed
            .prompt
            .contains("[Attached image: web2codex-image-1.png]")
    );
    assert!(!parsed.prompt.contains("aGVsbG8="));
}

#[test]
fn inline_user_audio_is_attached_and_not_inserted_as_base64_prompt_text() {
    let parsed = parse_text_only_responses_request(
        &serde_json::to_vec(&json!({
            "model": "Web picker choice",
            "input": [{"type":"message", "role":"user", "content":[
                {"type":"input_text", "text":"Transcribe this"},
                {"type":"input_audio", "audio_url":"data:audio/wav;base64,aGVsbG8="}
            ]}]
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(parsed.attachments.len(), 1);
    assert_eq!(parsed.attachments[0].name, "web2codex-audio-1.wav");
    assert_eq!(parsed.attachments[0].mime, "audio/wav");
    assert_eq!(
        parsed.attachments[0].data_url,
        "data:audio/wav;base64,aGVsbG8="
    );
    assert!(
        parsed
            .prompt
            .contains("[Attached audio: web2codex-audio-1.wav]")
    );
    assert!(!parsed.prompt.contains("aGVsbG8="));
}

#[test]
fn external_audio_is_rejected_instead_of_silently_omitted() {
    let request = serde_json::to_vec(&json!({
        "model":"Web picker choice",
        "input":[{"type":"message", "role":"user", "content":[
            {"type":"input_audio", "audio_url":"https://example.com/audio.wav"}
        ]}]
    }))
    .unwrap();
    assert!(matches!(
        parse_text_only_responses_request(&request),
        Err((_, "input_audio_unsupported", _))
    ));
}

#[test]
fn external_images_are_rejected_instead_of_silently_omitted() {
    let request = serde_json::to_vec(&json!({
        "model":"Web picker choice",
        "input":[{"type":"message", "role":"user", "content":[
            {"type":"input_image", "image_url":"https://example.com/image.png"}
        ]}]
    }))
    .unwrap();
    assert!(matches!(
        parse_text_only_responses_request(&request),
        Err((_, "input_image_unsupported", _))
    ));
}

#[test]
fn schemas_are_kept_in_the_connector_catalog_and_never_added_to_the_prompt() {
    let parsed = parse_text_only_responses_request(&serde_json::to_vec(&json!({
        "model":"Web picker choice", "input":"Edit the project",
        "instructions":"Follow the workspace instructions",
        "tools":[{"type":"function","name":"exec_command", "description":"SCHEMA_MUST_NOT_LEAK", "parameters":{"type":"object","properties":{"cmd":{"type":"string"}}}}]
    })).unwrap()).unwrap();
    assert!(parsed.prompt.contains("Edit the project"));
    assert!(!parsed.prompt.contains("SCHEMA_MUST_NOT_LEAK"));
    assert!(!parsed.prompt.contains("exec_command"));
    assert!(!parsed.prompt.contains("tool-call"));
    assert_eq!(parsed.tools.len(), 1);
}

#[test]
fn skill_catalog_and_explicit_skill_request_reach_the_web_chat() {
    let parsed = parse_text_only_responses_request(&serde_json::to_vec(&json!({
        "model":"Web picker choice",
        "instructions":"<skills_instructions>Use the workspace skill at /skills/example/SKILL.md.</skills_instructions>",
        "input":[{"type":"message","role":"user","content":[
            {"type":"input_text","text":"Use $example to update this project"}
        ]}],
        "tools":[{"type":"function","name":"exec_command","description":"TOOL_SCHEMA_MARKER","parameters":{"type":"object"}}]
    })).unwrap()).unwrap();
    assert!(parsed.prompt.contains("/skills/example/SKILL.md"));
    assert!(parsed.prompt.contains("Use $example"));
    assert!(!parsed.prompt.contains("TOOL_SCHEMA_MARKER"));
    assert_eq!(parsed.tools.len(), 1);
}

#[test]
fn native_outputs_preserve_context_and_resume_the_connector() {
    let parsed = parse_text_only_responses_request(&serde_json::to_vec(&json!({
        "model":"Web picker choice", "input":[
            {"type":"function_call","call_id":"native-call","name":"exec_command","arguments":"{}"},
            {"type":"function_call_output","call_id":"native-call","output":"PRIVATE_TOOL_RESULT"}
        ]
    })).unwrap()).unwrap();
    assert_eq!(parsed.prompt, "Earlier tool result:\nPRIVATE_TOOL_RESULT");
    assert!(parsed.expects_continuation);
    assert_eq!(
        parsed.outputs,
        vec![("native-call".into(), json!("PRIVATE_TOOL_RESULT"))]
    );
}

#[test]
fn a_new_user_message_after_historical_tools_begins_a_new_temporary_chat() {
    let parsed = parse_text_only_responses_request(
        &serde_json::to_vec(&json!({
            "model":"Web picker choice", "input":[
                {"type":"function_call_output","call_id":"old-call","output":"old result"},
                {"type":"message","role":"user","content":"Next task"}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(!parsed.expects_continuation);
    assert_eq!(
        parsed.prompt,
        "Earlier tool result:\nold result\n\nUser:\nNext task"
    );
}

#[test]
fn new_temporary_chat_preserves_images_from_previous_tool_results() {
    let result = json!({"type":"function_call_output","call_id":"old-call","output":[
        {"type":"input_text","text":"Screenshot"},
        {"type":"input_image","image_url":"data:image/png;base64,aGVsbG8="}
    ]});
    let request =
        |input| serde_json::to_vec(&json!({"model":"Web picker choice", "input":input})).unwrap();
    let fresh = parse_text_only_responses_request(&request(json!([
        result.clone(), {"type":"message","role":"user","content":"Describe it"}
    ])))
    .unwrap();
    assert_eq!(fresh.attachments.len(), 1);
    assert!(
        fresh
            .prompt
            .contains("[Attached image: web2codex-image-1.png]")
    );
    let continuation = parse_text_only_responses_request(&request(json!([result]))).unwrap();
    assert!(continuation.expects_continuation);
    assert!(continuation.attachments.is_empty());
}

#[test]
fn new_temporary_chat_preserves_structured_mcp_results_and_inline_images() {
    let result = json!({
        "type":"function_call_output",
        "call_id":"mcp-call",
        "output": {
            "content":[
                {"type":"text","text":"MCP_TEXT_RESULT"},
                {"type":"image","mimeType":"image/png","data":"aGVsbG8="},
                {"type":"audio","mimeType":"audio/wav","data":"YXVkaW8="}
            ],
            "structuredContent":{"path":"src/main.rs"},
            "isError":false
        }
    });
    let parsed = parse_text_only_responses_request(
        &serde_json::to_vec(&json!({
            "model":"Web picker choice",
            "input":[result, {"type":"message","role":"user","content":"Continue using that result"}]
        }))
        .unwrap(),
    )
    .unwrap();

    assert!(parsed.prompt.contains("MCP_TEXT_RESULT"));
    assert!(parsed.prompt.contains(r#"{"path":"src/main.rs"}"#));
    assert!(
        parsed
            .prompt
            .contains("[Attached image: web2codex-image-1.png]")
    );
    assert_eq!(parsed.attachments.len(), 2);
    assert_eq!(
        parsed.attachments[0].data_url,
        "data:image/png;base64,aGVsbG8="
    );
    assert_eq!(
        parsed.attachments[1].data_url,
        "data:audio/wav;base64,YXVkaW8="
    );
    assert!(
        parsed
            .prompt
            .contains("[Attached audio: web2codex-audio-2.wav]")
    );
}

#[test]
fn internal_failure_marker_is_not_in_the_next_chat_prompt() {
    let parsed = parse_text_only_responses_request(
        &serde_json::to_vec(&json!({
            "model":"Web picker choice", "input":[
                {"type":"function_call_output","call_id":"failed","output":[
                    {"type":"input_text","text":"web2codex://tool-result/failed/v1"},
                    {"type":"input_text","text":"External MCP failed"}
                ]},
                {"type":"message","role":"user","content":"Try again"}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(parsed.prompt.contains("External MCP failed"));
    assert!(!parsed.prompt.contains("web2codex://tool-result/failed/v1"));
}

#[test]
fn provider_search_results_extend_the_catalog_without_leaking_into_the_prompt() {
    let parsed = parse_text_only_responses_request(&serde_json::to_vec(&json!({
        "model":"Web picker choice", "input":[
            {"type":"tool_search_output","call_id":"search-call","status":"completed","execution":"client","tools":[
                {"type":"namespace","name":"custom_mcp","tools":[{"type":"function","name":"read_file","description":"found via native search","parameters":{"type":"object"}}]}
            ]}
        ]
    })).unwrap()).unwrap();
    assert!(parsed.expects_continuation);
    assert_eq!(parsed.tools.len(), 1);
    assert_eq!(parsed.tools[0].namespace, "custom_mcp");
    assert_eq!(parsed.outputs[0].0, "search-call");
    assert!(parsed.prompt.is_empty());
}
