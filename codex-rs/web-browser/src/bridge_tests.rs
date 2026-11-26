use super::*;
use serde_json::json;

#[test]
fn capability_probe_is_injected_only_for_the_chatgpt_origin() {
    assert!(should_probe_chatgpt_url(
        "https://chatgpt.com/?temporary-chat=true"
    ));
    assert!(should_probe_chatgpt_url("https://chatgpt.com/c/123"));
    assert!(!should_probe_chatgpt_url(
        "https://chatgpt.com.example.org/"
    ));
    assert!(!should_probe_chatgpt_url("http://chatgpt.com/"));
}

#[test]
fn native_power_slider_focus_requires_its_own_visible_foreground_view() {
    assert!(native_power_slider_focus_allowed(true, Some("agent-a"), "agent-a", true));
    assert!(!native_power_slider_focus_allowed(true, Some("agent-b"), "agent-a", true));
    assert!(!native_power_slider_focus_allowed(true, Some("agent-a"), "agent-a", false));
    assert!(!native_power_slider_focus_allowed(true, None, "agent-a", true));
    assert!(!native_power_slider_focus_allowed(false, Some("agent-a"), "agent-a", true));
}

#[test]
fn power_slider_ipc_preserves_legacy_default_and_honors_focus_opt_out() {
    assert!(power_slider_focus_requested(&json!({})));
    assert!(power_slider_focus_requested(&json!({"allow_focus": true})));
    assert!(!power_slider_focus_requested(&json!({"allow_focus": false})));
    assert!(!power_slider_focus_requested(&json!({"allow_focus": "true"})));
    assert!(!power_slider_focus_requested(&json!({"allow_focus": null})));
}

#[test]
fn background_browser_work_never_activates_or_focuses_the_window() {
    assert!(should_activate_browser(BrowserActivationIntent::UserAction));
    assert!(!should_activate_browser(
        BrowserActivationIntent::BackgroundWork
    ));
    assert!(!should_activate_browser(
        BrowserActivationIntent::BackgroundTabSelection
    ));
    assert!(should_apply_background_focus_guard(false));
    assert!(!should_apply_background_focus_guard(true));
    let current_style = 0x0004_0000 | 0x0008_0000;
    let guarded_style = extended_style_with_no_activate(current_style, true);
    assert_eq!(guarded_style, current_style | WS_EX_NOACTIVATE_STYLE);
    assert_eq!(
        extended_style_with_no_activate(guarded_style, false),
        current_style
    );
}

#[test]
fn parallel_background_requests_keep_the_focus_guard_until_the_last_one_finishes() {
    let mut guarded = HashSet::from(["agent-a".to_owned(), "agent-b".to_owned()]);
    assert!(!release_focus_guard_request("agent-a", &mut guarded));
    assert_eq!(guarded, HashSet::from(["agent-b".to_owned()]));
    assert!(!release_focus_guard_request("unknown", &mut guarded));
    assert!(release_focus_guard_request("agent-b", &mut guarded));
    assert!(guarded.is_empty());
}

#[test]
fn background_response_tabs_preserve_the_selected_tab() {
    assert!(!should_select_response_tab(
        BrowserActivationIntent::BackgroundWork,
        0,
        1
    ));
    assert!(should_select_response_tab(
        BrowserActivationIntent::BackgroundWork,
        1,
        1
    ));
    assert!(should_select_response_tab(
        BrowserActivationIntent::UserAction,
        0,
        1
    ));
    assert!(should_select_response_tab(
        BrowserActivationIntent::BackgroundTabSelection,
        0,
        1
    ));

    assert!(!response_tab_starts_in_viewport(
        BrowserActivationIntent::BackgroundWork,
        0,
        1
    ));
    assert!(response_tab_starts_in_viewport(
        BrowserActivationIntent::BackgroundWork,
        1,
        1
    ));
    assert!(response_tab_starts_in_viewport(
        BrowserActivationIntent::UserAction,
        0,
        1
    ));
}

#[test]
fn web2codex_lease_disconnect_starts_shutdown_grace() {
    let now = Instant::now();
    assert_eq!(
        session_disconnect_deadline(now, 0, true),
        Some(now + SESSION_DISCONNECT_GRACE)
    );
    assert_eq!(session_disconnect_deadline(now, 1, true), None);
    assert_eq!(session_disconnect_deadline(now, 0, false), None);
}

#[test]
fn client_write_half_close_keeps_response_stream_available() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut server, _) = listener.accept().unwrap();
    server.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    client.set_read_timeout(Some(Duration::from_secs(2))).unwrap();

    client.shutdown(std::net::Shutdown::Write).unwrap();
    assert_eq!(server.peek(&mut [0_u8; 1]).unwrap(), 0);
    assert!(!client_connection_closed(&server));

    server.write_all(b"response").unwrap();
    let mut response = [0_u8; 8];
    client.read_exact(&mut response).unwrap();
    assert_eq!(&response, b"response");
}

#[test]
fn primary_response_tab_is_visible_reused_and_closed_without_losing_request_state() {
    let mut tabs = AgentTabRegistry::default();
    tabs.bind_primary_response_request("root-request", 2);
    assert_eq!(tabs.tab_index_for_request("root-request"), Some(2));
    assert_eq!(tabs.reusable_primary_response_tab(), None);

    assert_eq!(tabs.release_request("root-request"), Vec::<usize>::new());
    assert_eq!(tabs.tab_index_for_request("root-request"), None);
    assert_eq!(tabs.reusable_primary_response_tab(), Some(2));

    tabs.bind_primary_response_request("next-root-request", 2);
    assert_eq!(tabs.reusable_primary_response_tab(), None);
    assert_eq!(tabs.close_tab(1), Vec::<String>::new());
    assert_eq!(tabs.tab_index_for_request("next-root-request"), Some(1));
    assert_eq!(tabs.close_tab(1), ["next-root-request"]);
    assert_eq!(tabs.tab_index_for_request("next-root-request"), None);
    assert_eq!(tabs.reusable_primary_response_tab(), None);
}

#[test]
fn primary_temporary_chat_is_retained_per_codex_thread_and_compaction_epoch() {
    let mut tabs = AgentTabRegistry::default();
    let first_binding = Arc::new(Mutex::new(Some("first-request".to_owned())));
    tabs.bind_response_request_id_state(2, first_binding.clone());
    tabs.bind_primary_thread_request("codex-thread", "first-request", 2, None);

    assert_eq!(tabs.primary_tab_for_thread("codex-thread").unwrap().tab_index, 2);
    assert_eq!(tabs.primary_thread_for_request("first-request"), Some("codex-thread"));
    assert_eq!(tabs.release_request("first-request"), Vec::<usize>::new());
    assert_eq!(tabs.primary_tab_for_thread("codex-thread").unwrap().active_request_id, None);
    assert_eq!(tabs.reusable_primary_response_tab(), None);
    assert_eq!(*first_binding.lock().unwrap(), None);

    let next_binding = Arc::new(Mutex::new(Some("next-request".to_owned())));
    tabs.bind_response_request_id_state(2, next_binding);
    tabs.bind_primary_thread_request("codex-thread", "next-request", 2, None);
    assert_eq!(tabs.tab_index_for_request("next-request"), Some(2));
    assert_eq!(tabs.primary_tab_for_thread("codex-thread").unwrap().conversation_epoch, None);

    tabs.release_request("next-request");
    tabs.bind_response_request_id_state(2, Arc::new(Mutex::new(Some("compacted-request".to_owned()))));
    tabs.bind_primary_thread_request(
        "codex-thread",
        "compacted-request",
        2,
        Some("12:checkpoint-2".to_owned()),
    );
    assert_eq!(
        tabs.primary_tab_for_thread("codex-thread").unwrap().conversation_epoch.as_deref(),
        Some("12:checkpoint-2")
    );
    assert_eq!(tabs.close_tab(2), ["compacted-request"]);
    assert_eq!(tabs.primary_tab_for_thread("codex-thread"), None);
}

#[test]
fn compaction_discards_the_staged_chat_before_the_next_codex_window() {
    let mut tabs = AgentTabRegistry::default();
    let binding = Arc::new(Mutex::new(Some("compact-request".to_owned())));
    tabs.bind_response_request_id_state(2, binding.clone());
    tabs.bind_primary_thread_request(
        "codex-thread",
        "compact-request",
        2,
        Some("compaction-window".to_owned()),
    );

    tabs.require_fresh_response_view("compact-request");

    assert_eq!(*binding.lock().unwrap(), None);
    assert!(tabs.response_request_id_state(2).is_none());
    assert_eq!(tabs.tab_index_for_request("compact-request"), Some(2));
}

#[test]
fn overlapping_primary_responses_use_separate_tabs_and_retire_the_old_one_later() {
    let mut tabs = AgentTabRegistry::default();
    tabs.bind_response_request_id_state(
        2,
        Arc::new(Mutex::new(Some("first-request".to_owned()))),
    );
    tabs.bind_primary_thread_request("codex-thread", "first-request", 2, None);

    // A newer request for an active Codex thread gets a fresh view instead of
    // colliding with the still-running ChatGPT response in the old view.
    tabs.bind_response_request_id_state(
        3,
        Arc::new(Mutex::new(Some("next-request".to_owned()))),
    );
    tabs.bind_primary_thread_request("codex-thread", "next-request", 3, None);
    assert_eq!(tabs.primary_tab_for_thread("codex-thread").unwrap().tab_index, 3);
    assert_eq!(tabs.tab_index_for_request("first-request"), Some(2));
    assert_eq!(tabs.tab_index_for_request("next-request"), Some(3));

    // Releasing the previous response closes only its retired tab.
    assert_eq!(tabs.release_request("first-request"), vec![2]);
    assert_eq!(tabs.primary_tab_for_thread("codex-thread").unwrap().tab_index, 3);
    assert_eq!(tabs.tab_index_for_request("next-request"), Some(3));
    assert!(tabs.release_request("next-request").is_empty());
}

#[test]
fn parallel_primary_responses_keep_one_tab_and_close_the_overflow() {
    let mut tabs = AgentTabRegistry::default();
    tabs.bind_primary_response_request("request-a", 2);
    tabs.bind_primary_response_request("request-b", 3);

    assert_eq!(tabs.release_request("request-a"), Vec::<usize>::new());
    assert_eq!(tabs.reusable_primary_response_tab(), None);
    assert_eq!(tabs.release_request("request-b"), vec![3]);
    assert_eq!(tabs.reusable_primary_response_tab(), Some(2));
}

#[test]
fn completed_primary_request_can_be_released_before_its_ui_release_event() {
    let broker = TurnBroker::default();
    let token = broker.begin("old-request".to_owned(), vec![]).unwrap();
    let mut tabs = AgentTabRegistry::default();
    let binding = Arc::new(Mutex::new(Some("old-request".to_owned())));
    tabs.bind_response_request_id_state(2, binding.clone());
    tabs.bind_primary_thread_request("codex-thread", "old-request", 2, None);

    assert_eq!(tabs.stale_response_request("new-request", Some("codex-thread"), None, false, &broker), None);
    assert!(broker.complete("old-request", None, Err("finished with an error".to_owned())));
    assert_eq!(tabs.stale_response_request("new-request", Some("other-thread"), None, false, &broker), None);
    assert_eq!(tabs.stale_response_request("new-request", Some("codex-thread"), None, false, &broker).as_deref(), Some("old-request"));

    assert!(tabs.release_request("old-request").is_empty());
    assert_eq!(*binding.lock().unwrap(), None);
    *binding.lock().unwrap() = Some("new-request".to_owned());
    tabs.bind_primary_thread_request("codex-thread", "new-request", 2, None);
    assert!(broker.retire_observer(&token, 0));
    assert!(tabs.release_request("old-request").is_empty());
    assert_eq!(tabs.tab_index_for_request("new-request"), Some(2));
    assert_eq!(binding.lock().unwrap().as_deref(), Some("new-request"));
}

#[test]
fn retired_agent_request_is_reconciled_without_closing_another_agents_tab() {
    let broker = TurnBroker::default();
    let token = broker.begin("old-agent-request".to_owned(), vec![]).unwrap();
    let agent = AgentRequestIdentity { thread_id: "agent-a".to_owned(), name: "A".to_owned(), agent_path: None };
    let other_agent = AgentRequestIdentity { thread_id: "agent-b".to_owned(), name: "B".to_owned(), agent_path: None };
    let mut tabs = AgentTabRegistry::default();
    tabs.bind_request(&agent, "old-agent-request", 1);
    tabs.bind_request(&other_agent, "other-request", 2);
    assert!(broker.retire_observer(&token, 0));
    assert_eq!(tabs.stale_response_request("new-agent-request", None, Some(&agent), false, &broker).as_deref(), Some("old-agent-request"));
    assert_eq!(tabs.request_close_for_request("old-agent-request"), Some(AgentTabCloseStatus::Deferred));
    assert_eq!(tabs.release_request("old-agent-request"), vec![1]);
    assert!(tabs.close_tab(1).is_empty());
    tabs.bind_request(&agent, "new-agent-request", 2);
    assert!(tabs.release_request("old-agent-request").is_empty());
    assert_eq!(tabs.tab_index_for_request("other-request"), Some(1));
    assert_eq!(tabs.tab_index_for_request("new-agent-request"), Some(2));
}

#[test]
fn anonymous_primary_tab_is_consumed_when_assigned_to_a_thread() {
    let mut tabs = AgentTabRegistry::default();
    tabs.bind_primary_response_request("anonymous", 2);
    tabs.release_request("anonymous");
    assert_eq!(tabs.reusable_primary_response_tab(), Some(2));

    tabs.bind_primary_thread_request("thread-a", "request-a", 2, None);
    tabs.release_request("request-a");
    assert_eq!(tabs.reusable_primary_response_tab(), None);
    tabs.bind_primary_thread_request("thread-b", "request-b", 3, None);
    tabs.release_request("request-b");
    assert_eq!(tabs.primary_tab_for_thread("thread-a").unwrap().tab_index, 2);
    assert_eq!(tabs.primary_tab_for_thread("thread-b").unwrap().tab_index, 3);
    assert_eq!(tabs.reusable_primary_response_tab(), None);
}

#[test]
fn tab_binding_replacement_and_close_invalidate_old_ipc_sources() {
    let mut tabs = AgentTabRegistry::default();
    let old = Arc::new(Mutex::new(Some("old".to_owned())));
    let new = Arc::new(Mutex::new(Some("new".to_owned())));
    tabs.bind_response_request_id_state(2, old.clone());
    tabs.bind_response_request_id_state(2, new.clone());
    assert_eq!(*old.lock().unwrap(), None);
    tabs.clear_response_request_id(2, "old");
    assert_eq!(new.lock().unwrap().as_deref(), Some("new"));
    tabs.bind_response_request_id_state(2, new.clone());
    assert_eq!(new.lock().unwrap().as_deref(), Some("new"));
    tabs.close_tab(2);
    assert_eq!(*new.lock().unwrap(), None);
}

#[test]
fn response_signals_and_rebind_rejections_keep_their_request_id() {
    let script = response_signal_script("old-request", "window.__web2codexCompleted=true;");
    assert_eq!(script, r#"if (window.__web2codexRequestId === "old-request") { window.__web2codexCompleted=true; }"#);
    let script = response_bind_script("new-request");
    assert!(script.contains(r#"const requestId = "new-request";"#));
    assert!(script.contains("await window.__web2codexBindRequest?.(requestId)"));
    assert!(script.contains("type: 'responses-bind-rejected', request_id: requestId"));
}

#[test]
fn only_collab_spawn_metadata_creates_agent_identity() {
    let root = json!({"thread_id":"root-thread", "agent_name":"Main"});
    assert_eq!(parse_agent_request_identity(Some(&root)), None);

    let other_subagent = json!({
        "thread_id":"helper-thread",
        "agent_name":"Reviewer",
        "x-openai-subagent":"review"
    });
    assert_eq!(parse_agent_request_identity(Some(&other_subagent)), None);

    let collab_spawn = json!({
        "thread_id":"child-thread-a",
        "agent_name":"Implementer",
        "x-openai-subagent":"collab_spawn"
    });
    assert_eq!(
        parse_agent_request_identity(Some(&collab_spawn)),
        Some(AgentRequestIdentity {
            thread_id: "child-thread-a".to_owned(),
            name: "Implementer".to_owned(),
            agent_path: None,
        })
    );
}

#[test]
fn thread_spawn_turn_metadata_recovers_agent_identity_without_legacy_header() {
    let metadata = json!({
        "thread_id":"child-thread-c",
        "x-codex-parent-thread-id":"parent-thread",
        "x-codex-turn-metadata":"{\"agent_name\":\"/root/architecture\",\"subagent_kind\":\"thread_spawn\"}"
    });

    assert_eq!(
        parse_agent_request_identity(Some(&metadata)),
        Some(AgentRequestIdentity {
            thread_id: "child-thread-c".to_owned(),
            name: "architecture".to_owned(),
            agent_path: Some("/root/architecture".to_owned()),
        })
    );
}

#[test]
fn nested_collab_metadata_routes_responses_to_the_child_thread() {
    let metadata = json!({
        "x-codex-parent-thread-id":"parent-thread",
        "x-codex-turn-metadata":json!({
            "thread_id":" child-thread-nested ",
            "agent_name":"/root/stream",
            "subagent_kind":"thread_spawn"
        }).to_string()
    });
    let parsed = parse_text_only_responses_request(
        &serde_json::to_vec(&json!({
            "model":"Web picker choice",
            "input":"Work on the task",
            "client_metadata":metadata.clone()
        })).unwrap(),
    ).unwrap();

    assert_eq!(parsed.conversation_id.as_deref(), Some("child-thread-nested"));
    assert_eq!(parsed.agent, Some(AgentRequestIdentity {
        thread_id: "child-thread-nested".to_owned(),
        name: "stream".to_owned(),
        agent_path: Some("/root/stream".to_owned()),
    }));

    let mut explicit = metadata.clone();
    explicit["thread_id"] = json!("explicit-child");
    assert_eq!(parse_agent_request_identity(Some(&explicit)).unwrap().thread_id, "explicit-child");
    explicit["thread_id"] = json!(" ");
    assert_eq!(parse_agent_request_identity(Some(&explicit)), None);

    for invalid in [json!(" "), json!("x".repeat(129)), json!(42)] {
        let invalid_metadata = json!({
            "x-openai-subagent":"collab_spawn",
            "x-codex-turn-metadata":json!({"thread_id":invalid}).to_string()
        });
        assert_eq!(parse_agent_request_identity(Some(&invalid_metadata)), None);
    }
}

#[test]
fn native_thread_spawn_metadata_routes_by_child_thread_id_without_legacy_headers() {
    let metadata = json!({
        "x-codex-turn-metadata": json!({
            "thread_id":"child-thread",
            "parent_thread_id":"parent-thread",
            "subagent_kind":"thread_spawn",
            "agent_name":"/root/architecture"
        }).to_string()
    });
    let parsed = parse_text_only_responses_request(
        &serde_json::to_vec(&json!({
            "model":"Web picker choice",
            "input":"Inspect the project",
            "client_metadata":metadata
        })).unwrap(),
    ).unwrap();

    assert_eq!(parsed.conversation_id.as_deref(), Some("child-thread"));
    assert_eq!(parsed.agent, Some(AgentRequestIdentity {
        thread_id: "child-thread".to_owned(),
        name: "architecture".to_owned(),
        agent_path: Some("/root/architecture".to_owned()),
    }));
}

#[test]
fn parent_thread_and_agent_path_are_sufficient_to_route_a_child_request() {
    let metadata = json!({
        "thread_id":"child-thread-d",
        "x-codex-parent-thread-id":"parent-thread",
        "x-codex-turn-metadata":"{\"agent_name\":\"/root/purpose\"}"
    });

    assert_eq!(
        parse_agent_request_identity(Some(&metadata))
            .and_then(|identity| identity.agent_path),
        Some("/root/purpose".to_owned())
    );
}

#[test]
fn collab_spawn_identity_is_preserved_from_responses_metadata() {
    let parsed = parse_text_only_responses_request(
        &serde_json::to_vec(&json!({
            "model":"Web picker choice",
            "input":"Work on the task",
            "client_metadata":{
                "thread_id":"child-thread-b",
                "x-codex-turn-metadata":"{\"agent_name\":\"/root/test-agent\"}",
                "x-openai-subagent":"collab_spawn"
            }
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        parsed.agent,
        Some(AgentRequestIdentity {
            thread_id: "child-thread-b".to_owned(),
            name: "test-agent".to_owned(),
            agent_path: Some("/root/test-agent".to_owned()),
        })
    );
    assert!(parsed.connector_required);
    assert!(parsed.mcp_required);
    assert!(parsed.mcp_action_required);
    assert!(parsed
        .prompt
        .contains("Use only the selected connector's documented tools to complete the assigned task"));
    assert!(parsed.prompt.contains("report only results verified by tool output"));
    assert!(!parsed.prompt.contains("wait for all final results"));
    assert!(parsed.prompt.len() < 2_000);
    assert!(parsed.tool_work_expected);
}

#[test]
fn resumed_codex_turn_sends_only_the_suffix_after_the_last_assistant_reply() {
    let parsed = parse_text_only_responses_request(
        &serde_json::to_vec(&json!({
            "model":"Web picker choice",
            "client_metadata":{"thread_id":"codex-thread"},
            "input":[
                {"type":"message","role":"user","content":"OLD_REQUEST"},
                {"type":"message","role":"assistant","content":"OLD_REPLY"},
                {"type":"message","role":"user","content":"NEW_REQUEST"}
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    assert_eq!(parsed.conversation_id.as_deref(), Some("codex-thread"));
    assert!(parsed.prompt.contains("OLD_REQUEST"));
    assert!(parsed.prompt.contains("NEW_REQUEST"));
    assert!(parsed.resume_prompt.contains("NEW_REQUEST"));
    assert!(!parsed.resume_prompt.contains("OLD_REQUEST"));
    assert!(!parsed.resume_prompt.contains("OLD_REPLY"));
}

#[test]
fn compaction_checkpoints_start_a_new_browser_conversation_epoch() {
    let parsed = parse_text_only_responses_request(
        &serde_json::to_vec(&json!({
            "model":"Web picker choice",
            "client_metadata":{"thread_id":"codex-thread"},
            "input":[
                {"type":"message","role":"user","content":"earlier"},
                {"type":"compaction","id":"checkpoint-2","encrypted_content":"opaque"},
                {"type":"message","role":"user","content":"summary and new request"}
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    assert_eq!(parsed.conversation_epoch.as_deref(), Some("1:checkpoint-2"));
}

#[test]
fn compaction_sends_only_essential_history_in_exactly_four_parts() {
    let parsed = parse_text_only_responses_request(
        &serde_json::to_vec(&json!({
            "model":"Web picker choice",
            "instructions":"LONG_BASE_INSTRUCTIONS_MUST_NOT_BE_SENT",
            "client_metadata":{"x-codex-turn-metadata":"{\"thread_id\":\"codex-thread\",\"request_kind\":\"compaction\"}"},
            "input":[
                {"type":"message","role":"system","content":"SYSTEM_PROMPT_MUST_NOT_BE_SENT"},
                {"type":"message","role":"developer","content":"DEVELOPER_PROMPT_MUST_NOT_BE_SENT"},
                {"type":"message","role":"user","content":"ACTIVE_GOAL_И_УСЛОВИЯ"},
                {"type":"message","role":"assistant","content":"COMPLETED_EVIDENCE"},
                {"type":"message","role":"user","content":"PENDING_WORK"},
                {"type":"context_compaction","id":"checkpoint-3","encrypted_content":"opaque"}
            ],
            "tools":[{"type":"function","name":"exec_command","parameters":{"type":"object"}}]
        }))
        .unwrap(),
    )
    .unwrap();

    assert_eq!(parsed.compaction_stages.len(), 3);
    assert!(parsed
        .conversation_epoch
        .as_deref()
        .is_some_and(|epoch| epoch.starts_with("compaction:")));
    assert!(parsed.prompt.starts_with("Context part 4/4:"));
    assert!(!parsed.prompt.contains("LONG_BASE_INSTRUCTIONS_MUST_NOT_BE_SENT"));
    assert!(!parsed.prompt.contains("SYSTEM_PROMPT_MUST_NOT_BE_SENT"));
    assert!(!parsed.prompt.contains("DEVELOPER_PROMPT_MUST_NOT_BE_SENT"));
    assert!(!parsed.connector_required);
    assert!(!parsed.mcp_required);

    let mut recovered = parsed
        .compaction_stages
        .iter()
        .map(|stage| {
            assert!(stage.prompt.contains("Store context part"));
            assert!(stage.prompt.contains(&stage.acknowledgement));
            stage
                .prompt
                .split_once("<part>\n")
                .and_then(|(_, content)| content.split_once("\n</part>"))
                .map(|(content, _)| content)
                .unwrap()
        })
        .collect::<Vec<_>>();
    recovered.push(
        parsed
            .prompt
            .split_once("<part>\n")
            .and_then(|(_, content)| content.split_once("\n</part>"))
            .map(|(content, _)| content)
            .unwrap(),
    );
    let recovered = recovered.concat();
    assert!(recovered.contains("ACTIVE_GOAL_И_УСЛОВИЯ"));
    assert!(recovered.contains("COMPLETED_EVIDENCE"));
    assert!(recovered.contains("PENDING_WORK"));
}

#[test]
fn compaction_split_preserves_unicode_and_all_source_text() {
    let source = "начало\n\nважная задача🙂\nсостояние\n\nожидающая работа";
    let parts = split_compaction_context_into_four(source);
    assert_eq!(parts.concat(), source);
    assert!(parts.iter().all(|part| !part.is_empty()));
}

#[test]
fn setup_smoke_metadata_is_explicit_and_does_not_create_agent_identity() {
    let turn_metadata = json!({
        "extra": {"web2codex_setup_smoke_test": "true"}
    })
    .to_string();
    let smoke_metadata = json!({"x-codex-turn-metadata": turn_metadata});
    assert!(is_setup_smoke_test_metadata(Some(&smoke_metadata)));
    assert_eq!(parse_agent_request_identity(Some(&smoke_metadata)), None);

    let parsed = parse_text_only_responses_request(
        &serde_json::to_vec(&json!({
            "model":"Web picker choice",
            "input":"Reply with exactly: READY TO WORK.",
            "client_metadata":smoke_metadata
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(parsed.setup_smoke_test);
    assert_eq!(parsed.agent, None);
    assert_eq!(
        parsed.prompt,
        format!(
            "Reply with exactly: READY TO WORK.\n\n{TUI_TEXT_RENDERING_INSTRUCTIONS}\n{MCP_CHAT_OUTPUT_INSTRUCTIONS}"
        )
    );

    assert!(!is_setup_smoke_test_metadata(Some(&json!({
        "thread_id":"root-thread",
        "agent_name":"Main"
    }))));
    assert!(!is_setup_smoke_test_metadata(Some(&json!({
        "x-codex-turn-metadata":"not-json"
    }))));
}

#[test]
fn setup_smoke_visible_tab_is_named_and_isolated_from_agent_views() {
    let mut tabs = AgentTabRegistry::default();
    let agent = AgentRequestIdentity {
        thread_id: "agent-for-smoke-test".to_owned(),
        name: "Builder".to_owned(),
        agent_path: None,
    };
    tabs.bind_setup_smoke_request("smoke-request", 1);
    tabs.bind_request(&agent, "agent-request", 2);

    assert_eq!(tabs.setup_smoke_tab_index(), Some(1));
    assert_eq!(tabs.tab_index_for_request("smoke-request"), Some(1));
    assert!(tabs.is_setup_smoke_request("smoke-request"));
    assert_eq!(tabs.label_at(1), Some("Setup · smoke test"));
    assert_eq!(tabs.thread_for_request("smoke-request"), None);
    assert_eq!(tabs.tab_index_for_request("agent-request"), Some(2));
    assert!(!tabs.is_setup_smoke_request("agent-request"));

    let _ = tabs.release_request("smoke-request");
    assert_eq!(tabs.tab_index_for_request("smoke-request"), None);
    assert_eq!(tabs.label_at(1), Some("Setup · smoke test"));
}

#[test]
fn parallel_agent_views_and_tool_continuations_stay_isolated() {
    let mut tabs = AgentTabRegistry::default();
    let first = AgentRequestIdentity {
        thread_id: "agent-a".to_owned(),
        name: "Builder".to_owned(),
        agent_path: None,
    };
    let second = AgentRequestIdentity {
        thread_id: "agent-b".to_owned(),
        name: "Reviewer".to_owned(),
        agent_path: None,
    };

    tabs.bind_request(&first, "request-a", 1);
    tabs.bind_request(&second, "request-b", 2);
    assert_eq!(tabs.tab_index("agent-a"), Some(1));
    assert_eq!(tabs.tab_index("agent-b"), Some(2));
    assert_eq!(tabs.thread_for_request("request-a"), Some("agent-a"));
    assert_eq!(tabs.thread_for_request("request-b"), Some("agent-b"));

    // Tool continuations reuse the same request ID, so they remain attached to
    // the original agent's browser tab rather than creating or crossing tabs.
    assert_eq!(tabs.thread_for_request("request-a"), Some("agent-a"));
    assert_eq!(tabs.thread_for_request("request-b"), Some("agent-b"));

    // A later model request from the same thread reuses its tab slot.
    let _ = tabs.release_request("request-a");
    tabs.bind_request(&first, "request-a-next", 1);
    assert_eq!(tabs.thread_for_request("request-a"), None);
    assert_eq!(tabs.thread_for_request("request-a-next"), Some("agent-a"));
    assert_eq!(tabs.tab_index("agent-a"), Some(1));

    let canceled = tabs.close_tab(1);
    assert_eq!(canceled, ["request-a-next"]);
    assert_eq!(tabs.tab_index("agent-a"), None);
    assert_eq!(tabs.tab_index("agent-b"), Some(1));
    assert_eq!(tabs.thread_for_request("request-b"), Some("agent-b"));
}

#[test]
fn completed_agent_tab_waits_for_its_active_request_before_closing() {
    let mut tabs = AgentTabRegistry::default();
    let agent = AgentRequestIdentity {
        thread_id: "finished-agent".to_owned(),
        name: "Builder".to_owned(),
        agent_path: None,
    };
    tabs.bind_request(&agent, "final-request", 2);

    assert_eq!(
        tabs.request_close_for_request("final-request"),
        Some(AgentTabCloseStatus::Deferred)
    );
    assert_eq!(tabs.release_request("final-request"), vec![2]);
    assert_eq!(tabs.close_tab(2), Vec::<String>::new());
    assert_eq!(tabs.tab_index("finished-agent"), None);
}

#[test]
fn terminal_agent_close_cancels_only_its_own_stale_browser_request() {
    let mut tabs = AgentTabRegistry::default();
    let agent = AgentRequestIdentity {
        thread_id: "terminal-agent".to_owned(),
        name: "Builder".to_owned(),
        agent_path: Some("/root/terminal-agent".to_owned()),
    };
    tabs.bind_request(&agent, "stale-response-request", 2);

    assert_eq!(
        tabs.request_close("terminal-agent", Some("/root/terminal-agent")),
        AgentTabCloseStatus::Ready(2)
    );
    assert_eq!(tabs.close_tab(2), ["stale-response-request"]);
    assert_eq!(tabs.tab_index("terminal-agent"), None);
    assert_eq!(tabs.thread_for_request("stale-response-request"), None);
}

#[test]
fn completed_agent_response_closes_its_tab_after_the_request_releases() {
    let mut tabs = AgentTabRegistry::default();
    let agent = AgentRequestIdentity {
        thread_id: "completed-agent".to_owned(),
        name: "Builder".to_owned(),
        agent_path: None,
    };
    tabs.bind_request(&agent, "completed-request", 2);

    assert_eq!(
        tabs.request_close_for_request("completed-request"),
        Some(AgentTabCloseStatus::Deferred)
    );
    assert_eq!(tabs.release_request("completed-request"), vec![2]);
    assert_eq!(tabs.close_tab(2), Vec::<String>::new());
    assert_eq!(tabs.tab_index("completed-agent"), None);
}

#[test]
fn release_before_response_completion_still_closes_the_agent_tab() {
    let mut tabs = AgentTabRegistry::default();
    let agent = AgentRequestIdentity {
        thread_id: "released-agent".to_owned(),
        name: "Builder".to_owned(),
        agent_path: None,
    };
    tabs.bind_request(&agent, "released-request", 2);

    // ReleaseResponses can win the race against ResponsesResult. The release
    // handler requests closure before dropping the request-to-agent mapping.
    assert_eq!(
        tabs.request_close_for_request("released-request"),
        Some(AgentTabCloseStatus::Deferred)
    );
    assert_eq!(tabs.release_request("released-request"), vec![2]);

    // A later ResponsesResult is harmless after the request was released.
    assert_eq!(tabs.request_close_for_request("released-request"), None);
    assert_eq!(tabs.close_tab(2), Vec::<String>::new());
    assert_eq!(tabs.tab_index("released-agent"), None);
}

#[test]
fn parallel_agent_failure_closes_only_its_own_tab_and_keeps_indices_valid() {
    let mut tabs = AgentTabRegistry::default();
    let first = AgentRequestIdentity {
        thread_id: "failed-agent-a".to_owned(),
        name: "Builder".to_owned(),
        agent_path: None,
    };
    let second = AgentRequestIdentity {
        thread_id: "running-agent-b".to_owned(),
        name: "Reviewer".to_owned(),
        agent_path: None,
    };
    tabs.bind_request(&first, "request-a", 1);
    tabs.bind_request(&second, "request-b", 2);

    assert_eq!(
        tabs.request_close_for_request("request-a"),
        Some(AgentTabCloseStatus::Deferred)
    );
    assert_eq!(tabs.tab_index("running-agent-b"), Some(2));
    assert_eq!(
        tabs.thread_for_request("request-b"),
        Some("running-agent-b")
    );

    assert_eq!(tabs.release_request("request-a"), vec![1]);
    assert_eq!(tabs.close_tab(1), Vec::<String>::new());
    assert_eq!(tabs.tab_index("failed-agent-a"), None);
    assert_eq!(tabs.tab_index("running-agent-b"), Some(1));
    assert_eq!(
        tabs.thread_for_request("request-b"),
        Some("running-agent-b")
    );
}

#[test]
fn five_parallel_agents_keep_their_tabs_when_one_agent_fails() {
    let mut tabs = AgentTabRegistry::default();
    let agents = (0..5)
        .map(|index| AgentRequestIdentity {
            thread_id: format!("agent-{index}"),
            name: format!("Agent {index}"),
            agent_path: None,
        })
        .collect::<Vec<_>>();

    for (index, agent) in agents.iter().enumerate() {
        tabs.bind_request(agent, &format!("request-{index}"), index + 1);
    }

    assert_eq!(
        tabs.request_close_for_request("request-2"),
        Some(AgentTabCloseStatus::Deferred)
    );
    assert_eq!(tabs.release_request("request-2"), vec![3]);
    assert_eq!(tabs.close_tab(3), Vec::<String>::new());

    for (index, agent) in agents.iter().enumerate() {
        if index == 2 {
            assert_eq!(tabs.tab_index(&agent.thread_id), None);
            assert_eq!(tabs.thread_for_request("request-2"), None);
            continue;
        }

        let expected_index = if index < 2 { index + 1 } else { index };
        assert_eq!(tabs.tab_index(&agent.thread_id), Some(expected_index));
        assert_eq!(
            tabs.thread_for_request(&format!("request-{index}")),
            Some(agent.thread_id.as_str())
        );
    }
}

#[test]
fn close_requested_before_agent_tab_registration_closes_it_when_idle() {
    let mut tabs = AgentTabRegistry::default();
    assert_eq!(
        tabs.request_close("agent-before-registration", None),
        AgentTabCloseStatus::Deferred
    );

    let agent = AgentRequestIdentity {
        thread_id: "agent-before-registration".to_owned(),
        name: "Builder".to_owned(),
        agent_path: None,
    };
    tabs.bind_request(&agent, "late-agent-request", 3);
    assert_eq!(tabs.tab_index("agent-before-registration"), Some(3));
    assert!(
        tabs.tab_for_thread("agent-before-registration")
            .is_some_and(|tab| tab.close_when_idle)
    );
    assert_eq!(tabs.release_request("late-agent-request"), vec![3]);
}

#[test]
fn completed_idle_agent_tab_closes_without_cancelling_other_agents() {
    let mut tabs = AgentTabRegistry::default();
    let finished = AgentRequestIdentity {
        thread_id: "finished-agent".to_owned(),
        name: "Builder".to_owned(),
        agent_path: None,
    };
    let running = AgentRequestIdentity {
        thread_id: "running-agent".to_owned(),
        name: "Reviewer".to_owned(),
        agent_path: None,
    };
    tabs.bind_request(&finished, "finished-request", 1);
    tabs.bind_request(&running, "running-request", 2);
    assert_eq!(
        tabs.release_request("finished-request"),
        Vec::<usize>::new()
    );

    assert_eq!(
        tabs.request_close("finished-agent", None),
        AgentTabCloseStatus::Ready(1)
    );
    assert_eq!(tabs.close_tab(1), Vec::<String>::new());
    assert_eq!(tabs.tab_index("finished-agent"), None);
    assert_eq!(tabs.tab_index("running-agent"), Some(1));
    assert_eq!(
        tabs.thread_for_request("running-request"),
        Some("running-agent")
    );
}

#[test]
fn completed_agent_tab_can_be_closed_by_agent_path_when_thread_ids_differ() {
    let mut tabs = AgentTabRegistry::default();
    let agent = AgentRequestIdentity {
        thread_id: "browser-thread-id".to_owned(),
        name: "reader".to_owned(),
        agent_path: Some("/root/reader".to_owned()),
    };
    tabs.bind_request(&agent, "reader-request", 2);
    assert_eq!(tabs.release_request("reader-request"), Vec::<usize>::new());

    assert_eq!(
        tabs.request_close("terminal-event-thread-id", Some("/root/reader")),
        AgentTabCloseStatus::Ready(2)
    );
    assert_eq!(tabs.close_tab(2), Vec::<String>::new());
    assert_eq!(tabs.tab_index("browser-thread-id"), None);
}

#[test]
fn close_by_agent_path_before_registration_closes_tab_when_response_releases() {
    let mut tabs = AgentTabRegistry::default();
    assert_eq!(
        tabs.request_close("terminal-event-thread-id", Some("/root/reader")),
        AgentTabCloseStatus::Deferred
    );

    let agent = AgentRequestIdentity {
        thread_id: "browser-thread-id".to_owned(),
        name: "reader".to_owned(),
        agent_path: Some("/root/reader".to_owned()),
    };
    tabs.bind_request(&agent, "late-reader-request", 3);
    assert!(
        tabs.tab_for_thread("browser-thread-id")
            .is_some_and(|tab| tab.close_when_idle)
    );
    assert_eq!(tabs.release_request("late-reader-request"), vec![3]);
}

#[test]
fn closing_a_tab_preserves_the_other_selected_tab_index() {
    assert_eq!(active_tab_after_close(3, 1), 2);
    assert_eq!(active_tab_after_close(1, 3), 1);
    assert_eq!(active_tab_after_close(2, 2), 0);
}

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
fn connector_is_required_only_when_native_tools_are_enabled() {
    let tool = json!({
        "type": "function",
        "name": "exec_command",
        "parameters": {"type": "object", "properties": {}}
    });
    let plain = parse_text_only_responses_request(
        &serde_json::to_vec(&json!({"model":"Web picker choice", "input":"Hello"})).unwrap(),
    )
    .unwrap();
    assert!(!plain.connector_required);
    assert!(!plain.tool_work_expected);

    let callable = parse_text_only_responses_request(
        &serde_json::to_vec(&json!({
            "model":"Web picker choice", "input":"Hello", "tools":[tool.clone()]
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(callable.connector_required);
    assert!(
        !callable.tool_work_expected,
        "an available tool alone should not disable ordinary response streaming"
    );

    let requested = parse_text_only_responses_request(
        &serde_json::to_vec(&json!({
            "model":"Web picker choice", "input":"Run tests in the repository", "tools":[tool.clone()]
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(requested.tool_work_expected);

    let project_request = parse_text_only_responses_request(
        &serde_json::to_vec(&json!({
            "model":"Web picker choice", "input":"Create a small Rust project in the test directory", "tools":[tool.clone()]
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(project_request.tool_work_expected);

    let disabled = parse_text_only_responses_request(
        &serde_json::to_vec(&json!({
            "model":"Web picker choice", "input":"Hello", "tools":[tool.clone()], "tool_choice":"none"
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(!disabled.connector_required);
}

#[test]
fn explicit_mcp_requests_require_the_connector_without_flagging_plain_questions() {
    let tool = json!({
        "type": "function",
        "name": "exec_command",
        "parameters": {"type": "object", "properties": {}}
    });
    let required = parse_text_only_responses_request(
        &serde_json::to_vec(&json!({
            "model":"Web picker choice",
            "input":"Use only the selected MCP connector to discover tools.",
            "tools":[tool.clone()]
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(required.mcp_required);
    assert!(required.connector_required);
    assert!(!required.mcp_action_required);
    assert!(required.tool_work_expected);

    let connector_only = parse_text_only_responses_request(
        &serde_json::to_vec(&json!({
            "model":"Web picker choice",
            "input":"Use only the selected MCP connector. Discover the available tools."
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(connector_only.mcp_required);
    assert!(connector_only.connector_required);
    assert!(!connector_only.mcp_action_required);
    assert!(connector_only.tool_work_expected);
    assert!(connector_only.prompt.contains("selected connector's documented tools"));

    let operation = parse_text_only_responses_request(
        &serde_json::to_vec(&json!({
            "model":"Web picker choice",
            "instructions":"Generic host guidance for tools",
            "input":"Use only the selected MCP connector: read the project file, start two parallel subagents, and compare their results.",
            "tools":[tool.clone()]
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(operation.mcp_required);
    assert!(operation.mcp_action_required);
    assert!(matches!(
        &operation.tool_choice,
        HarnessToolChoice::Required
    ));
    let routing_rules = operation
        .prompt
        .find("Use only the selected connector's documented tools")
        .expect("MCP routing rules must be present");
    let generic_guidance = operation
        .prompt
        .find("Generic host guidance for tools")
        .expect("host guidance must be preserved");
    let user_request = operation
        .prompt
        .find("Use only the selected MCP connector:")
        .expect("the original user request must be preserved");
    assert!(routing_rules < generic_guidance);
    assert!(generic_guidance < user_request);
    assert!(operation
        .prompt
        .contains("work in parallel, do not recurse, and wait for all final results"));
    assert!(operation.prompt.len() < 2_000);

    let child = parse_text_only_responses_request(
        &serde_json::to_vec(&json!({
            "model":"Web picker choice",
            "input":"Read the requested project file and report one verified constant.",
            "tools":[tool.clone()],
            "client_metadata":{
                "thread_id":"child-thread",
                "x-openai-subagent":"collab_spawn",
                "agent_name":"/root/inspect"
            }
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(child.mcp_action_required);
    assert!(matches!(&child.tool_choice, HarnessToolChoice::Required));
    assert!(child.prompt.contains(MCP_AGENT_TASK_INSTRUCTIONS));
    assert!(child.prompt.len() < 2_000);

    let plain = parse_text_only_responses_request(
        &serde_json::to_vec(&json!({
            "model":"Web picker choice",
            "input":"What does MCP mean?",
            "tools":[tool.clone()]
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(!plain.mcp_required);
    assert!(!plain.tool_work_expected);

    let negated = parse_text_only_responses_request(
        &serde_json::to_vec(&json!({
            "model":"Web picker choice",
            "input":"Do not use MCP; just explain it.",
            "tools":[tool]
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(!negated.mcp_required);

    let last_user_message_wins = parse_text_only_responses_request(
        &serde_json::to_vec(&json!({
            "model":"Web picker choice",
            "input":[
                {"type":"message","role":"user","content":"Use only the selected MCP connector."},
                {"type":"message","role":"assistant","content":"Previous result."},
                {"type":"message","role":"user","content":"What does MCP mean?"}
            ],
            "tools":[tool]
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(!last_user_message_wins.mcp_required);
}

#[test]
fn background_probe_preserves_other_models_levels_for_the_same_picker() {
    let path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join(format!("web2codex-capabilities-{}.json", Uuid::new_v4()));
    let snapshot = |reasoning_by_model| AccountCapabilities {
        probe_version: 5,
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
fn empty_background_probe_preserves_the_last_nonempty_model_list() {
    let path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join(format!("web2codex-capabilities-{}.json", Uuid::new_v4()));
    let previous = AccountCapabilities {
        probe_version: 5,
        picker_options: vec!["GPT-6".into(), "GPT-5.6".into()],
        reasoning_by_model: BTreeMap::from([("GPT-6".into(), vec!["medium".into()])]),
        detected_at_unix_seconds: 10,
    };
    store_account_capabilities(&path, previous, false).unwrap();
    store_account_capabilities(
        &path,
        AccountCapabilities {
            probe_version: 5,
            picker_options: Vec::new(),
            reasoning_by_model: BTreeMap::new(),
            detected_at_unix_seconds: 20,
        },
        true,
    )
    .unwrap();
    let stored: AccountCapabilities =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    std::fs::remove_file(path).unwrap();
    assert_eq!(stored.picker_options, ["GPT-6", "GPT-5.6"]);
    assert_eq!(stored.reasoning_by_model["GPT-6"], ["medium"]);
    assert_eq!(stored.detected_at_unix_seconds, 20);
}

#[test]
fn detected_web_models_accept_image_and_audio_input() {
    let snapshot = std::env::temp_dir().join(format!("web2codex-models-{}.json", Uuid::new_v4()));
    std::fs::write(
        &snapshot,
        serde_json::to_vec(&json!({
            "probe_version": 5,
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
fn opaque_file_id_media_is_rejected_with_a_local_resolution_error() {
    let image = serde_json::to_vec(&json!({
        "model":"Web picker choice",
        "input":[{"type":"message", "role":"user", "content":[
            {"type":"input_image", "file_id":"file_image_123"}
        ]}]
    }))
    .unwrap();
    assert!(matches!(
        parse_text_only_responses_request(&image),
        Err((_, "input_image_file_id_unavailable", message)) if message.contains("local attachment bytes")
    ));

    let audio = serde_json::to_vec(&json!({
        "model":"Web picker choice",
        "input":[{"type":"message", "role":"user", "content":[
            {"type":"input_audio", "file_id":"file_audio_123"}
        ]}]
    }))
    .unwrap();
    assert!(matches!(
        parse_text_only_responses_request(&audio),
        Err((_, "input_audio_file_id_unavailable", message)) if message.contains("local attachment bytes")
    ));
}

#[test]
fn tool_choice_supports_auto_none_required_and_named_tools() {
    let tools = json!([
        {"type":"function","name":"exec_command","parameters":{"type":"object"}},
        {"type":"custom","name":"apply_patch","format":{"type":"text"}}
    ]);
    for (choice, expected) in [
        (json!("auto"), ToolChoice::Auto),
        (json!("none"), ToolChoice::None),
        (json!("required"), ToolChoice::Required),
        (
            json!({"type":"function","name":"exec_command"}),
            ToolChoice::Named {
                kind: CallableToolKind::Function,
                namespace: "functions".into(),
                name: "exec_command".into(),
            },
        ),
        (
            json!({"type":"custom","name":"apply_patch"}),
            ToolChoice::Named {
                kind: CallableToolKind::Custom,
                namespace: "functions".into(),
                name: "apply_patch".into(),
            },
        ),
    ] {
        let parsed = parse_text_only_responses_request(
            &serde_json::to_vec(&json!({
                "model":"Web picker choice", "input":"Try a change",
                "tools":tools.clone(), "tool_choice":choice
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(parsed.tool_choice, expected);
    }
    let no_tools = serde_json::to_vec(&json!({
        "model":"Web picker choice", "input":"Hello", "tool_choice":"required"
    }))
    .unwrap();
    assert!(matches!(
        parse_text_only_responses_request(&no_tools),
        Err((_, "tool_choice_unsupported", _))
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
    assert!(!parsed.prompt.contains("Cosmic Birch"));
    assert!(parsed.prompt.len() < 2_000);
}

#[test]
fn fresh_prompts_include_terminal_format_limits_and_preserve_exact_output_requests() {
    let parsed = parse_text_only_responses_request(&serde_json::to_vec(&json!({
        "model":"Web picker choice",
        "stream":true,
        "instructions":"Follow the workspace instructions",
        "input":[{"type":"message","role":"user","content":"Reply with exactly: READY TO WORK. Also show a table and a left-to-right arrow diagram."}]
    })).unwrap()).unwrap();

    let developer_instructions = parsed.prompt.find("Developer instructions:").unwrap();
    let terminal_instructions = parsed.prompt.find(TUI_TEXT_RENDERING_INSTRUCTIONS).unwrap();
    let user_message = parsed
        .prompt
        .find("User:\nReply with exactly: READY TO WORK. Also show a table and a left-to-right arrow diagram.")
        .unwrap();
    assert!(developer_instructions < terminal_instructions);
    assert!(user_message < terminal_instructions);
    assert!(
        parsed.stream,
        "the host must keep the bridged Responses request streaming"
    );
    assert!(parsed.prompt.contains("Answer concisely in plain text or Markdown"));
    assert!(parsed.prompt.contains("Keep protocol data private"));
    assert!(parsed.prompt.len() < 1_000);
    assert!(parsed.prompt.contains(
        "Reply with exactly: READY TO WORK. Also show a table and a left-to-right arrow diagram."
    ));
}

#[test]
fn project_instructions_and_explicit_skill_request_reach_the_web_chat() {
    let parsed = parse_text_only_responses_request(&serde_json::to_vec(&json!({
        "model":"Web picker choice",
        "instructions":"Project instructions from AGENTS.md: preserve local changes. Use the workspace skill at /skills/example/SKILL.md.",
        "input":[{"type":"message","role":"user","content":[
            {"type":"input_text","text":"Use $example to update this project"}
        ]}],
        "tools":[{"type":"function","name":"exec_command","description":"TOOL_SCHEMA_MARKER","parameters":{"type":"object"}}]
    })).unwrap()).unwrap();
    assert!(
        parsed
            .prompt
            .contains("Project instructions from AGENTS.md")
    );
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
    assert_eq!(
        parsed.prompt,
        format!(
            "Earlier tool result:\nold result\n\nUser:\nNext task\n\n{TUI_TEXT_RENDERING_INSTRUCTIONS}\n{MCP_CHAT_OUTPUT_INSTRUCTIONS}"
        )
    );
}

#[test]
fn historical_tool_arguments_are_not_serialized_into_a_new_chat_prompt() {
    let parsed = parse_text_only_responses_request(
        &serde_json::to_vec(&json!({
            "model":"Web picker choice",
            "input":[
                {
                    "type":"mcp_call",
                    "call_id":"old-call",
                    "namespace":"mcp__example_connector",
                    "name":"perform_action",
                    "arguments":{
                        "operation":"functions.exec_command",
                        "arguments":{"cmd":"Write-Output MCP_ARGUMENT_SECRET"}
                    }
                },
                {
                    "type":"custom_tool_call",
                    "name":"apply_patch",
                    "input":"CUSTOM_TOOL_INPUT_SECRET"
                },
                {
                    "type":"local_shell_call",
                    "action":{"command":"Write-Output SHELL_ACTION_SECRET"}
                },
                {
                    "type":"mcp_tool_call_output",
                    "call_id":"old-call",
                    "output":{"content":[{"type":"text","text":"MCP_RESULT_TEXT"}]}
                },
                {"type":"message","role":"user","content":"Next task"}
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    assert!(parsed.prompt.contains(
        "Earlier tool call (mcp__example_connector.perform_action) was executed through MCP."
    ));
    assert!(parsed.prompt.contains("MCP_RESULT_TEXT"));
    assert!(parsed.prompt.contains("User:\nNext task"));
    for secret in [
        "MCP_ARGUMENT_SECRET",
        "CUSTOM_TOOL_INPUT_SECRET",
        "SHELL_ACTION_SECRET",
        "\"operation\"",
    ] {
        assert!(
            !parsed.prompt.contains(secret),
            "unexpected prompt text: {secret}"
        );
    }
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
fn new_temporary_chat_preserves_mcp_text_and_inline_images_without_serializing_structured_data() {
    let result = json!({
        "type":"function_call_output",
        "call_id":"mcp-call",
        "output": {
            "content":[
                {"type":"text","text":"MCP_TEXT_RESULT"},
                {"type":"image","mimeType":"image/png","data":"aGVsbG8="},
                {"type":"audio","mimeType":"audio/wav","data":"YXVkaW8="},
                {"type":"input_file","file_url":"data:application/pdf;base64,JVBERi0xLjQ=","filename":"C:\\workspace\\report final.pdf"}
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
    assert!(!parsed.prompt.contains(r#"{"path":"src/main.rs"}"#));
    assert!(
        parsed
            .prompt
            .contains("[Attached image: web2codex-image-1.png]")
    );
    assert_eq!(parsed.attachments.len(), 3);
    assert_eq!(
        parsed.attachments[0].data_url,
        "data:image/png;base64,aGVsbG8="
    );
    assert_eq!(
        parsed.attachments[1].data_url,
        "data:audio/wav;base64,YXVkaW8="
    );
    assert_eq!(parsed.attachments[2].name, "report final.pdf");
    assert_eq!(parsed.attachments[2].mime, "application/pdf");
    assert_eq!(
        parsed.attachments[2].data_url,
        "data:application/pdf;base64,JVBERi0xLjQ="
    );
    assert!(
        parsed
            .prompt
            .contains("[Attached audio: web2codex-audio-2.wav]")
    );
    assert!(parsed.prompt.contains("[Attached file: report final.pdf]"));
}

#[test]
fn structured_only_tool_results_are_not_serialized_into_a_new_chat_prompt() {
    let output = json!({
        "structuredContent": {"private_marker": "MCP_NATIVE_ONLY"}
    });
    let mut attachments = Vec::new();

    let text = tool_output_to_context(&output, true, &mut attachments);

    assert!(text.is_empty());
    assert!(attachments.is_empty());
    assert!(!text.contains("MCP_NATIVE_ONLY"));
    assert!(!text.contains("structuredContent"));
}

#[test]
fn structured_only_tool_errors_keep_a_short_error_notice_without_json() {
    let output = json!({
        "isError": true,
        "structuredContent": {"private_marker": "MCP_NATIVE_ONLY"}
    });
    let mut attachments = Vec::new();

    let text = tool_output_to_context(&output, true, &mut attachments);

    assert_eq!(text, "The previous MCP tool returned an error.");
    assert!(!text.contains("MCP_NATIVE_ONLY"));
    assert!(!text.contains("structuredContent"));
}

#[test]
fn unsupported_content_part_error_names_its_type_without_echoing_payload() {
    let request = serde_json::to_vec(&json!({
        "model":"Web picker choice",
        "input":[{"type":"message","role":"user","content":[
            {"type":"future_video_part","data":"PRIVATE_PAYLOAD"}
        ]}]
    }))
    .unwrap();
    let Err((_, code, message)) = parse_text_only_responses_request(&request) else {
        panic!("unsupported content part should be rejected");
    };
    assert_eq!(code, "input_format_unsupported");
    assert_eq!(
        message,
        "the conversation contains an unsupported content part type 'future_video_part'"
    );
    assert!(!message.contains("PRIVATE_PAYLOAD"));
}

#[test]
fn encrypted_content_is_skipped_while_neighboring_text_is_preserved() {
    let request = serde_json::to_vec(&json!({
        "model":"Web picker choice",
        "input":[{"type":"message","role":"user","content":[
            {"type":"input_text","text":"TEXT_BEFORE"},
            {"type":"encrypted_content","encrypted_content":"OPAQUE_PRIVATE_BLOB"},
            {"type":"input_text","text":"TEXT_AFTER"}
        ]}]
    }))
    .unwrap();
    let parsed = parse_text_only_responses_request(&request).unwrap();
    assert!(parsed.prompt.contains("TEXT_BEFORE"));
    assert!(parsed.prompt.contains("TEXT_AFTER"));
    assert!(!parsed.prompt.contains("OPAQUE_PRIVATE_BLOB"));
}

#[test]
fn earlier_typed_mcp_resources_keep_their_payload_and_links() {
    let resource = json!({
        "type":"resource",
        "resource":{"uri":"file:///workspace/notes.txt","mimeType":"text/plain","text":"RESOURCE_TEXT"}
    });
    let resource_link = json!({
        "type":"resource_link",
        "name":"design notes",
        "uri":"web2codex://workspace/design-notes",
        "description":"DESIGN_LINK_DESCRIPTION",
        "mimeType":"text/markdown"
    });
    let parsed = parse_text_only_responses_request(
        &serde_json::to_vec(&json!({
            "model":"Web picker choice", "input":[
                {"type":"function_call_output","call_id":"mcp-call","output":{
                    "content":[resource, resource_link],
                    "structuredContent":{"marker":"STRUCTURED_RESOURCE_DATA"}
                }},
                {"type":"message","role":"user","content":"Use the prior resource"}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(parsed.prompt.contains("RESOURCE_TEXT"));
    assert!(parsed.prompt.contains("file:///workspace/notes.txt"));
    assert!(parsed.prompt.contains("DESIGN_LINK_DESCRIPTION"));
    assert!(parsed.prompt.contains("web2codex://workspace/design-notes"));
    assert!(parsed.prompt.contains("STRUCTURED_RESOURCE_DATA"));
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
