//! Model-family presentation at constrained sizes and with configured list bindings.

use super::*;
use pretty_assertions::assert_eq;

fn set_effort_profile_model(chat: &mut ChatWidget) -> String {
    set_chatgpt_auth(chat);
    let model = chat.current_model().to_string();
    let mut preset = get_available_model(chat, &model);
    preset.supported_reasoning_efforts = vec![
        ReasoningEffortPreset {
            effort: ReasoningEffortConfig::Low,
            description: "instant".to_string(),
        },
        ReasoningEffortPreset {
            effort: ReasoningEffortConfig::Medium,
            description: "balanced".to_string(),
        },
        ReasoningEffortPreset {
            effort: ReasoningEffortConfig::High,
            description: "thorough".to_string(),
        },
    ];
    Arc::make_mut(&mut chat.model_catalog).models = vec![preset];
    model
}

#[tokio::test]
async fn effort_profile_popup_shows_native_levels_and_current_selection() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.5")).await;
    set_effort_profile_model(&mut chat);
    chat.set_reasoning_effort(Some(ReasoningEffortConfig::Medium));

    chat.open_effort_profile_popup();

    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert!(
        popup.contains("Instant"),
        "expected Instant effort:\n{popup}"
    );
    assert!(popup.contains("Medium"), "expected Medium effort:\n{popup}");
    assert!(popup.contains("High"), "expected High effort:\n{popup}");
    assert!(popup.contains("Applied through ChatGPT's native effort control"));
}

#[tokio::test]
async fn effort_profile_instant_maps_to_native_low_reasoning() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.5")).await;
    let model = set_effort_profile_model(&mut chat);
    while rx.try_recv().is_ok() {}

    chat.select_effort_profile("Instant");

    let events = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(
        events.iter().any(|event| matches!(
            event,
            AppEvent::ApplyModelSelection {
                model: selected_model,
                effort: Some(ReasoningEffortConfig::Low),
            } if selected_model == &model
        )),
        "expected Instant to apply native low reasoning; events: {events:?}"
    );
}

#[tokio::test]
async fn effort_profile_refreshes_web_catalog_before_showing_detected_levels() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.5")).await;
    set_effort_profile_model(&mut chat);
    chat.config.model_provider_id = "web2codex".to_string();
    let mut preset = chat.model_catalog.models[0].clone();
    Arc::make_mut(&mut chat.model_catalog).models[0]
        .supported_reasoning_efforts
        .clear();
    preset.supported_reasoning_efforts = vec![ReasoningEffortPreset {
        effort: ReasoningEffortConfig::XHigh,
        description: "extra high".to_string(),
    }];

    chat.open_effort_profile_popup();
    let request_id = chat.model_popup_request_id.expect("effort catalog request");
    assert!(render_bottom_popup(&chat, /*width*/ 80).contains("Loading ChatGPT effort levels"));
    assert!(chat.on_models_loaded(request_id, Ok(vec![preset])));
    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert!(popup.contains("Extra High"), "{popup}");
    assert!(!popup.contains("Instant"), "{popup}");
}

#[tokio::test]
async fn effort_profile_applies_explicit_level_after_web_catalog_refresh() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.5")).await;
    let model = set_effort_profile_model(&mut chat);
    chat.config.model_provider_id = "web2codex".to_string();
    let mut preset = chat.model_catalog.models[0].clone();
    preset
        .supported_reasoning_efforts
        .push(ReasoningEffortPreset {
            effort: ReasoningEffortConfig::XHigh,
            description: "extra high".to_string(),
        });
    chat.select_effort_profile("xhigh");
    let request_id = chat.model_popup_request_id.expect("effort catalog request");
    assert!(
        !std::iter::from_fn(|| rx.try_recv().ok())
            .any(|event| matches!(event, AppEvent::ApplyModelSelection { .. }))
    );
    assert!(chat.on_models_loaded(request_id, Ok(vec![preset])));
    assert!(std::iter::from_fn(|| rx.try_recv().ok()).any(|event| matches!(event,
        AppEvent::ApplyModelSelection { model: selected_model, effort: Some(ReasoningEffortConfig::XHigh) } if selected_model == model
    )));
}

#[tokio::test]
async fn model_picker_compact_hint_and_actions_follow_configured_bindings() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.5")).await;
    let mut keymap = crate::keymap::RuntimeKeymap::defaults();
    keymap.list.accept = vec![key_hint::plain(KeyCode::F(3))];
    keymap.list.cancel = vec![key_hint::plain(KeyCode::F(2))];
    chat.bottom_pane.set_keymap_bindings(&keymap);
    chat.open_all_models_popup();
    let popup = render_bottom_popup(&chat, /*width*/ 40);
    assert_eq!(popup.lines().last().unwrap().trim(), "f3 select · f2 back");
    chat.handle_key_event(KeyEvent::from(KeyCode::F(3)));
    assert_matches!(rx.try_recv(), Ok(AppEvent::OpenReasoningPopup { .. }));
    chat.handle_key_event(KeyEvent::from(KeyCode::F(2)));
    assert!(chat.no_modal_or_popup_active());
}

#[tokio::test]
async fn model_picker_opens_after_discovery_populates_an_empty_catalog() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.5")).await;
    chat.thread_id = Some(ThreadId::new());
    let presets = std::mem::take(&mut Arc::make_mut(&mut chat.model_catalog).models);
    chat.open_model_popup();
    let loading = render_bottom_popup(&chat, /*width*/ 80);
    let loading_title = loading.lines().find(|line| line.contains("Loading models"));
    insta::assert_snapshot!(loading_title.unwrap().trim(), @"Loading models…");
    let obsolete = chat.model_popup_request_id.unwrap();
    chat.handle_key_event(KeyCode::Esc.into());
    assert!(chat.no_modal_or_popup_active());
    chat.open_model_popup();
    assert!(!chat.on_models_loaded(obsolete, Ok(presets.clone())));
    let request_id = chat.model_popup_request_id.unwrap();
    assert!(chat.on_models_loaded(request_id, Ok(presets)));
    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert_chatwidget_snapshot!("model_selection_popup", popup);
}
