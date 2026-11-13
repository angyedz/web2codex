use super::*;
use serde_json::json;

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
fn native_outputs_resume_the_connector_without_becoming_a_new_web_prompt() {
    let parsed = parse_text_only_responses_request(&serde_json::to_vec(&json!({
        "model":"Web picker choice", "input":[
            {"type":"function_call","call_id":"native-call","name":"exec_command","arguments":"{}"},
            {"type":"function_call_output","call_id":"native-call","output":"PRIVATE_TOOL_RESULT"}
        ]
    })).unwrap()).unwrap();
    assert!(parsed.prompt.is_empty());
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
    assert_eq!(parsed.prompt, "User:\nNext task");
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
