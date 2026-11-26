#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::mpsc::{self, SyncSender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tao::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
#[cfg(target_os = "linux")]
use tao::platform::unix::WindowExtUnix;
use tao::window::{Window, WindowBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use uuid::Uuid;
#[cfg(target_os = "macos")]
use wry::WebViewBuilderExtDarwin;
use wry::{Rect, WebContext, WebView, WebViewBuilder};

#[cfg(test)]
mod bridge_tests;
mod responses_http;
mod turn_broker;

use turn_broker::TurnBroker;

const CHATGPT_SIGN_IN_URL: &str = "https://chatgpt.com/";
const CHATGPT_TEMPORARY_CHAT_URL: &str = "https://chatgpt.com/?temporary-chat=true";
const BROWSER_BRIDGE_PROTOCOL: u32 = 3;
const STALE_LOCK_AGE: Duration = Duration::from_secs(30);
const MAX_REQUEST_BODY_BYTES: usize = 32 * 1024 * 1024;
const MAX_REQUEST_HEADER_BYTES: usize = 16 * 1024;
const MAX_RESPONSE_TEXT_BYTES: usize = 4 * 1024 * 1024;
const MAX_PROMPT_TEXT_BYTES: usize = 2 * 1024 * 1024;
const SESSION_DISCONNECT_GRACE: Duration = Duration::from_secs(2);
const MAX_TOOL_SCHEMAS_BYTES: usize = 4 * 1024 * 1024;
const MAX_TOOL_COUNT: usize = 1024;
const MAX_TOOL_CALL_INPUT_BYTES: usize = 512 * 1024;
const PENDING_AGENT_CLOSE_TTL: Duration = Duration::from_secs(300);
const INTERRUPTED_PRIMARY_CHAT_TTL: Duration = Duration::from_secs(10 * 60);
const TUI_TEXT_RENDERING_INSTRUCTIONS: &str =
    "Use Markdown headings, lists, tables, and code blocks; show URLs directly.";
const MCP_CHAT_OUTPUT_INSTRUCTIONS: &str = "Keep protocol data private. Summarize completed work and report errors plainly.";
const TOOL_COMPLETION_INSTRUCTIONS: &str = "For explicitly requested agents, work in parallel, do not recurse, and wait for all final results.";
const MCP_TOOL_ROUTING_INSTRUCTIONS: &str = "Use only the selected connector's documented tools; do not guess or fall back, and report failures.";
const MCP_AGENT_TASK_INSTRUCTIONS: &str = "Use only the selected connector's documented tools to complete the assigned task; report only results verified by tool output.";
// ChatGPT already supplies its own system prompt. Compact Web2Codex's built-in
// prompt even when it is combined with custom project instructions.
const DEFAULT_BASE_INSTRUCTIONS: &str =
    include_str!("../../protocol/src/prompts/base_instructions/default.md");
const COMPACT_BASE_INSTRUCTIONS: &str = "Act as a coding agent. Follow user and project instructions; use tools when needed and report verified results.";
const INFERENCE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const ACCOUNT_CAPABILITY_PROBE_SCRIPT: &str = include_str!("model_picker.js");
const TEMPORARY_CHAT_ENFORCEMENT_SCRIPT: &str = r##"
(() => {
  if (window.top !== window) return;

  const chatOrigin = "https://chatgpt.com";
  const chatHosts = new Set(["chatgpt.com", "chat.openai.com"]);
  const temporaryChatUrl = `${chatOrigin}/?temporary-chat=true`;
  const isChatUrl = url => url.protocol === "https:" && chatHosts.has(url.hostname.toLowerCase().replace(/\.$/, ""));
  const preserveTemporaryChat = value => {
    if (value == null) return null;
    try {
      const url = new URL(value, location.href);
      if (!isChatUrl(url)) return null;
      if (url.origin !== chatOrigin || url.pathname !== "/") return temporaryChatUrl;
      url.searchParams.set("temporary-chat", "true");
      return `${url.pathname}${url.search}${url.hash}`;
    } catch (_) {
      return null;
    }
  };
  const ensureTemporaryChat = () => {
    const current = new URL(location.href);
    if (!isChatUrl(current)) return;
    const destination = preserveTemporaryChat(current.href);
    if (destination && new URL(destination, current.href).href !== current.href) location.replace(destination);
  };

  // Keep the WebView on Temporary Chat, including when ChatGPT uses SPA history
  // navigation to open a saved conversation.
  for (const method of ["pushState", "replaceState"]) {
    const original = history[method];
    history[method] = function(state, title, url) {
      const next = preserveTemporaryChat(url);
      if (url != null && next == null) return original.call(this, state, title, url);
      if (url != null) {
        const target = new URL(url, location.href);
        if (isChatUrl(target) && (target.origin !== chatOrigin || target.pathname !== "/")) {
          return original.call(this, state, title, temporaryChatUrl);
        }
      }
      return original.call(this, state, title, next ?? url);
    };
  }
  addEventListener("popstate", ensureTemporaryChat, true);
  addEventListener("pageshow", ensureTemporaryChat, true);
  // The Navigation API can be used for same-document route changes without
  // calling the History methods wrapped above. Cancel non-entry routes there too.
  if (window.navigation?.addEventListener) {
    window.navigation.addEventListener("navigate", event => {
      let target;
      try {
        target = new URL(event.destination.url);
      } catch (_) {
        return;
      }
      if (!isChatUrl(target)) return;
      const isTemporaryEntry = target.origin === chatOrigin
        && target.pathname === "/"
        && target.searchParams.get("temporary-chat") === "true";
      if (!isTemporaryEntry && event.cancelable) {
        event.preventDefault();
      }
    }, true);
  }
  ensureTemporaryChat();

  // Links can be handled by ChatGPT's delegated click handlers before a route
  // reaches the browser navigation layer. Preserve normal root/new-chat links,
  // but keep saved-chat and other app routes on the Temporary Chat entry.
  document.addEventListener("click", event => {
    const anchor = event.composedPath().find(node => node instanceof HTMLAnchorElement);
    if (!anchor || anchor.hasAttribute("download")) return;
    let target;
    try {
      target = new URL(anchor.href, location.href);
    } catch (_) {
      return;
    }
    if (!isChatUrl(target)) return;
    if (target.origin === chatOrigin && target.pathname === "/") {
      target.searchParams.set("temporary-chat", "true");
      anchor.href = `${target.pathname}${target.search}${target.hash}`;
      return;
    }
    event.preventDefault();
    event.stopImmediatePropagation();
    location.replace(temporaryChatUrl);
  }, true);

  // The browser instance was opened in Temporary Chat; prevent its visible control
  // from switching this ChatGPT session to saved-chat mode.
  document.addEventListener("click", event => {
    if (!(event.target instanceof Element)) return;
    const control = event.target.closest(
      'button, [role="button"], [role="switch"], [role="menuitem"], [role="menuitemcheckbox"], [role="menuitemradio"]'
    );
    if (!control) return;
    const label = [
      control.getAttribute("aria-label"),
      control.getAttribute("title"),
      control.getAttribute("data-testid"),
      control.innerText,
      control.textContent
    ].filter(Boolean).join(" ").replace(/\s+/g, " ").trim();
    if (!/(temporary[\s-]*(chat|conversation)|temp[\s-]*chat|временн\w*[\s-]*(чат|разговор)|(чат|разговор)[\s-]*временн\w*)/i.test(label)) return;
    event.preventDefault();
    event.stopImmediatePropagation();
  }, true);
})();
"##;
const RESPONSES_BRIDGE_SCRIPT: &str = include_str!("responses_bridge.js");
const ACCOUNT_CHOOSER_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Sign in to Web2Codex</title>
  <style>
    :root { color-scheme: dark; font: 15px/1.5 system-ui, sans-serif; background: #111318; color: #f2f3f5; }
    * { box-sizing: border-box; }
    body { min-height: 100vh; margin: 0; display: grid; place-items: center; padding: 28px; }
    main { width: min(100%, 520px); }
    h1 { margin: 0 0 8px; font-size: 26px; letter-spacing: -.03em; }
    p { color: #a8acb6; margin: 0 0 24px; }
    .choices { display: grid; gap: 12px; }
    button { width: 100%; min-height: 58px; border: 1px solid #343943; border-radius: 12px; padding: 14px 18px; background: #1a1d24; color: inherit; text-align: left; font: inherit; font-weight: 600; cursor: pointer; }
    button:hover, button:focus-visible { background: #252a34; border-color: #737d90; outline: none; }
    button:disabled { cursor: wait; opacity: .65; }
    #status { min-height: 24px; margin-top: 16px; font-size: 13px; }
    .note { margin-top: 18px; font-size: 12px; }
  </style>
</head>
<body>
  <main>
    <h1>Sign in to ChatGPT</h1>
    <p>Choose the account you want to use. The choice is saved locally; ChatGPT still controls what that account can access.</p>
    <div class="choices">
      <button data-tier="free-go">Login with Free/Go account</button>
      <button data-tier="plus">Login with Plus account</button>
      <button data-tier="pro">Login with Pro account</button>
    </div>
    <p id="status" role="status" aria-live="polite"></p>
    <p class="note">Sign-in stays in this private Web2Codex browser profile.</p>
  </main>
  <script>
    const buttons = [...document.querySelectorAll("button[data-tier]")];
    const status = document.querySelector("#status");
    for (const button of buttons) button.addEventListener("click", () => {
      for (const item of buttons) item.disabled = true;
      status.textContent = "Opening ChatGPT sign-in…";
      window.ipc.postMessage(button.dataset.tier);
    });
  </script>
</body>
</html>"##;
#[cfg(target_os = "windows")]
const WEBVIEW_CREATION_CONTEXT: &str =
    "failed to create the native WebView; install the Microsoft Edge WebView2 Runtime";
#[cfg(target_os = "linux")]
const WEBVIEW_CREATION_CONTEXT: &str =
    "failed to create the native WebView; install GTK 3 and WebKitGTK 4.1 runtime libraries";
#[cfg(target_os = "macos")]
const WEBVIEW_CREATION_CONTEXT: &str = "failed to create the native WKWebView";
#[cfg(target_os = "macos")]
const PROFILE_DATA_STORE_ID: [u8; 16] = [
    0x43, 0x6f, 0x64, 0x65, 0x78, 0x57, 0x65, 0x62, 0x50, 0x72, 0x6f, 0x66, 0x69, 0x6c, 0x65, 0x31,
];

#[derive(Debug, Deserialize, Serialize)]
struct BrowserState {
    address: SocketAddr,
    token: String,
    #[serde(default)]
    bridge_protocol: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct AccountCapabilities {
    probe_version: u8,
    picker_options: Vec<String>,
    #[serde(default)]
    reasoning_by_model: BTreeMap<String, Vec<String>>,
    detected_at_unix_seconds: u64,
}

fn account_capabilities_from_message(message: &serde_json::Value) -> Option<AccountCapabilities> {
    if message
        .get("probe_version")
        .and_then(serde_json::Value::as_u64)
        != Some(5)
    {
        return None;
    }
    let picker_options = message
        .get("picker_options")
        .and_then(serde_json::Value::as_array)?;
    if picker_options.len() > 40 {
        return None;
    }
    let picker_options = picker_options
        .iter()
        .filter_map(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|option| !option.is_empty() && option.len() <= 120)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let reasoning_by_model = message
        .get("reasoning_by_model")
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(label, _)| picker_options.contains(label))
        .filter_map(|(label, levels)| {
            let levels = levels.as_array()?;
            if levels.len() > 5 {
                return None;
            }
            let levels = levels
                .iter()
                .filter_map(serde_json::Value::as_str)
                .filter(|level| matches!(*level, "low" | "medium" | "high" | "xhigh" | "max"))
                .map(str::to_owned)
                .collect::<Vec<_>>();
            (!levels.is_empty()).then(|| (label.clone(), levels))
        })
        .collect();
    let detected_at_unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Some(AccountCapabilities {
        probe_version: 5,
        picker_options,
        reasoning_by_model,
        detected_at_unix_seconds,
    })
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
struct LocalUsageStats {
    version: u8,
    total: LocalUsageBucket,
    days: BTreeMap<String, LocalUsageBucket>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
struct LocalUsageBucket {
    requests: u64,
    completed_responses: u64,
    failed_responses: u64,
    input_characters: u64,
    output_characters: u64,
    response_milliseconds: u64,
    model_requests: BTreeMap<String, u64>,
}

impl LocalUsageBucket {
    fn record(
        &mut self,
        model: &str,
        input_characters: usize,
        output_characters: Option<usize>,
        response_milliseconds: u64,
    ) {
        self.requests = self.requests.saturating_add(1);
        self.response_milliseconds = self
            .response_milliseconds
            .saturating_add(response_milliseconds);
        self.input_characters = self
            .input_characters
            .saturating_add(u64::try_from(input_characters).unwrap_or(u64::MAX));
        let count = self.model_requests.entry(model.to_string()).or_default();
        *count = count.saturating_add(1);
        match output_characters {
            Some(output_characters) => {
                self.completed_responses = self.completed_responses.saturating_add(1);
                self.output_characters = self
                    .output_characters
                    .saturating_add(u64::try_from(output_characters).unwrap_or(u64::MAX));
            }
            None => self.failed_responses = self.failed_responses.saturating_add(1),
        }
    }
}

fn record_local_usage(
    path: &Path,
    model: &str,
    input_characters: usize,
    output_characters: Option<usize>,
    response_milliseconds: u64,
) -> anyhow::Result<()> {
    let mut stats = fs::read(path)
        .ok()
        .and_then(|contents| serde_json::from_slice::<LocalUsageStats>(&contents).ok())
        .filter(|stats| stats.version == 1)
        .unwrap_or_default();
    stats.version = 1;
    let date = chrono::Utc::now().date_naive().to_string();
    stats.total.record(
        model,
        input_characters,
        output_characters,
        response_milliseconds,
    );
    stats.days.entry(date).or_default().record(
        model,
        input_characters,
        output_characters,
        response_milliseconds,
    );
    let oldest_day = (chrono::Utc::now().date_naive() - chrono::Duration::days(365)).to_string();
    stats.days.retain(|day, _| day >= &oldest_day);
    write_private_json(path, &stats)
}

fn model_catalog_from_picker_snapshot(path: &Path) -> serde_json::Value {
    let Ok(contents) = fs::read(path) else {
        return serde_json::json!({ "models": [] });
    };
    let Ok(capabilities) = serde_json::from_slice::<AccountCapabilities>(&contents) else {
        return serde_json::json!({ "models": [] });
    };
    model_catalog_from_capabilities(&capabilities)
}

fn model_catalog_from_capabilities(capabilities: &AccountCapabilities) -> serde_json::Value {
    if capabilities.probe_version != 5 {
        return serde_json::json!({ "models": [] });
    }

    let mut seen = Vec::new();
    let reasoning_by_model = &capabilities.reasoning_by_model;
    let models = capabilities
        .picker_options
        .iter()
        .cloned()
        .enumerate()
        .filter_map(|(priority, label)| {
            let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
            if label.is_empty() || label.len() > 120 || seen.contains(&label) {
                return None;
            }
            seen.push(label.clone());
            let levels = reasoning_by_model
                .get(&label)
                .into_iter()
                .flatten()
                .filter(|effort| matches!(effort.as_str(), "low" | "medium" | "high" | "xhigh" | "max"))
                .map(|effort| serde_json::json!({
                    "effort": effort,
                    "description": "Detected in the ChatGPT Web reasoning control."
                }))
                .collect::<Vec<_>>();
            let default_effort = if levels.iter().any(|level| level["effort"] == "medium") {
                Some("medium")
            } else {
                levels.first().and_then(|level| level["effort"].as_str())
            };
            let supports_reasoning_effort_updates = !levels.is_empty();
            Some(serde_json::json!({
                // The visible picker label is an opaque, account-specific key. The request bridge
                // checks it against the selected Web model before submitting any prompt.
                "slug": label.clone(),
                "display_name": label.clone(),
                "description": "Detected in the signed-in ChatGPT Web model picker.",
                // The Codex catalog parser requires an instruction template.
                // Native tool descriptions and schemas are supplied via MCP.
                "base_instructions": "You are a coding agent. Follow the user's instructions and report your work clearly. Always reply to the user in clear ordinary prose; never return a bare JSON response.",
                "default_reasoning_level": default_effort,
                "supported_reasoning_levels": levels,
                // Every detected ChatGPT Web model receives the same harness-side
                // context cap. The user can raise it through /context, up to 400k.
                "context_window": 200_000,
                "max_context_window": 400_000,
                "effective_context_window_percent": 100,
                "shell_type": "unified_exec",
                "tool_mode": "direct",
                "multi_agent_version": "v1",
                "visibility": "list",
                "supported_in_api": true,
                "priority": i32::try_from(priority).unwrap_or(i32::MAX),
                "availability_nux": null,
                "upgrade": null,
                "include_skills_usage_instructions": true,
                "include_plugin_usage_instructions": false,
                "include_apps_usage_instructions": false,
                "supports_reasoning_summary_parameter": false,
                "default_reasoning_summary": "none",
                "support_verbosity": false,
                "default_verbosity": null,
                "apply_patch_tool_type": "freeform",
                "web_search_tool_type": "text",
                "web_search_provider": null,
                "truncation_policy": { "mode": "bytes", "limit": 10000 },
                "supports_image_detail_original": false,
                "experimental_supported_tools": [],
                "input_modalities": ["text", "image", "audio"],
                "supports_search_tool": false,
                "supports_experimental_context": false,
                "use_responses_lite": false,
                "supports_reasoning_effort_updates": supports_reasoning_effort_updates
            }))
        })
        .collect::<Vec<_>>();
    serde_json::json!({ "models": models })
}

#[cfg(test)]
mod model_catalog_tests {
    use super::*;

    fn capabilities(reasoning_levels: &[&str]) -> AccountCapabilities {
        AccountCapabilities {
            probe_version: 5,
            picker_options: vec!["GPT-6 default".to_string()],
            reasoning_by_model: BTreeMap::from([(
                "GPT-6 default".to_string(),
                reasoning_levels
                    .iter()
                    .map(|level| (*level).to_string())
                    .collect(),
            )]),
            detected_at_unix_seconds: 0,
        }
    }

    #[test]
    fn detected_picker_efforts_enable_effort_updates() {
        let catalog = model_catalog_from_capabilities(&capabilities(&["low", "medium", "high"]));
        let model = &catalog["models"][0];

        assert_eq!(model["supports_reasoning_effort_updates"], true);
        assert_eq!(model["multi_agent_version"], "v1");
        assert_eq!(
            model["supported_reasoning_levels"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn absent_picker_efforts_do_not_claim_support() {
        let catalog = model_catalog_from_capabilities(&capabilities(&[]));
        let model = &catalog["models"][0];

        assert_eq!(model["supports_reasoning_effort_updates"], false);
        assert!(
            model["supported_reasoning_levels"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum AccountTierChoice {
    FreeGo,
    Plus,
    Pro,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BrowserPage {
    SignIn,
    TemporaryChat,
    AccountChooser,
}

enum BrowserEvent {
    Chrome(String),
    SetPowerSliderValue {
        view_id: String,
        request_id: String,
        index: u8,
        allow_focus: bool,
    },
    PowerSliderValueResult {
        view_id: String,
        request_id: String,
        result: Result<(), String>,
    },
    NewTab(String),
    BackgroundTab(String, SyncSender<Result<usize, String>>),
    ListTabs(SyncSender<Vec<String>>),
    CloseAgentTab(String, Option<String>, SyncSender<AgentTabCloseOutcome>),
    Open(BrowserPage, bool, Option<SyncSender<Result<(), String>>>),
    ClientConnected(SyncSender<()>),
    ClientDisconnected,
    ContinueLogin(AccountTierChoice),
    AccountCapabilities(AccountCapabilities, bool),
    ResponsesBridgeReady {
        request_id: String,
        conversation_has_history: bool,
    },
    ResponsesBridgeUnavailable {
        request_id: String,
        binding: Arc<Mutex<Option<String>>>,
        document_reloaded: bool,
        error: String,
    },
    ResponsesPromptSubmitted(String),
    ResponsesAuthRequired(String),
    ResponsesRequest {
        request_id: String,
        conversation_id: Option<String>,
        conversation_epoch: Option<String>,
        agent: Option<AgentRequestIdentity>,
        setup_smoke_test: bool,
        model: String,
        reasoning_effort: Option<String>,
        connector_required: bool,
        mcp_required: bool,
        mcp_action_required: bool,
        tool_work_expected: bool,
        prompt: String,
        stages: Vec<serde_json::Value>,
        resume_prompt: String,
        attachments: Vec<MediaAttachment>,
        resume_attachments: Vec<MediaAttachment>,
        interruption_prompt: Option<String>,
        interruption_attachment_names: Vec<String>,
    },
    ResponsesToolState(String),
    ResponsesText {
        request_id: String,
        delta: String,
    },
    ResponsesReasoningHeading {
        request_id: String,
        heading: String,
    },
    ResponsesResult {
        request_id: String,
        revision: Option<u64>,
        result: Result<String, String>,
    },
    CancelResponses(String),
    ReleaseResponses(String),
    Hide,
    Stop(Option<TcpStream>),
}

#[cfg(target_os = "windows")]
fn power_slider_window_is_foreground(hwnd: usize) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, IsChild, IsWindowVisible};

    let hwnd = hwnd as windows_sys::Win32::Foundation::HWND;
    unsafe {
        let foreground = GetForegroundWindow();
        !foreground.is_null() && IsChild(foreground, hwnd) != 0 && IsWindowVisible(hwnd) != 0
    }
}

#[cfg(target_os = "windows")]
fn set_accessible_power_slider(hwnd: usize, index: u8, allow_focus: bool) -> anyhow::Result<()> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_MULTITHREADED,
    };
    use windows::Win32::UI::Accessibility::{
        IUIAutomation, IUIAutomationExpandCollapsePattern, IUIAutomationInvokePattern,
        IUIAutomationRangeValuePattern, CUIAutomation, TreeScope_Descendants,
        UIA_ExpandCollapsePatternId, UIA_InvokePatternId, UIA_MenuItemControlTypeId,
        UIA_RangeValuePatternId, UIA_SliderControlTypeId,
    };

    anyhow::ensure!(index <= 2, "ChatGPT power slider index is out of range");
    let allow_focus = allow_focus && power_slider_window_is_foreground(hwnd);
    let ensure_foreground = || -> anyhow::Result<()> {
        anyhow::ensure!(allow_focus && power_slider_window_is_foreground(hwnd), "ChatGPT power slider source is no longer visible in the foreground");
        Ok(())
    };
    let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    let result = (|| {
        let automation: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }?;
        let root = unsafe { automation.ElementFromHandle(HWND(hwnd as *mut _)) }?;
        let condition = unsafe { automation.CreateTrueCondition() }?;
        let find_candidates = || -> anyhow::Result<Vec<(
            windows::Win32::UI::Accessibility::IUIAutomationElement,
            IUIAutomationRangeValuePattern,
            f64,
        )>> {
            let elements = unsafe { root.FindAll(TreeScope_Descendants, &condition) }?;
            let length = unsafe { elements.Length() }?;
            let mut candidates = Vec::new();
            for position in 0..length {
                let Ok(element) = (unsafe { elements.GetElement(position) }) else {
                    continue;
                };
                if unsafe { element.CurrentControlType() }.ok() != Some(UIA_SliderControlTypeId) {
                    continue;
                }
                let Ok(pattern) = (unsafe {
                    element.GetCurrentPatternAs::<IUIAutomationRangeValuePattern>(
                        UIA_RangeValuePatternId,
                    )
                }) else {
                    continue;
                };
                let (Ok(minimum), Ok(maximum), Ok(read_only), Ok(offscreen), Ok(enabled), Ok(current)) = (
                    unsafe { pattern.CurrentMinimum() },
                    unsafe { pattern.CurrentMaximum() },
                    unsafe { pattern.CurrentIsReadOnly() },
                    unsafe { element.CurrentIsOffscreen() },
                    unsafe { element.CurrentIsEnabled() },
                    unsafe { pattern.CurrentValue() },
                ) else {
                    continue;
                };
                if minimum == 0.0
                    && maximum == 2.0
                    && [0.0, 1.0, 2.0].contains(&current)
                    && !read_only.as_bool()
                    && enabled.as_bool()
                    && (!allow_focus || !offscreen.as_bool())
                {
                    candidates.push((element, pattern, current));
                }
            }
            Ok(candidates)
        };

        let mut candidates = find_candidates()?;
        if candidates.is_empty() {
            // ChatGPT initially exposes its power control as a menu item; the
            // range slider appears in the UI Automation tree only after that
            // item is expanded. Expanding through UI Automation does not
            // activate the browser window. SetFocus remains limited to an
            // already-foreground source view.
            let elements = unsafe { root.FindAll(TreeScope_Descendants, &condition) }?;
            let length = unsafe { elements.Length() }?;
            for position in 0..length {
                let Ok(element) = (unsafe { elements.GetElement(position) }) else {
                    continue;
                };
                if unsafe { element.CurrentControlType() }.ok() != Some(UIA_MenuItemControlTypeId) {
                    continue;
                }
                let Ok(name) = (unsafe { element.CurrentName() }) else {
                    continue;
                };
                let normalized_name = name.to_string().to_lowercase();
                if !normalized_name.contains("power") && !normalized_name.contains("мощ") {
                    continue;
                }
                if allow_focus {
                    ensure_foreground()?;
                    let _ = unsafe { element.SetFocus() };
                }
                if let Ok(pattern) = unsafe {
                    element.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(
                        UIA_ExpandCollapsePatternId,
                    )
                } {
                    if allow_focus {
                        ensure_foreground()?;
                    }
                    let _ = unsafe { pattern.Expand() };
                }
                if let Ok(pattern) = unsafe {
                    element.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
                } {
                    if allow_focus {
                        ensure_foreground()?;
                    }
                    let _ = unsafe { pattern.Invoke() };
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(150));
            candidates = find_candidates()?;
        }

        anyhow::ensure!(
            candidates.len() == 1,
            "ChatGPT did not expose one writable three-position power slider in the source view"
        );
        if (candidates[0].2 - f64::from(index)).abs() < f64::EPSILON {
            return Ok(());
        }
        // Focus only a source view that was already active and foreground.
        // For background views use the provider's RangeValue pattern directly,
        // if it exposes the slider at all. The page must still verify that the
        // selection persists, since UIA success alone does not prove React committed it.
        if allow_focus {
            ensure_foreground()?;
            let _ = unsafe { candidates[0].0.SetFocus() };
            ensure_foreground()?;
        }
        unsafe { candidates[0].1.SetValue(f64::from(index)) }?;
        let mut applied = false;
        for _ in 0..20 {
            let current = unsafe { candidates[0].1.CurrentValue() }?;
            if (current - f64::from(index)).abs() < f64::EPSILON {
                applied = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        anyhow::ensure!(applied, "ChatGPT power slider did not accept the requested value");
        Ok(())
    })();
    if initialized {
        unsafe { CoUninitialize() };
    }
    result
}

#[cfg(not(target_os = "windows"))]
fn set_accessible_power_slider(_hwnd: usize, _index: u8, _allow_focus: bool) -> anyhow::Result<()> {
    anyhow::bail!("native ChatGPT power slider input is only available on Windows")
}

struct PendingResponsesRequest {
    model: String,
    input_characters: usize,
    resume_input_characters: Option<usize>,
    compaction: bool,
    // Retain the payload until the page confirms the send click. A login
    // redirect before that point can safely resume the same request.
    payload: Option<serde_json::Value>,
    resume_payload: Option<serde_json::Value>,
    dispatch_started: bool,
    awaiting_auth: bool,
    bridge_ready_deadline: Option<Instant>,
    started_at: std::time::Instant,
}

struct InterruptedPrimaryChat {
    tab_index: usize,
    conversation_epoch: Option<String>,
    request_id_state: Option<Arc<Mutex<Option<String>>>>,
    interrupted_at: Instant,
}

// Initial composer discovery and the capability probe can take 105 seconds.
const RESPONSE_BRIDGE_READY_TIMEOUT: Duration = Duration::from_secs(120);

fn response_bridge_wakeup(
    idle_deadline: Option<Instant>,
    pending: &HashMap<String, PendingResponsesRequest>,
) -> ControlFlow {
    idle_deadline
        .into_iter()
        .chain(pending.values().filter_map(|request| request.bridge_ready_deadline))
        .min()
        .map_or(ControlFlow::Wait, ControlFlow::WaitUntil)
}

fn response_signal_script(request_id: &str, assignment: &str) -> String {
    format!(
        "if (window.__web2codexRequestId === {}) {{ {assignment} }}",
        serde_json::json!(request_id)
    )
}

fn response_bind_script(request_id: &str) -> String {
    format!(
        r#"(async () => {{
            const requestId = {};
            let accepted = false;
            try {{ accepted = await window.__web2codexBindRequest?.(requestId); }} catch {{}}
            if (!accepted) window.ipc.postMessage(JSON.stringify({{ type: 'responses-bind-rejected', request_id: requestId }}));
        }})();"#,
        serde_json::json!(request_id)
    )
}

const COLLAB_SPAWN_SUBAGENT: &str = "collab_spawn";
const SETUP_SMOKE_TEST_METADATA_KEY: &str = "web2codex_setup_smoke_test";

#[derive(Clone, Debug, PartialEq, Eq)]
struct AgentRequestIdentity {
    thread_id: String,
    name: String,
    agent_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AgentBrowserTab {
    tab_index: usize,
    name: String,
    agent_path: Option<String>,
    conversation_epoch: Option<String>,
    active_request_id: Option<String>,
    close_when_idle: bool,
}

#[derive(Default)]
struct AgentTabRegistry {
    by_thread: HashMap<String, AgentBrowserTab>,
    request_to_thread: HashMap<String, String>,
    pending_close: HashMap<String, Instant>,
    pending_close_by_path: HashMap<String, Instant>,
    setup_smoke_tab: Option<AgentBrowserTab>,
    primary_response_tabs: HashMap<String, usize>,
    primary_by_thread: HashMap<String, AgentBrowserTab>,
    primary_request_to_thread: HashMap<String, String>,
    interrupted_primary_chats: HashMap<String, InterruptedPrimaryChat>,
    response_request_ids: HashMap<usize, Arc<Mutex<Option<String>>>>,
    retired_primary_response_ids: HashMap<usize, Arc<Mutex<Option<String>>>>,
    reusable_primary_response_tab: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgentTabCloseStatus {
    Deferred,
    Ready(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgentTabCloseOutcome {
    NotFound,
    Deferred,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BrowserActivationIntent {
    UserAction,
    BackgroundWork,
    BackgroundTabSelection,
}

fn should_activate_browser(intent: BrowserActivationIntent) -> bool {
    matches!(intent, BrowserActivationIntent::UserAction)
}

fn should_apply_background_focus_guard(window_is_foreground: bool) -> bool {
    !window_is_foreground
}

const WS_EX_NOACTIVATE_STYLE: isize = 0x08000000;

fn extended_style_with_no_activate(style: isize, prevent_activation: bool) -> isize {
    if prevent_activation {
        style | WS_EX_NOACTIVATE_STYLE
    } else {
        style & !WS_EX_NOACTIVATE_STYLE
    }
}

#[cfg(target_os = "windows")]
fn set_window_no_activate(window: &Window, prevent_activation: bool) -> anyhow::Result<()> {
    use std::ffi::c_void;
    use std::ptr::null_mut;

    use tao::platform::windows::WindowExtWindows;
    use windows_sys::Win32::Foundation::{GetLastError, SetLastError};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos,
    };

    let hwnd = window.hwnd() as *mut c_void;
    let current_style = unsafe {
        SetLastError(0);
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let error = GetLastError();
        anyhow::ensure!(
            style != 0 || error == 0,
            "could not read the browser window style: Windows error {error}"
        );
        style
    };
    let updated_style = extended_style_with_no_activate(current_style, prevent_activation);
    if updated_style == current_style {
        return Ok(());
    }

    unsafe {
        SetLastError(0);
        let previous = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, updated_style);
        let error = GetLastError();
        anyhow::ensure!(
            previous != 0 || error == 0,
            "could not update the browser window style: Windows error {error}"
        );
    }

    let applied = unsafe {
        SetWindowPos(
            hwnd,
            null_mut(),
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER,
        )
    };
    anyhow::ensure!(
        applied != 0,
        "could not apply the browser window activation style"
    );
    Ok(())
}

#[cfg(target_os = "windows")]
fn browser_window_is_foreground(window: &Window) -> bool {
    use std::ffi::c_void;

    use tao::platform::windows::WindowExtWindows;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    unsafe { GetForegroundWindow() == window.hwnd() as *mut c_void }
}

#[cfg(not(target_os = "windows"))]
fn browser_window_is_foreground(_window: &Window) -> bool {
    false
}

#[cfg(not(target_os = "windows"))]
fn set_window_no_activate(_window: &Window, _prevent_activation: bool) -> anyhow::Result<()> {
    Ok(())
}

fn should_select_response_tab(
    intent: BrowserActivationIntent,
    active_tab: usize,
    response_tab: usize,
) -> bool {
    intent == BrowserActivationIntent::BackgroundTabSelection
        || should_activate_browser(intent)
        || active_tab == response_tab
}

fn response_tab_starts_in_viewport(
    intent: BrowserActivationIntent,
    active_tab: usize,
    response_tab: usize,
) -> bool {
    should_select_response_tab(intent, active_tab, response_tab)
}

fn session_disconnect_deadline(
    now: Instant,
    clients: usize,
    session_owned: bool,
) -> Option<Instant> {
    (clients == 0 && session_owned).then_some(now + SESSION_DISCONNECT_GRACE)
}

fn activate_browser_window(window: &Window, intent: BrowserActivationIntent) {
    if should_activate_browser(intent) {
        if let Err(error) = set_window_no_activate(window, false) {
            eprintln!("Could not enable browser window activation: {error:#}");
        }
        window.set_visible(true);
        window.set_focus();
    }
}

fn release_focus_guard_request(request_id: &str, guarded_requests: &mut HashSet<String>) -> bool {
    guarded_requests.remove(request_id) && guarded_requests.is_empty()
}

fn release_background_focus_guard(
    request_id: &str,
    window: &Window,
    guarded_requests: &mut HashSet<String>,
) {
    if release_focus_guard_request(request_id, guarded_requests) {
        if let Err(error) = set_window_no_activate(window, false) {
            eprintln!("Could not release browser focus guard: {error:#}");
        }
    }
}

fn activate_browser_tab(window: &Window, view: &WebView, intent: BrowserActivationIntent) {
    let _ = view.set_visible(true);
    if should_activate_browser(intent) {
        activate_browser_window(window, intent);
        let _ = view.focus();
    }
}

impl AgentTabRegistry {
    fn prune_pending_closes(&mut self) {
        let now = Instant::now();
        self.pending_close
            .retain(|_, requested_at| now.duration_since(*requested_at) < PENDING_AGENT_CLOSE_TTL);
        self.pending_close_by_path
            .retain(|_, requested_at| now.duration_since(*requested_at) < PENDING_AGENT_CLOSE_TTL);
    }

    fn tab_index(&self, thread_id: &str) -> Option<usize> {
        self.by_thread.get(thread_id).map(|tab| tab.tab_index)
    }

    fn tab_for_thread(&self, thread_id: &str) -> Option<&AgentBrowserTab> {
        self.by_thread.get(thread_id)
    }

    fn thread_for_request(&self, request_id: &str) -> Option<&str> {
        self.request_to_thread.get(request_id).map(String::as_str)
    }

    fn request_close_for_request(&mut self, request_id: &str) -> Option<AgentTabCloseStatus> {
        let thread_id = self.thread_for_request(request_id)?.to_owned();
        Some(self.request_close_internal(&thread_id, None, /*close_active_now*/ false))
    }

    fn setup_smoke_tab_index(&self) -> Option<usize> {
        self.setup_smoke_tab.as_ref().map(|tab| tab.tab_index)
    }

    fn tab_index_for_request(&self, request_id: &str) -> Option<usize> {
        if let Some(index) = self.primary_response_tabs.get(request_id) {
            return Some(*index);
        }
        if let Some(thread_id) = self.thread_for_request(request_id) {
            return self.tab_index(thread_id);
        }
        self.setup_smoke_tab
            .as_ref()
            .filter(|tab| tab.active_request_id.as_deref() == Some(request_id))
            .map(|tab| tab.tab_index)
    }

    fn primary_tab_for_thread(&self, thread_id: &str) -> Option<&AgentBrowserTab> {
        self.primary_by_thread.get(thread_id)
    }

    fn note_interrupted_primary(&mut self, request_id: &str) {
        self.interrupted_primary_chats
            .retain(|_, interrupted| interrupted.interrupted_at.elapsed() < INTERRUPTED_PRIMARY_CHAT_TTL);
        let Some(thread_id) = self.primary_request_to_thread.get(request_id).cloned() else {
            return;
        };
        let Some(tab) = self.primary_by_thread.get(&thread_id) else {
            return;
        };
        if tab.active_request_id.as_deref() != Some(request_id) {
            return;
        }
        self.interrupted_primary_chats.insert(
            thread_id,
            InterruptedPrimaryChat {
                tab_index: tab.tab_index,
                conversation_epoch: tab.conversation_epoch.clone(),
                request_id_state: self
                    .response_request_ids
                    .get(&tab.tab_index)
                    .or_else(|| self.retired_primary_response_ids.get(&tab.tab_index))
                    .cloned(),
                interrupted_at: Instant::now(),
            },
        );
    }

    fn take_interrupted_primary(
        &mut self,
        thread_id: &str,
        conversation_epoch: Option<&str>,
    ) -> Option<Arc<Mutex<Option<String>>>> {
        let Some(interrupted) = self.interrupted_primary_chats.remove(thread_id) else {
            return None;
        };
        let tab_still_matches = self.primary_by_thread.get(thread_id).is_some_and(|tab| {
            tab.tab_index == interrupted.tab_index
                && tab.conversation_epoch.as_deref() == interrupted.conversation_epoch.as_deref()
        });
        (tab_still_matches
            && interrupted.interrupted_at.elapsed() < INTERRUPTED_PRIMARY_CHAT_TTL
            && interrupted.conversation_epoch.as_deref() == conversation_epoch)
            .then_some(interrupted.request_id_state)
            .flatten()
    }

    fn stale_response_request(
        &self,
        request_id: &str,
        conversation_id: Option<&str>,
        agent: Option<&AgentRequestIdentity>,
        setup_smoke_test: bool,
        broker: &TurnBroker,
    ) -> Option<String> {
        let tab = if let Some(agent) = agent {
            self.tab_for_thread(&agent.thread_id)
        } else if setup_smoke_test {
            self.setup_smoke_tab.as_ref()
        } else {
            conversation_id.and_then(|thread_id| self.primary_tab_for_thread(thread_id))
        }?;
        let previous = tab.active_request_id.as_deref()?;
        (previous != request_id && broker.snapshot(previous)["active"].as_bool() == Some(false))
            .then(|| previous.to_owned())
    }

    fn primary_thread_for_request(&self, request_id: &str) -> Option<&str> {
        self.primary_request_to_thread
            .get(request_id)
            .map(String::as_str)
    }

    fn require_fresh_response_view(&mut self, request_id: &str) {
        let Some(tab_index) = self.tab_index_for_request(request_id) else {
            return;
        };
        if let Some(state) = self.response_request_ids.remove(&tab_index)
            && let Ok(mut current) = state.lock()
        {
            *current = None;
            if self.primary_request_to_thread.contains_key(request_id) {
                self.retired_primary_response_ids.insert(tab_index, state.clone());
            }
        }
    }

    fn response_request_id_state(
        &self,
        tab_index: usize,
    ) -> Option<Arc<Mutex<Option<String>>>> {
        self.response_request_ids.get(&tab_index).cloned()
    }

    fn bind_response_request_id_state(
        &mut self,
        tab_index: usize,
        request_id_state: Arc<Mutex<Option<String>>>,
    ) {
        if let Some(retired) = self.retired_primary_response_ids.remove(&tab_index)
            && !Arc::ptr_eq(&retired, &request_id_state)
            && let Ok(mut current) = retired.lock()
        {
            *current = None;
        }
        if let Some(previous) = self.response_request_ids.insert(tab_index, request_id_state.clone())
            && !Arc::ptr_eq(&previous, &request_id_state)
            && let Ok(mut current) = previous.lock()
        {
            *current = None;
        }
    }

    fn clear_response_request_id(
        &self,
        tab_index: usize,
        request_id: &str,
    ) {
        if let Some(state) = self.response_request_id_state(tab_index)
            && let Ok(mut state) = state.lock()
            && state.as_deref() == Some(request_id)
        {
            *state = None;
        }
    }

    fn reusable_primary_response_tab(&self) -> Option<usize> {
        self.primary_response_tabs
            .is_empty()
            .then_some(self.reusable_primary_response_tab)
            .flatten()
            .filter(|index| !self.primary_by_thread.values().any(|tab| tab.tab_index == *index))
    }

    fn bind_primary_response_request(&mut self, request_id: &str, tab_index: usize) {
        self.primary_response_tabs
            .insert(request_id.to_owned(), tab_index);
    }

    fn bind_primary_thread_request(
        &mut self,
        thread_id: &str,
        request_id: &str,
        tab_index: usize,
        conversation_epoch: Option<String>,
    ) {
        if self.reusable_primary_response_tab == Some(tab_index) {
            self.reusable_primary_response_tab = None;
        }
        self.primary_response_tabs
            .insert(request_id.to_owned(), tab_index);
        self.primary_request_to_thread
            .insert(request_id.to_owned(), thread_id.to_owned());
        self.primary_by_thread.insert(
            thread_id.to_owned(),
            AgentBrowserTab {
                tab_index,
                name: "ChatGPT".to_owned(),
                agent_path: None,
                conversation_epoch,
                active_request_id: Some(request_id.to_owned()),
                close_when_idle: false,
            },
        );
    }

    fn is_setup_smoke_request(&self, request_id: &str) -> bool {
        self.setup_smoke_tab
            .as_ref()
            .is_some_and(|tab| tab.active_request_id.as_deref() == Some(request_id))
    }

    fn bind_request(
        &mut self,
        identity: &AgentRequestIdentity,
        request_id: &str,
        tab_index: usize,
    ) {
        self.bind_request_with_epoch(identity, request_id, tab_index, None);
    }

    fn bind_request_with_epoch(
        &mut self,
        identity: &AgentRequestIdentity,
        request_id: &str,
        tab_index: usize,
        conversation_epoch: Option<String>,
    ) {
        self.prune_pending_closes();
        let pending_thread_close = self.pending_close.remove(&identity.thread_id).is_some();
        let pending_path_close = identity
            .agent_path
            .as_deref()
            .is_some_and(|path| self.pending_close_by_path.remove(path).is_some());
        let close_when_idle = pending_thread_close || pending_path_close;
        if let Some(previous_request) = self
            .by_thread
            .get(&identity.thread_id)
            .and_then(|tab| tab.active_request_id.as_ref())
        {
            self.request_to_thread.remove(previous_request);
        }
        self.by_thread.insert(
            identity.thread_id.clone(),
            AgentBrowserTab {
                tab_index,
                name: identity.name.clone(),
                agent_path: identity.agent_path.clone(),
                conversation_epoch,
                active_request_id: Some(request_id.to_owned()),
                close_when_idle,
            },
        );
        self.request_to_thread
            .insert(request_id.to_owned(), identity.thread_id.clone());
    }

    fn bind_setup_smoke_request(&mut self, request_id: &str, tab_index: usize) {
        self.setup_smoke_tab = Some(AgentBrowserTab {
            tab_index,
            name: "Setup · smoke test".to_owned(),
            agent_path: None,
            conversation_epoch: None,
            active_request_id: Some(request_id.to_owned()),
            close_when_idle: false,
        });
    }

    fn release_request(&mut self, request_id: &str) -> Vec<usize> {
        let mut tabs_to_close = Vec::new();
        if let Some(index) = self.primary_response_tabs.remove(request_id) {
            self.clear_response_request_id(index, request_id);
            if let Some(thread_id) = self.primary_request_to_thread.remove(request_id) {
                if let Some(tab) = self.primary_by_thread.get_mut(&thread_id)
                    && tab.active_request_id.as_deref() == Some(request_id)
                {
                    tab.active_request_id = None;
                } else if self
                    .primary_by_thread
                    .get(&thread_id)
                    .is_some_and(|tab| tab.tab_index != index)
                {
                    // A newer response for this Codex thread may have moved to
                    // a fresh Temporary Chat while this response was still
                    // active. Retire the old view only after its own observer
                    // releases it, leaving the newer tab untouched.
                    tabs_to_close.push(index);
                }
                return tabs_to_close;
            }
            if self.reusable_primary_response_tab.is_none() {
                self.reusable_primary_response_tab = Some(index);
            } else if self.reusable_primary_response_tab != Some(index) {
                tabs_to_close.push(index);
            }
            return tabs_to_close;
        }
        if let Some(thread_id) = self.request_to_thread.remove(request_id) {
            let closed_tab = self.by_thread.get_mut(&thread_id).and_then(|tab| {
                if tab.active_request_id.as_deref() != Some(request_id) {
                    return None;
                }
                tab.active_request_id = None;
                Some((tab.tab_index, tab.close_when_idle))
            });
            if let Some((tab_index, close_when_idle)) = closed_tab {
                self.clear_response_request_id(tab_index, request_id);
                if close_when_idle {
                    tabs_to_close.push(tab_index);
                }
            }
        }
        let setup_tab_index = self.setup_smoke_tab.as_mut().and_then(|tab| {
            if tab.active_request_id.as_deref() != Some(request_id) {
                return None;
            }
            tab.active_request_id = None;
            Some(tab.tab_index)
        });
        if let Some(tab_index) = setup_tab_index {
            self.clear_response_request_id(tab_index, request_id);
        }
        tabs_to_close
    }

    fn request_close(&mut self, thread_id: &str, agent_path: Option<&str>) -> AgentTabCloseStatus {
        self.request_close_internal(thread_id, agent_path, /*close_active_now*/ true)
    }

    fn request_close_internal(
        &mut self,
        thread_id: &str,
        agent_path: Option<&str>,
        close_active_now: bool,
    ) -> AgentTabCloseStatus {
        self.prune_pending_closes();
        let matched_thread_id = self
            .by_thread
            .contains_key(thread_id)
            .then_some(thread_id.to_owned())
            .or_else(|| {
                let agent_path = agent_path?;
                self.by_thread
                    .iter()
                    .find(|(_, tab)| tab.agent_path.as_deref() == Some(agent_path))
                    .map(|(thread_id, _)| thread_id.clone())
            });
        let Some(matched_thread_id) = matched_thread_id else {
            self.pending_close
                .insert(thread_id.to_owned(), Instant::now());
            if let Some(agent_path) = agent_path.filter(|path| !path.trim().is_empty()) {
                self.pending_close_by_path
                    .insert(agent_path.to_owned(), Instant::now());
            }
            return AgentTabCloseStatus::Deferred;
        };
        let Some(tab) = self.by_thread.get_mut(&matched_thread_id) else {
            return AgentTabCloseStatus::Deferred;
        };
        tab.close_when_idle = true;
        if tab.active_request_id.is_some() && !close_active_now {
            AgentTabCloseStatus::Deferred
        } else {
            // Explicit close-agent commands are issued only after the host reports
            // a terminal agent status. A stale observer must not strand its tab.
            AgentTabCloseStatus::Ready(tab.tab_index)
        }
    }

    fn close_tab(&mut self, tab_index: usize) -> Vec<String> {
        let removed_threads = self
            .by_thread
            .iter()
            .filter(|(_, tab)| tab.tab_index == tab_index)
            .map(|(thread_id, _)| thread_id.clone())
            .collect::<Vec<_>>();
        let removed_primary_threads = self
            .primary_by_thread
            .iter()
            .filter(|(_, tab)| tab.tab_index == tab_index)
            .map(|(thread_id, _)| thread_id.clone())
            .collect::<Vec<_>>();
        let canceled_requests = removed_threads
            .iter()
            .filter_map(|thread_id| {
                self.by_thread
                    .get(thread_id)
                    .and_then(|tab| tab.active_request_id.clone())
            })
            .collect::<Vec<_>>();
        let mut canceled_requests = canceled_requests;
        canceled_requests.extend(removed_primary_threads.iter().filter_map(|thread_id| {
            self.primary_by_thread
                .get(thread_id)
                .and_then(|tab| tab.active_request_id.clone())
        }));
        for thread_id in &removed_threads {
            if let Some(tab) = self.by_thread.remove(thread_id)
                && let Some(agent_path) = tab.agent_path
            {
                self.pending_close_by_path.remove(&agent_path);
            }
            self.pending_close.remove(thread_id);
        }
        for thread_id in &removed_primary_threads {
            if let Some(tab) = self.primary_by_thread.remove(thread_id)
                && let Some(request_id) = tab.active_request_id
            {
                self.primary_request_to_thread.remove(&request_id);
                self.primary_response_tabs.remove(&request_id);
            }
        }
        self.interrupted_primary_chats.retain(|thread_id, interrupted| {
            !removed_primary_threads.contains(thread_id) && interrupted.tab_index != tab_index
        });
        for interrupted in self.interrupted_primary_chats.values_mut() {
            if interrupted.tab_index > tab_index {
                interrupted.tab_index -= 1;
            }
        }
        canceled_requests.extend(
            self.primary_response_tabs
                .iter()
                .filter(|(_, index)| **index == tab_index)
                .map(|(request_id, _)| request_id.clone()),
        );
        self.primary_response_tabs
            .retain(|_, index| *index != tab_index);
        if self.reusable_primary_response_tab == Some(tab_index) {
            self.reusable_primary_response_tab = None;
        }
        if let Some(state) = self.response_request_ids.remove(&tab_index)
            && let Ok(mut current) = state.lock()
        {
            *current = None;
        }
        if let Some(state) = self.retired_primary_response_ids.remove(&tab_index)
            && let Ok(mut current) = state.lock()
        {
            *current = None;
        }
        for index in self.primary_response_tabs.values_mut() {
            if *index > tab_index {
                *index -= 1;
            }
        }
        if let Some(index) = self.reusable_primary_response_tab.as_mut()
            && *index > tab_index
        {
            *index -= 1;
        }
        let request_id_states = std::mem::take(&mut self.response_request_ids);
        self.response_request_ids = request_id_states
            .into_iter()
            .map(|(index, state)| (if index > tab_index { index - 1 } else { index }, state))
            .collect();
        let retired_request_id_states = std::mem::take(&mut self.retired_primary_response_ids);
        self.retired_primary_response_ids = retired_request_id_states
            .into_iter()
            .map(|(index, state)| (if index > tab_index { index - 1 } else { index }, state))
            .collect();
        if self
            .setup_smoke_tab
            .as_ref()
            .is_some_and(|tab| tab.tab_index == tab_index)
        {
            if let Some(request_id) = self
                .setup_smoke_tab
                .as_ref()
                .and_then(|tab| tab.active_request_id.clone())
            {
                canceled_requests.push(request_id);
            }
            self.setup_smoke_tab = None;
        }
        self.request_to_thread
            .retain(|_, thread_id| self.by_thread.contains_key(thread_id));
        self.primary_request_to_thread.retain(|_, thread_id| {
            self.primary_by_thread.contains_key(thread_id)
        });
        for tab in self.primary_by_thread.values_mut() {
            if tab.tab_index > tab_index {
                tab.tab_index -= 1;
            }
        }
        for tab in self.by_thread.values_mut() {
            if tab.tab_index > tab_index {
                tab.tab_index -= 1;
            }
        }
        if let Some(tab) = self.setup_smoke_tab.as_mut()
            && tab.tab_index > tab_index
        {
            tab.tab_index -= 1;
        }
        canceled_requests
    }

    #[cfg(test)]
    fn label_at(&self, tab_index: usize) -> Option<&str> {
        self.by_thread
            .values()
            .find(|tab| tab.tab_index == tab_index)
            .map(|tab| tab.name.as_str())
            .or_else(|| {
                self.setup_smoke_tab
                    .as_ref()
                    .filter(|tab| tab.tab_index == tab_index)
                    .map(|tab| tab.name.as_str())
            })
    }
}

#[derive(Clone, Debug, serde::Serialize)]
struct MediaAttachment {
    name: String,
    mime: String,
    data_url: String,
}

struct ParsedHttpRequest {
    method: String,
    path: String,
    authorization: String,
    content_type: Option<String>,
    body: Vec<u8>,
}

fn main() -> anyhow::Result<()> {
    if !env::args().any(|argument| argument == "--background") {
        anyhow::bail!("Web2Codex starts only when requested by its terminal command");
    }

    run_background()
}

fn run_background() -> anyhow::Result<()> {
    let data_dir = data_directory()?;
    fs::create_dir_all(&data_dir).with_context(|| {
        format!(
            "failed to create browser data directory {}",
            data_dir.display()
        )
    })?;
    restrict_permissions(&data_dir)?;

    let state_path = data_dir.join("browser-state.json");
    let lock_path = data_dir.join("browser.lock");
    if send_control(&state_path, "/status").is_ok() {
        return Ok(());
    }

    let event_loop = EventLoopBuilder::<BrowserEvent>::with_user_event().build();
    #[cfg(target_os = "macos")]
    let event_loop = {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        let mut event_loop = event_loop;
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
        event_loop.set_activate_ignoring_other_apps(false);
        event_loop
    };
    let window = WindowBuilder::new()
        .with_title("Web2Codex")
        .with_inner_size(LogicalSize::new(1200.0, 800.0))
        // `with_focused(false)` only clears the initial focus request. The
        // background controller must also be non-focusable from creation so
        // Windows never promotes it while the MCP tunnel starts.
        .with_focusable(false)
        .with_focused(false)
        .with_visible(false)
        .build(&event_loop)
        .context("failed to initialize the hidden Web2Codex window")?;
    let tray_icon = create_tray_icon(event_loop.create_proxy())
        .context("failed to create the Web2Codex tray icon; a system tray host is required")?;

    let lock_file = acquire_lock(&lock_path, &state_path)?;
    let listener = TcpListener::bind("127.0.0.1:0")
        .context("failed to bind the local Web2Codex control socket")?;
    let state = BrowserState {
        address: listener.local_addr()?,
        token: Uuid::new_v4().to_string(),
        bridge_protocol: 7,
    };
    write_state(&state_path, &state)?;

    let event_proxy = event_loop.create_proxy();
    let broker = Arc::new(TurnBroker::default());
    let control_event_proxy = event_proxy.clone();
    let control_broker = Arc::clone(&broker);
    let control_account_capabilities_path = data_dir.join("account-capabilities.json");
    thread::Builder::new()
        .name("codex-web-control".to_string())
        .spawn(move || {
            serve_control_requests(
                listener,
                state.token,
                control_account_capabilities_path,
                control_event_proxy,
                control_broker,
            )
        })
        .context("failed to start the Web2Codex control listener")?;

    run_event_loop(
        event_loop,
        window,
        data_dir.join("profile"),
        data_dir.join("selected-account-tier.json"),
        data_dir.join("account-capabilities.json"),
        data_dir.join("usage-stats.json"),
        state_path,
        lock_file,
        broker,
        Some(tray_icon),
    )
}

fn create_tray_icon(proxy: EventLoopProxy<BrowserEvent>) -> anyhow::Result<TrayIcon> {
    let open = MenuItem::with_id("web2codex.open", "Open ChatGPT", true, None);
    let sign_in = MenuItem::with_id("web2codex.sign-in", "Sign in / switch account", true, None);
    let separator = PredefinedMenuItem::separator();
    let quit = MenuItem::with_id("web2codex.quit", "Quit Web2Codex", true, None);
    let menu = Menu::with_items(&[&open, &sign_in, &separator, &quit])
        .context("failed to create the Web2Codex tray menu")?;

    let mut rgba = vec![0_u8; 16 * 16 * 4];
    for y in 2..14 {
        for x in 2..14 {
            let offset = (y * 16 + x) * 4;
            rgba[offset..offset + 4].copy_from_slice(&[76, 125, 255, 255]);
        }
    }
    // A small white W remains legible in both light and dark system trays.
    for (x, y) in [
        (4, 5),
        (4, 6),
        (4, 7),
        (4, 8),
        (4, 9),
        (5, 10),
        (6, 9),
        (7, 8),
        (8, 9),
        (9, 10),
        (10, 9),
        (11, 8),
        (11, 7),
        (11, 6),
        (11, 5),
    ] {
        let offset = (y * 16 + x) * 4;
        rgba[offset..offset + 4].copy_from_slice(&[255, 255, 255, 255]);
    }
    let icon = Icon::from_rgba(rgba, 16, 16).context("failed to create the Web2Codex tray icon")?;
    let tray_icon = TrayIconBuilder::new()
        .with_tooltip("Web2Codex")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .build()
        .context("failed to register the Web2Codex system tray icon")?;

    let open_id = open.id().clone();
    let sign_in_id = sign_in.id().clone();
    let quit_id = quit.id().clone();
    let selected_account_tier_path = data_directory()?.join("selected-account-tier.json");
    thread::Builder::new()
        .name("codex-web-tray-menu".to_string())
        .spawn(move || {
            while let Ok(event) = MenuEvent::receiver().recv() {
                let browser_event = if event.id == open_id {
                    let page = if selected_account_tier_path.is_file() {
                        BrowserPage::TemporaryChat
                    } else {
                        BrowserPage::AccountChooser
                    };
                    BrowserEvent::Open(page, false, None)
                } else if event.id == sign_in_id {
                    BrowserEvent::Open(BrowserPage::AccountChooser, false, None)
                } else if event.id == quit_id {
                    BrowserEvent::Stop(None)
                } else {
                    continue;
                };
                if proxy.send_event(browser_event).is_err() {
                    break;
                }
            }
        })
        .context("failed to listen for Web2Codex tray menu actions")?;

    Ok(tray_icon)
}

fn run_event_loop(
    event_loop: tao::event_loop::EventLoop<BrowserEvent>,
    window: Window,
    profile_dir: PathBuf,
    selected_account_tier_path: PathBuf,
    account_capabilities_path: PathBuf,
    usage_stats_path: PathBuf,
    state_path: PathBuf,
    lock_file: PathBuf,
    broker: Arc<TurnBroker>,
    tray_icon: Option<TrayIcon>,
) -> ! {
    let window_id = window.id();
    let chrome_proxy = event_loop.create_proxy();
    let chrome = WebViewBuilder::new()
        .with_html(include_str!("browser_chrome.html"))
        .with_bounds(chrome_bounds(&window))
        .with_ipc_handler(move |request| {
            let _ = chrome_proxy.send_event(BrowserEvent::Chrome(request.body().to_owned()));
        })
        .build_as_child(&window)
        .expect("failed to initialize browser toolbar");
    let mut web_context: Option<WebContext> = None;
    let mut webview: Option<WebView> = None;
    let mut main_tab_label: Option<String> = None;
    let mut extra_tabs: Vec<WebView> = Vec::new();
    let mut extra_tab_labels: Vec<Option<String>> = Vec::new();
    let mut active_tab: usize = 0;
    let mut browser_page: Option<BrowserPage> = None;
    let mut clients = 0_usize;
    // A launching terminal may disappear before it gets its first socket.
    // Standalone "web start" waits for a user to attach or stop it.
    let mut session_owned = env::args().any(|arg| arg == "--session-owned");
    let mut idle_deadline = session_owned.then(|| Instant::now() + Duration::from_secs(10 * 60));
    // Turns share sign-in storage, never a composer or a capability token.
    let mut response_views: HashMap<String, WebView> = HashMap::new();
    let mut agent_tabs = AgentTabRegistry::default();
    let mut pending_responses: HashMap<String, PendingResponsesRequest> = HashMap::new();
    let mut background_focus_guard_requests = HashSet::new();
    let event_proxy = event_loop.create_proxy();

    event_loop.run(move |event, _, control_flow| {
        let now = Instant::now();
        for (request_id, pending) in &mut pending_responses {
            if pending.bridge_ready_deadline.is_some_and(|deadline| now >= deadline) {
                pending.bridge_ready_deadline = None;
                agent_tabs.require_fresh_response_view(request_id);
                release_background_focus_guard(
                    request_id,
                    &window,
                    &mut background_focus_guard_requests,
                );
                broker.complete(request_id, None, Err("ChatGPT response bridge did not acknowledge the request in time. The next turn will open a fresh Temporary Chat.".into()));
            }
        }
        *control_flow = response_bridge_wakeup(idle_deadline, &pending_responses);
        let mut stop_reply = None;

        // The broker can finish or retire a turn before its queued UI release
        // is processed. Reconcile that old binding before opening the next
        // request for the same conversation, using the normal release path.
        let request_to_release = match &event {
            Event::UserEvent(BrowserEvent::ReleaseResponses(request_id)) => Some(request_id.clone()),
            Event::UserEvent(BrowserEvent::ResponsesRequest { request_id, conversation_id, conversation_epoch, interruption_prompt, agent, setup_smoke_test, .. }) => {
                if broker.snapshot(request_id)["active"].as_bool() != Some(true) {
                    return;
                }
                let stale = agent_tabs.stale_response_request(
                    request_id,
                    conversation_id.as_deref(),
                    agent.as_ref(),
                    *setup_smoke_test,
                    &broker,
                );
                if let Some(previous_request_id) = stale.as_deref()
                    && interruption_prompt.is_some()
                    && let Some(thread_id) = conversation_id.as_deref()
                    && agent_tabs
                        .primary_tab_for_thread(thread_id)
                        .is_some_and(|tab| {
                            tab.conversation_epoch.as_deref() == conversation_epoch.as_deref()
                        })
                    && pending_responses.contains_key(previous_request_id)
                {
                    agent_tabs.note_interrupted_primary(previous_request_id);
                    if let Some(view) = response_view_for_request(
                        previous_request_id,
                        &response_views,
                        &agent_tabs,
                        &webview,
                        &extra_tabs,
                    ) {
                        let previous_request_id = serde_json::to_string(previous_request_id)
                            .unwrap_or_else(|_| "null".to_owned());
                        let _ = view.evaluate_script(&format!(
                            "window.__web2codexCancel?.({previous_request_id});"
                        ));
                    }
                }
                stale
            }
            _ => None,
        };
        if let Some(request_id) = request_to_release {
            let _ = agent_tabs.request_close_for_request(&request_id);
            if let Some(view) = response_view_for_request(
                &request_id,
                &response_views,
                &agent_tabs,
                &webview,
                &extra_tabs,
            ) {
                let _ = view.evaluate_script(&response_signal_script(&request_id, "window.__web2codexCompleted=true;"));
            }
            // Preserve a cancelled primary chat when the next request is an
            // interruption from the same Codex thread and context epoch.
            let preserve_interrupted_primary = agent_tabs
                .primary_thread_for_request(&request_id)
                .and_then(|thread_id| agent_tabs.interrupted_primary_chats.get(thread_id))
                .is_some();
            if pending_responses.contains_key(&request_id) && !preserve_interrupted_primary {
                agent_tabs.require_fresh_response_view(&request_id);
            }
            release_background_focus_guard(
                &request_id,
                &window,
                &mut background_focus_guard_requests,
            );
            response_views.remove(&request_id);
            for index in agent_tabs.release_request(&request_id) {
                let _ = close_browser_tab(
                    index,
                    &window,
                    &mut background_focus_guard_requests,
                    &mut webview,
                    &mut main_tab_label,
                    &mut browser_page,
                    &mut extra_tabs,
                    &mut extra_tab_labels,
                    &mut active_tab,
                    &mut agent_tabs,
                    &broker,
                    &mut pending_responses,
                    &mut response_views,
                    &chrome,
                );
            }
            if let Some(pending) = pending_responses.remove(&request_id) {
                let milliseconds = u64::try_from(pending.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
                let _ = record_local_usage(&usage_stats_path, &pending.model, pending.input_characters, None, milliseconds);
            }
            if response_views.is_empty() && webview.is_none() { web_context = None; }
        }
        match event {
            Event::UserEvent(BrowserEvent::ClientConnected(reply)) => {
                if reply.send(()).is_ok() {
                    clients += 1;
                    // Once Web2Codex attaches, the browser belongs to that app session.
                    // Close it after the final lease ends, even if `web start` had
                    // previously left the helper persistent in the background.
                    session_owned = true;
                    idle_deadline = None;
                }
            }
            Event::UserEvent(BrowserEvent::ClientDisconnected) => {
                clients = clients.saturating_sub(1);
                idle_deadline = session_disconnect_deadline(Instant::now(), clients, session_owned);
            }
            Event::UserEvent(BrowserEvent::Open(page, reuse, reply)) => {
                if webview.is_some() && (reuse || page == BrowserPage::TemporaryChat) {
                    activate_browser_window(&window, BrowserActivationIntent::UserAction);
                    if let Some(reply) = reply {
                        let _ = reply.send(Ok(()));
                    }
                    return;
                }
                if let Err(error) = prepare_profile_directory(&profile_dir) {
                    if let Some(reply) = reply {
                        let _ = reply.send(Err(error.to_string()));
                    } else {
                        eprintln!("Could not prepare the Web2Codex browser profile: {error:#}");
                    }
                    return;
                }
                // The picker snapshot belongs to the signed-in browser profile.
                // Keep it when reopening a Temporary Chat so the CLI can supply
                // the known model catalog immediately; the page probe refreshes it.
                if page == BrowserPage::AccountChooser {
                    let _ = fs::remove_file(&account_capabilities_path);
                }
                webview = None;
                for tab in &extra_tabs { let _ = tab.set_visible(false); }
                active_tab = 0;
                let context = web_context.get_or_insert_with(|| WebContext::new(Some(profile_dir.clone())));
                main_tab_label = None;
                match build_webview(&window, context, page, event_proxy.clone(), None, false, None) {
                    Ok(created) => { webview = Some(created); browser_page = Some(page); }
                    Err(error) => {
                        if let Some(reply) = reply {
                            let _ = reply.send(Err(format!("{error:#}")));
                        } else {
                            eprintln!("Could not open the Web2Codex browser: {error:#}");
                        }
                        return;
                    }
                }
                activate_browser_window(&window, BrowserActivationIntent::UserAction);
                if clients == 0 && session_owned {
                    idle_deadline = Some(Instant::now() + Duration::from_secs(10 * 60));
                }
                if let Some(reply) = reply {
                    let _ = reply.send(Ok(()));
                }
            }
            Event::UserEvent(BrowserEvent::NewTab(url)) => {
                if extra_tabs.len() >= 31 || !url::Url::parse(&url).is_ok_and(|url| matches!(url.scheme(), "http" | "https")) { return; }
                if webview.is_some() {
                    if let Some(view) = active_browser_view(&webview, &extra_tabs, active_tab) { let _ = view.set_visible(false); }
                    let context = web_context.get_or_insert_with(|| WebContext::new(Some(profile_dir.clone())));
                    let proxy = event_proxy.clone();
                    match WebViewBuilder::new_with_web_context(context)
                        .with_bounds(content_bounds(&window, false))
                        .with_url(&url)
                        .with_new_window_req_handler(move |url, _| { let _ = proxy.send_event(BrowserEvent::NewTab(url)); wry::NewWindowResponse::Deny })
                        .build_as_child(&window) {
                    Ok(view) => { extra_tabs.push(view); extra_tab_labels.push(None); active_tab = extra_tabs.len(); }
                            Err(error) => { eprintln!("Could not open tab: {error}"); if let Some(view) = active_browser_view(&webview, &extra_tabs, active_tab) { let _ = view.set_visible(true); } }
                        }
                    sync_browser_chrome(
                        &chrome,
                        &webview,
                        &extra_tabs,
                        active_tab,
                        main_tab_label.as_deref(),
                        &extra_tab_labels,
                    );
                }
            }
            Event::UserEvent(BrowserEvent::BackgroundTab(url, reply)) => {
                if extra_tabs.len() >= 31 { let _ = reply.send(Err("The browser has reached its 32 tab limit".to_owned())); return; }
                let context = web_context.get_or_insert_with(|| WebContext::new(Some(profile_dir.clone())));
                let created = if webview.is_none() && url == CHATGPT_TEMPORARY_CHAT_URL {
                    build_webview(
                        &window,
                        context,
                        BrowserPage::TemporaryChat,
                        event_proxy.clone(),
                        None,
                        false,
                        None,
                    )
                } else {
                    let proxy = event_proxy.clone();
                    let view_id = Uuid::new_v4().to_string();
                    let ipc_view_id = view_id.clone();
                    let builder = WebViewBuilder::new_with_web_context(context).with_id(&view_id);
                    let builder = if should_probe_chatgpt_url(&url) {
                        let capability_proxy = event_proxy.clone();
                        builder
                            .with_initialization_script("window.__web2codexDeepProbe = true;")
                            .with_initialization_script(ACCOUNT_CAPABILITY_PROBE_SCRIPT)
                            .with_ipc_handler(move |request| {
                                if request.uri().scheme_str() != Some("https")
                                    || request.uri().host() != Some("chatgpt.com")
                                    || request
                                        .uri()
                                        .port_u16()
                                        .is_some_and(|port| port != 443)
                                {
                                    return;
                                }
                                let Ok(message) = serde_json::from_str::<serde_json::Value>(
                                    request.body().as_str(),
                                ) else {
                                    return;
                                };
                                if message.get("type").and_then(serde_json::Value::as_str)
                                    == Some("set-power-slider")
                                {
                                    if let (Some(index), Some(request_id)) = (message
                                        .get("index")
                                        .and_then(serde_json::Value::as_u64)
                                        .filter(|index| *index <= 2), message
                                        .get("request_id")
                                        .and_then(serde_json::Value::as_str)
                                        .filter(|request_id| !request_id.is_empty() && request_id.len() <= 96)
                                        .map(str::to_owned))
                                    {
                                        let _ = capability_proxy.send_event(
                                            BrowserEvent::SetPowerSliderValue {
                                                view_id: ipc_view_id.clone(),
                                                request_id,
                                                index: index as u8,
                                                allow_focus: power_slider_focus_requested(&message),
                                            },
                                        );
                                    }
                                    return;
                                }
                                if message.get("type").and_then(serde_json::Value::as_str)
                                    != Some("account-capabilities")
                                {
                                    return;
                                }
                                let Some(capabilities) =
                                    account_capabilities_from_message(&message)
                                else {
                                    return;
                                };
                                let _ = capability_proxy.send_event(
                                    BrowserEvent::AccountCapabilities(capabilities, true),
                                );
                            })
                    } else {
                        builder
                    };
                    builder
                        .with_bounds(content_bounds(&window, false))
                        .with_visible(webview.is_none())
                        .with_url(&url)
                        .with_new_window_req_handler(move |url, _| { let _ = proxy.send_event(BrowserEvent::NewTab(url)); wry::NewWindowResponse::Deny })
                        .build_as_child(&window)
                        .map_err(anyhow::Error::from)
                };
                match created {
                    Ok(view) => {
                        let index = if webview.is_none() { webview = Some(view); main_tab_label = None; browser_page = None; active_tab = 0; 0 } else { extra_tabs.push(view); extra_tab_labels.push(None); extra_tabs.len() };
                        let _ = reply.send(Ok(index));
                        sync_browser_chrome(
                            &chrome,
                            &webview,
                            &extra_tabs,
                            active_tab,
                            main_tab_label.as_deref(),
                            &extra_tab_labels,
                        );
                    }
                    Err(error) => { let _ = reply.send(Err(error.to_string())); }
                }
            }
            Event::UserEvent(BrowserEvent::ListTabs(reply)) => {
                let mut tabs = Vec::new();
                if let Some(view) = &webview { tabs.push(view.url().unwrap_or_default()); }
                tabs.extend(extra_tabs.iter().map(|view| view.url().unwrap_or_default()));
                let _ = reply.send(tabs);
            }
            Event::UserEvent(BrowserEvent::CloseAgentTab(thread_id, agent_path, reply)) => {
                let result = match agent_tabs.request_close(&thread_id, agent_path.as_deref()) {
                    AgentTabCloseStatus::Deferred => AgentTabCloseOutcome::Deferred,
                    AgentTabCloseStatus::Ready(index) => {
                        if close_browser_tab(
                            index,
                            &window,
                            &mut background_focus_guard_requests,
                            &mut webview,
                            &mut main_tab_label,
                            &mut browser_page,
                            &mut extra_tabs,
                            &mut extra_tab_labels,
                            &mut active_tab,
                            &mut agent_tabs,
                            &broker,
                            &mut pending_responses,
                            &mut response_views,
                            &chrome,
                        ) {
                            AgentTabCloseOutcome::Closed
                        } else {
                            AgentTabCloseOutcome::NotFound
                        }
                    }
                };
                if response_views.is_empty() && webview.is_none() {
                    web_context = None;
                }
                let _ = reply.send(result);
            }
            Event::UserEvent(BrowserEvent::Chrome(message)) => {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&message) {
                    let action = value.get("action").and_then(serde_json::Value::as_str).unwrap_or("");
                    let index = value.get("index").and_then(serde_json::Value::as_u64).and_then(|i| usize::try_from(i).ok());
                    match action {
                        "new" => { activate_browser_window(&window, BrowserActivationIntent::UserAction); let _ = event_proxy.send_event(BrowserEvent::NewTab(CHATGPT_TEMPORARY_CHAT_URL.to_owned())); }
                        "select" if index.is_some_and(|i| i <= extra_tabs.len()) => {
                            if let Some(view) = active_browser_view(&webview, &extra_tabs, active_tab) { let _ = view.set_visible(false); }
                            active_tab = index.unwrap_or(0);
                            if let Some(view) = active_browser_view(&webview, &extra_tabs, active_tab) { activate_browser_tab(&window, view, BrowserActivationIntent::UserAction); }
                        }
                        "close" if !extra_tabs.is_empty() && index.is_some_and(|i| i <= extra_tabs.len()) => {
                            activate_browser_window(&window, BrowserActivationIntent::UserAction);
                            let _ = close_browser_tab(
                                index.unwrap_or(0),
                                &window,
                                &mut background_focus_guard_requests,
                                &mut webview,
                                &mut main_tab_label,
                                &mut browser_page,
                                &mut extra_tabs,
                                &mut extra_tab_labels,
                                &mut active_tab,
                                &mut agent_tabs,
                                &broker,
                                &mut pending_responses,
                                &mut response_views,
                                &chrome,
                            );
                        }
                        "navigate" => if let (Some(view), Some(input)) = (active_browser_view(&webview, &extra_tabs, active_tab), value.get("value").and_then(serde_json::Value::as_str)) {
                            activate_browser_window(&window, BrowserActivationIntent::UserAction);
                            if let Some(url) = browser_address(input) { let _ = view.load_url(&url); }
                        },
                        "back" => if let Some(view) = active_browser_view(&webview, &extra_tabs, active_tab) { activate_browser_window(&window, BrowserActivationIntent::UserAction); let _ = view.go_back(); },
                        "forward" => if let Some(view) = active_browser_view(&webview, &extra_tabs, active_tab) { activate_browser_window(&window, BrowserActivationIntent::UserAction); let _ = view.go_forward(); },
                        "reload" => if let Some(view) = active_browser_view(&webview, &extra_tabs, active_tab) { activate_browser_window(&window, BrowserActivationIntent::UserAction); let _ = view.evaluate_script("location.reload()"); },
                        _ => {}
                    }
                    sync_browser_chrome(
                        &chrome,
                        &webview,
                        &extra_tabs,
                        active_tab,
                        main_tab_label.as_deref(),
                        &extra_tab_labels,
                    );
                }
            }
            Event::UserEvent(BrowserEvent::SetPowerSliderValue { view_id, request_id, index, allow_focus }) => {
                #[cfg(target_os = "windows")]
                {
                    use wry::WebViewExtWindows;
                    let Some(view) = browser_view_by_id(&webview, &extra_tabs, &response_views, &view_id) else { return; };
                    if !view.url().is_ok_and(|url| should_probe_chatgpt_url(&url)) { return; }
                    let active_id = active_browser_view(&webview, &extra_tabs, active_tab).map(WebView::id);
                    let hwnd = view.hwnd().0 as usize;
                    let allow_focus = native_power_slider_focus_allowed(allow_focus, active_id, &view_id, power_slider_window_is_foreground(hwnd));
                    let proxy = event_proxy.clone();
                    let worker_view_id = view_id.clone();
                    let worker_request_id = request_id.clone();
                    if let Err(error) = thread::Builder::new()
                        .name("chatgpt-power-slider".to_string())
                        .spawn(move || {
                            let result = set_accessible_power_slider(hwnd, index, allow_focus)
                                .map_err(|error| format!("{error:#}"));
                            if let Err(error) = &result {
                                eprintln!("could not set ChatGPT power slider: {error:#}");
                            }
                            let _ = proxy.send_event(BrowserEvent::PowerSliderValueResult {
                                view_id: worker_view_id,
                                request_id: worker_request_id,
                                result,
                            });
                        })
                    {
                        let _ = event_proxy.send_event(BrowserEvent::PowerSliderValueResult {
                            view_id,
                            request_id,
                            result: Err(format!("could not start native power slider worker: {error}")),
                        });
                    }
                }
                #[cfg(not(target_os = "windows"))]
                {
                    let _ = event_proxy.send_event(BrowserEvent::PowerSliderValueResult {
                        view_id,
                        request_id,
                        result: Err("native ChatGPT power slider input is only available on Windows".into()),
                    });
                }
            }
            Event::UserEvent(BrowserEvent::PowerSliderValueResult { view_id, request_id, result }) => {
                let response = match result {
                    Ok(()) => serde_json::json!({ "ok": true }),
                    Err(error) => serde_json::json!({ "ok": false, "error": error }),
                };
                let request_id = serde_json::to_string(&request_id).unwrap_or_else(|_| "\"\"".into());
                let script = format!(
                    "window.__web2codexPowerSliderResults ||= Object.create(null); window.__web2codexPowerSliderResults[{request_id}] = {response};"
                );
                if let Some(view) = browser_view_by_id(&webview, &extra_tabs, &response_views, &view_id)
                    && view.url().is_ok_and(|url| should_probe_chatgpt_url(&url))
                {
                    let _ = view.evaluate_script(&script);
                }
            }
            Event::UserEvent(BrowserEvent::ContinueLogin(tier)) => {
                if browser_page != Some(BrowserPage::AccountChooser) { return; }
                let Some(view) = webview.as_ref() else { return; };
                if let Err(error) = store_account_tier(&selected_account_tier_path, tier) {
                    eprintln!("Could not save account choice: {error:#}");
                    let _ = view.evaluate_script("document.querySelector('#status').textContent='Could not save your account choice. Please try again.'; for (const button of document.querySelectorAll('button[data-tier]')) button.disabled=false;");
                    return;
                }
                if let Err(error) = view.load_url(CHATGPT_SIGN_IN_URL) {
                    eprintln!("Could not open ChatGPT sign-in: {error:#}");
                    let _ = view.evaluate_script("document.querySelector('#status').textContent='Could not open ChatGPT. Please try again.'; for (const button of document.querySelectorAll('button[data-tier]')) button.disabled=false;");
                    return;
                }
                browser_page = Some(BrowserPage::SignIn);
            }
            Event::UserEvent(BrowserEvent::AccountCapabilities(capabilities, partial)) => {
                let has_models = !capabilities.picker_options.is_empty();
                if let Err(error) = store_account_capabilities(&account_capabilities_path, capabilities, partial) {
                    eprintln!("failed to save ChatGPT account capabilities: {error:#}");
                }
                if !partial && has_models {
                    let retry_ids = pending_responses
                        .iter_mut()
                        .filter_map(|(request_id, pending)| {
                            if pending.awaiting_auth && pending.payload.is_some() {
                                pending.awaiting_auth = false;
                                pending.dispatch_started = false;
                                pending.bridge_ready_deadline = Some(Instant::now() + RESPONSE_BRIDGE_READY_TIMEOUT);
                                Some(request_id.clone())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>();
                    for request_id in &retry_ids {
                        let context = web_context
                            .get_or_insert_with(|| WebContext::new(Some(profile_dir.clone())));
                        let agent_identity = agent_tabs
                            .thread_for_request(request_id)
                            .and_then(|thread_id| {
                                agent_tabs.tab_for_thread(thread_id).map(|tab| {
                                    AgentRequestIdentity {
                                        thread_id: thread_id.to_owned(),
                                        name: tab.name.clone(),
                                        agent_path: tab.agent_path.clone(),
                                    }
                                })
                            });
                        let primary_thread_id = agent_tabs
                            .primary_thread_for_request(request_id)
                            .map(str::to_owned);
                        let primary_epoch = primary_thread_id
                            .as_deref()
                            .and_then(|thread_id| agent_tabs.primary_tab_for_thread(thread_id))
                            .and_then(|tab| tab.conversation_epoch.clone());
                        let setup_smoke_test = agent_tabs.is_setup_smoke_request(request_id);
                        // Login recovery gets a fresh Temporary Chat. Reconstruct the
                        // canonical context instead of resuming a possibly stale page.
                        agent_tabs.require_fresh_response_view(request_id);
                        let result = if let Some(agent_identity) = agent_identity {
                            response_views.remove(request_id);
                            open_agent_response_tab(
                                &agent_identity,
                                request_id,
                                primary_epoch.clone().or_else(|| {
                                    agent_tabs
                                        .tab_for_thread(&agent_identity.thread_id)
                                        .and_then(|tab| tab.conversation_epoch.clone())
                                }),
                                &window,
                                context,
                                event_proxy.clone(),
                                &mut webview,
                                &mut main_tab_label,
                                &mut browser_page,
                                &mut extra_tabs,
                                &mut extra_tab_labels,
                                &mut active_tab,
                                &mut agent_tabs,
                                &chrome,
                            )
                        } else if setup_smoke_test {
                            response_views.remove(request_id);
                            open_setup_smoke_response_tab(
                                request_id,
                                &window,
                                context,
                                event_proxy.clone(),
                                &mut webview,
                                &mut main_tab_label,
                                &mut browser_page,
                                &mut extra_tabs,
                                &mut extra_tab_labels,
                                &mut active_tab,
                                &mut agent_tabs,
                                &chrome,
                            )
                        } else {
                            open_primary_response_tab(
                                request_id,
                                primary_thread_id.as_deref(),
                                primary_epoch,
                                false,
                                None,
                                &window,
                                context,
                                event_proxy.clone(),
                                &mut webview,
                                &mut main_tab_label,
                                &mut browser_page,
                                &mut extra_tabs,
                                &mut extra_tab_labels,
                                &mut active_tab,
                                &mut agent_tabs,
                                &chrome,
                            )
                        };
                        if let Err(error) = result {
                            release_background_focus_guard(
                                request_id,
                                &window,
                                &mut background_focus_guard_requests,
                            );
                            broker.complete(request_id, None, Err(error.to_string()));
                            pending_responses.remove(request_id);
                        }
                    }
                    // Authentication recovery is background work. Keep the
                    // current window visibility and foreground unchanged so
                    // retrying an agent request cannot interrupt another app.
                }
            }
            Event::UserEvent(BrowserEvent::ResponsesBridgeReady {
                request_id,
                conversation_has_history,
            }) => {
                if let (Some(view), Some(pending)) = (
                    response_view_for_request(
                        &request_id,
                        &response_views,
                        &agent_tabs,
                        &webview,
                        &extra_tabs,
                    ),
                    pending_responses.get_mut(&request_id),
                ) {
                    if pending.awaiting_auth || (!pending.dispatch_started && pending.bridge_ready_deadline.is_none()) {
                        return;
                    }
                    pending.bridge_ready_deadline = None;
                    let use_resume_payload =
                        conversation_has_history && pending.resume_payload.is_some();
                    let payload = if use_resume_payload {
                        pending.resume_payload.as_ref().or(pending.payload.as_ref())
                    } else {
                        pending.payload.as_ref()
                    }
                    .cloned()
                    .filter(|_| !pending.dispatch_started);
                    let Some(payload) = payload else {
                        agent_tabs.require_fresh_response_view(&request_id);
                        release_background_focus_guard(
                            &request_id,
                            &window,
                            &mut background_focus_guard_requests,
                        );
                        broker.complete(&request_id, None, Err("The Temporary Chat reloaded after dispatch. Check the previous turn before sending another request.".into()));
                        return;
                    };
                    if use_resume_payload {
                        pending.input_characters = pending
                            .resume_input_characters
                            .unwrap_or(pending.input_characters);
                    }
                    pending.dispatch_started = true;
                    if let Err(error) = view.evaluate_script(&response_signal_script(&request_id, &format!("if (location.origin === 'https://chatgpt.com' && location.pathname === '/' && new URL(location.href).searchParams.get('temporary-chat') === 'true') window.__web2codexSend({payload});"))) {
                        agent_tabs.require_fresh_response_view(&request_id);
                        release_background_focus_guard(
                            &request_id,
                            &window,
                            &mut background_focus_guard_requests,
                        );
                        broker.complete(&request_id, None, Err(format!("Could not send prompt to ChatGPT Web: {error}")));
                    }
                }
            }
            Event::UserEvent(BrowserEvent::ResponsesBridgeUnavailable {
                request_id,
                binding,
                document_reloaded,
                error,
            }) => {
                let current_binding = agent_tabs.tab_index_for_request(&request_id)
                    .and_then(|index| agent_tabs.response_request_id_state(index));
                if current_binding.as_ref().is_some_and(|current| Arc::ptr_eq(current, &binding))
                    && let Some(pending) = pending_responses.get_mut(&request_id)
                    && !pending.awaiting_auth
                    && (document_reloaded || !pending.dispatch_started)
                {
                    pending.bridge_ready_deadline = None;
                    agent_tabs.require_fresh_response_view(&request_id);
                    release_background_focus_guard(
                        &request_id,
                        &window,
                        &mut background_focus_guard_requests,
                    );
                    broker.complete(&request_id, None, Err(error));
                }
            }
            Event::UserEvent(BrowserEvent::ResponsesPromptSubmitted(request_id)) => {
                if let Some(pending) = pending_responses.get_mut(&request_id) {
                    pending.payload = None;
                    pending.resume_payload = None;
                    pending.awaiting_auth = false;
                }
            }
            Event::UserEvent(BrowserEvent::ResponsesAuthRequired(request_id)) => {
                let can_resume = pending_responses
                    .get_mut(&request_id)
                    .is_some_and(|pending| {
                        if pending.payload.is_some() {
                            pending.awaiting_auth = true;
                            pending.dispatch_started = false;
                            pending.bridge_ready_deadline = None;
                            true
                        } else {
                            false
                        }
                    });
                response_views.remove(&request_id);
                if !can_resume {
                    broker.complete(
                        &request_id,
                        None,
                        Err("ChatGPT sign-in expired after the prompt was sent. The turn stopped to avoid sending it twice; sign in, check the previous turn, then retry if it did not complete.".into()),
                    );
                    release_background_focus_guard(
                        &request_id,
                        &window,
                        &mut background_focus_guard_requests,
                    );
                }
                let page = if selected_account_tier_path.is_file() {
                    BrowserPage::SignIn
                } else {
                    BrowserPage::AccountChooser
                };
                if browser_page != Some(page) || webview.is_none() {
                    if let Err(error) = prepare_profile_directory(&profile_dir) {
                        eprintln!("could not prepare ChatGPT sign-in recovery: {error:#}");
                        release_background_focus_guard(
                            &request_id,
                            &window,
                            &mut background_focus_guard_requests,
                        );
                        return;
                    }
                    if page == BrowserPage::AccountChooser {
                        let _ = fs::remove_file(&account_capabilities_path);
                    }
                    webview = None;
                    main_tab_label = None;
                    let context = web_context
                        .get_or_insert_with(|| WebContext::new(Some(profile_dir.clone())));
                    match build_webview(&window, context, page, event_proxy.clone(), None, false, None) {
                        Ok(created) => {
                            webview = Some(created);
                            browser_page = Some(page);
                        }
                        Err(error) => {
                            eprintln!("could not open ChatGPT sign-in recovery: {error:#}");
                            release_background_focus_guard(
                                &request_id,
                                &window,
                                &mut background_focus_guard_requests,
                            );
                            return;
                        }
                    }
                }
                activate_browser_window(&window, BrowserActivationIntent::BackgroundWork);
            }
            Event::UserEvent(BrowserEvent::ResponsesRequest { request_id, conversation_id, conversation_epoch, agent, setup_smoke_test, model, reasoning_effort, connector_required, mcp_required, mcp_action_required, tool_work_expected, prompt, stages, resume_prompt, attachments, resume_attachments, interruption_prompt, interruption_attachment_names }) => {
                if let Err(error) = prepare_profile_directory(&profile_dir) {
                    broker.complete(&request_id, None, Err(error.to_string())); return;
                }
                if !setup_smoke_test {
                    if should_apply_background_focus_guard(browser_window_is_foreground(&window)) {
                        if let Err(error) = set_window_no_activate(&window, true) {
                            broker.complete(
                                &request_id,
                                None,
                                Err(format!("Could not protect foreground focus while sending a background request: {error:#}")),
                            );
                            return;
                        }
                    }
                    background_focus_guard_requests.insert(request_id.clone());
                }
                let context = web_context.get_or_insert_with(|| WebContext::new(Some(profile_dir.clone())));
                let persistent_thread = !setup_smoke_test && (agent.is_some() || conversation_id.is_some());
                let compaction = !stages.is_empty();
                let mut interrupted_request_state = None;
                if !compaction && agent.is_none() && interruption_prompt.is_some()
                    && let Some(thread_id) = conversation_id.as_deref()
                {
                    let previous_request = agent_tabs
                        .primary_tab_for_thread(thread_id)
                        .filter(|tab| tab.conversation_epoch == conversation_epoch)
                        .and_then(|tab| tab.active_request_id.clone())
                        .filter(|previous_request| previous_request != &request_id);
                    if let Some(previous_request) = previous_request
                        && broker.snapshot(&previous_request)["active"].as_bool() == Some(true)
                    {
                        agent_tabs.note_interrupted_primary(&previous_request);
                        if let Some(view) = response_view_for_request(
                            &previous_request,
                            &response_views,
                            &agent_tabs,
                            &webview,
                            &extra_tabs,
                        ) {
                            let previous_request = serde_json::to_string(&previous_request)
                                .unwrap_or_else(|_| "null".to_owned());
                            let _ = view.evaluate_script(&format!(
                                "window.__web2codexCancel?.({previous_request});"
                            ));
                        }
                    }
                    interrupted_request_state = agent_tabs
                        .take_interrupted_primary(thread_id, conversation_epoch.as_deref());
                }
                let interrupt_existing_response = interrupted_request_state.is_some();
                let prompt = if interrupt_existing_response {
                    interruption_prompt.map_or(prompt, |message| {
                        format!(
                            "The user interrupted the previous response and sent a new message. Continue the same task, taking this message and its attachments into account:\n\nUser message:\n{message}"
                        )
                    })
                } else {
                    prompt
                };
                let attachments = if interrupt_existing_response {
                    attachments
                        .into_iter()
                        .filter(|attachment| {
                            interruption_attachment_names.contains(&attachment.name)
                        })
                        .collect()
                } else {
                    attachments
                };
                if let Some(agent) = agent {
                    if let Err(error) = open_agent_response_tab(
                        &agent,
                        &request_id,
                        conversation_epoch.clone(),
                        &window,
                        context,
                        event_proxy.clone(),
                        &mut webview,
                        &mut main_tab_label,
                        &mut browser_page,
                        &mut extra_tabs,
                        &mut extra_tab_labels,
                        &mut active_tab,
                        &mut agent_tabs,
                        &chrome,
                    ) {
                        release_background_focus_guard(
                            &request_id,
                            &window,
                            &mut background_focus_guard_requests,
                        );
                        broker.complete(&request_id, None, Err(error.to_string()));
                        return;
                    }
                } else if setup_smoke_test {
                    if let Err(error) = open_setup_smoke_response_tab(
                        &request_id,
                        &window,
                        context,
                        event_proxy.clone(),
                        &mut webview,
                        &mut main_tab_label,
                        &mut browser_page,
                        &mut extra_tabs,
                        &mut extra_tab_labels,
                        &mut active_tab,
                        &mut agent_tabs,
                        &chrome,
                    ) {
                        release_background_focus_guard(
                            &request_id,
                            &window,
                            &mut background_focus_guard_requests,
                        );
                        broker.complete(&request_id, None, Err(error.to_string()));
                        return;
                    }
                } else {
                    if let Err(error) = open_primary_response_tab(
                        &request_id,
                        conversation_id.as_deref(),
                        conversation_epoch.clone(),
                        interrupt_existing_response,
                        interrupted_request_state,
                        &window,
                        context,
                        event_proxy.clone(),
                        &mut webview,
                        &mut main_tab_label,
                        &mut browser_page,
                        &mut extra_tabs,
                        &mut extra_tab_labels,
                        &mut active_tab,
                        &mut agent_tabs,
                        &chrome,
                    ) {
                        release_background_focus_guard(
                            &request_id,
                            &window,
                            &mut background_focus_guard_requests,
                        );
                        broker.complete(&request_id, None, Err(error.to_string()));
                        return;
                    }
                }
                let stage_characters = stages
                    .iter()
                    .filter_map(|stage| stage["prompt"].as_str())
                    .map(|stage| stage.chars().count())
                    .sum::<usize>();
                let input_characters = prompt.chars().count() + stage_characters;
                let resume_input_characters = (!resume_prompt.is_empty()).then(|| {
                    resume_prompt.chars().count() + stage_characters
                });
                let payload = serde_json::json!({"request_id":request_id,"model":model,"reasoning_effort":reasoning_effort,"connector_required":connector_required,"mcp_required":mcp_required,"mcp_action_required":mcp_action_required,"tool_work_expected":tool_work_expected,"prompt":prompt,"stages":stages.clone(),"attachments":attachments});
                let resume_payload = persistent_thread
                    .then_some(())
                    .filter(|_| !resume_prompt.is_empty())
                    .map(|_| serde_json::json!({"request_id":request_id,"model":model,"reasoning_effort":reasoning_effort,"connector_required":connector_required,"mcp_required":mcp_required,"mcp_action_required":mcp_action_required,"tool_work_expected":tool_work_expected,"prompt":resume_prompt,"stages":stages,"attachments":resume_attachments}));
                pending_responses.insert(
                    request_id,
                    PendingResponsesRequest {
                        model,
                        input_characters,
                        resume_input_characters,
                        compaction,
                        payload: Some(payload),
                        resume_payload,
                        dispatch_started: false,
                        awaiting_auth: false,
                        bridge_ready_deadline: Some(
                            Instant::now() + RESPONSE_BRIDGE_READY_TIMEOUT,
                        ),
                        started_at: std::time::Instant::now(),
                    },
                );
            }
            Event::UserEvent(BrowserEvent::ResponsesToolState(request_id)) => {
                let state = broker.snapshot(&request_id);
                if state.get("active").and_then(serde_json::Value::as_bool) == Some(false) {
                    if let Some(view) = response_view_for_request(
                        &request_id,
                        &response_views,
                        &agent_tabs,
                        &webview,
                        &extra_tabs,
                    ) {
                        let _ = view.evaluate_script(&response_signal_script(&request_id, "window.__web2codexCompleted=true;"));
                    }
                    release_background_focus_guard(
                        &request_id,
                        &window,
                        &mut background_focus_guard_requests,
                    );
                    if agent_tabs.tab_index_for_request(&request_id).is_none() {
                        response_views.remove(&request_id);
                    }
                    if let Some(pending) = pending_responses.remove(&request_id) {
                        agent_tabs.require_fresh_response_view(&request_id);
                        let milliseconds = u64::try_from(pending.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
                        let _ = record_local_usage(&usage_stats_path, &pending.model, pending.input_characters, None, milliseconds);
                    }
                    if response_views.is_empty() && webview.is_none() { web_context = None; }
                    return;
                }
                    if let Some(view) = response_view_for_request(
                        &request_id,
                        &response_views,
                        &agent_tabs,
                        &webview,
                        &extra_tabs,
                    ) {
                    let _ = view.evaluate_script(&response_signal_script(&request_id, &format!("window.__web2codexToolState={state};")));
                }
            }
            Event::UserEvent(BrowserEvent::ResponsesText { request_id, delta }) => {
                let _ = broker.append_text(&request_id, delta);
            }
            Event::UserEvent(BrowserEvent::ResponsesReasoningHeading {
                request_id,
                heading,
            }) => {
                let _ = broker.append_reasoning_heading(&request_id, heading);
            }
            Event::UserEvent(BrowserEvent::ResponsesResult { request_id, revision, result }) => {
                let output_characters = result.as_ref().ok().map(|text| text.chars().count());
                if broker.complete(&request_id, revision, result) {
                    // Agent text has already been handed to Codex through the response stream.
                    // Close the auxiliary browser tab once its HTTP observer releases it.
                    let _ = agent_tabs.request_close_for_request(&request_id);
                    release_background_focus_guard(
                        &request_id,
                        &window,
                        &mut background_focus_guard_requests,
                    );
                    if let Some(view) = response_view_for_request(
                        &request_id,
                        &response_views,
                        &agent_tabs,
                        &webview,
                        &extra_tabs,
                    ) {
                        let _ = view.evaluate_script(&response_signal_script(&request_id, "window.__web2codexCompleted=true;"));
                    }
                    if let Some(pending) = pending_responses.remove(&request_id) {
                        if pending.compaction {
                            // The staged source context was only needed to make this summary.
                            // Start the next Codex window in a clean Temporary Chat.
                            agent_tabs.require_fresh_response_view(&request_id);
                        }
                        let milliseconds = u64::try_from(pending.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
                        if let Err(error) = record_local_usage(&usage_stats_path, &pending.model, pending.input_characters, output_characters, milliseconds) {
                            eprintln!("failed to record local Web2Codex usage: {error:#}");
                        }
                    }
                }
            }
            Event::UserEvent(BrowserEvent::ReleaseResponses(_)) => {}
            Event::UserEvent(BrowserEvent::CancelResponses(request_id)) => {
                agent_tabs.note_interrupted_primary(&request_id);
                if let Some(view) = response_view_for_request(
                    &request_id,
                    &response_views,
                    &agent_tabs,
                    &webview,
                    &extra_tabs,
                ) {
                    let request_id = serde_json::to_string(&request_id).unwrap_or_else(|_| "null".to_owned());
                    let _ = view.evaluate_script(&format!("window.__web2codexCancel?.({request_id});"));
                }
            }
            Event::UserEvent(BrowserEvent::Hide) => {
                window.set_visible(false);
            }
            Event::UserEvent(BrowserEvent::Stop(reply)) => {
                stop_reply = reply;
            }
            Event::WindowEvent { window_id: event_window_id, event: WindowEvent::CloseRequested, .. } if event_window_id == window_id => {
                window.set_visible(false);
            }
            Event::WindowEvent { window_id: event_window_id, event: WindowEvent::Focused(false), .. } if event_window_id == window_id && !background_focus_guard_requests.is_empty() => {
                if let Err(error) = set_window_no_activate(&window, true) {
                    eprintln!("Could not prevent a background response from reclaiming focus: {error:#}");
                }
            }
            Event::WindowEvent { window_id: event_window_id, event: WindowEvent::Focused(true), .. } if event_window_id == window_id && !background_focus_guard_requests.is_empty() => {
                if let Err(error) = set_window_no_activate(&window, false) {
                    eprintln!("Could not restore browser interaction after user activation: {error:#}");
                }
            }
            Event::WindowEvent { window_id: event_window_id, event: WindowEvent::Resized(_), .. } if event_window_id == window_id => {
                let _ = chrome.set_bounds(chrome_bounds(&window));
                if let Some(view) = &webview { let _ = view.set_bounds(content_bounds(&window, false)); }
                for view in &extra_tabs { let _ = view.set_bounds(content_bounds(&window, false)); }
                for view in response_views.values() { let _ = view.set_bounds(content_bounds(&window, true)); }
            }
            _ => {}
        }
        if stop_reply.is_some() || idle_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            window.set_visible(false);
            broker.stop("ChatGPT WebView stopped before the response completed");
            response_views.clear();
            pending_responses.clear();
            remove_runtime_files(&state_path, &lock_file);
            // Acknowledge the stop before exiting the process and terminating
            // the detached HTTP request threads.
            if let Some(mut stream) = stop_reply {
                let _ = write_response(&mut stream, "204 No Content", "");
            }
            idle_deadline = None;
            *control_flow = ControlFlow::Exit;
        } else {
            *control_flow = response_bridge_wakeup(idle_deadline, &pending_responses);
        }
    })
}

fn chrome_bounds(window: &Window) -> Rect {
    let size = window.inner_size();
    let height = (82.0 * window.scale_factor()).round() as u32;
    Rect {
        position: PhysicalPosition::new(0, 0).into(),
        size: PhysicalSize::new(size.width, height.min(size.height)).into(),
    }
}

fn content_bounds(window: &Window, background: bool) -> Rect {
    let size = window.inner_size();
    let top = ((82.0 * window.scale_factor()).round() as u32).min(size.height);
    Rect {
        position: PhysicalPosition::new(if background { -32000 } else { 0 }, top as i32).into(),
        size: PhysicalSize::new(size.width.max(1), size.height.saturating_sub(top).max(1)).into(),
    }
}

fn active_browser_view<'a>(
    main: &'a Option<WebView>,
    extras: &'a [WebView],
    active: usize,
) -> Option<&'a WebView> {
    if active == 0 {
        main.as_ref()
    } else {
        extras.get(active - 1)
    }
}

fn browser_view_by_id<'a>(
    main: &'a Option<WebView>,
    extras: &'a [WebView],
    responses: &'a HashMap<String, WebView>,
    view_id: &str,
) -> Option<&'a WebView> {
    main.iter()
        .chain(extras.iter())
        .chain(responses.values())
        .find(|view| view.id() == view_id)
}

#[cfg(any(target_os = "windows", test))]
fn native_power_slider_focus_allowed(
    requested: bool,
    active_view_id: Option<&str>,
    source_view_id: &str,
    visible_foreground: bool,
) -> bool {
    requested && visible_foreground && active_view_id == Some(source_view_id)
}

fn power_slider_focus_requested(message: &serde_json::Value) -> bool {
    // Older clients omitted the field. An explicit or malformed opt-out
    // must never grant focus permission.
    message.get("allow_focus").is_none_or(|value| value.as_bool() == Some(true))
}

fn browser_address(input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() || input.len() > 8192 {
        return None;
    }
    let candidate = if input.contains("://") {
        input.to_owned()
    } else if !input.contains(char::is_whitespace)
        && (input.contains('.') || input.starts_with("localhost"))
    {
        format!("https://{input}")
    } else {
        let mut url = url::Url::parse("https://www.google.com/search").ok()?;
        url.query_pairs_mut().append_pair("q", input);
        return Some(url.into());
    };
    let url = url::Url::parse(&candidate).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.to_string())
}

fn should_probe_chatgpt_url(input: &str) -> bool {
    url::Url::parse(input)
        .is_ok_and(|url| url.origin().ascii_serialization() == "https://chatgpt.com")
}

fn sync_browser_chrome(
    chrome: &WebView,
    main: &Option<WebView>,
    extras: &[WebView],
    active: usize,
    main_label: Option<&str>,
    extra_labels: &[Option<String>],
) {
    let mut tabs = Vec::new();
    if let Some(view) = main {
        tabs.push(view.url().unwrap_or_default());
    }
    tabs.extend(extras.iter().map(|view| view.url().unwrap_or_default()));
    let current = tabs.get(active).cloned().unwrap_or_default();
    let titles: Vec<String> = tabs
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let custom = if index == 0 {
                main_label
            } else {
                extra_labels.get(index - 1).and_then(Option::as_deref)
            };
            custom.map(str::to_owned).unwrap_or_else(|| {
                url::Url::parse(item)
                    .ok()
                    .and_then(|url| url.host_str().map(str::to_owned))
                    .unwrap_or_else(|| "Новая вкладка".to_owned())
            })
        })
        .collect();
    let state = serde_json::json!({"tabs":titles,"active":active,"url":current});
    let _ = chrome.evaluate_script(&format!("window.setBrowserState?.({state})"));
}

fn close_browser_tab(
    index: usize,
    window: &Window,
    background_focus_guard_requests: &mut HashSet<String>,
    webview: &mut Option<WebView>,
    main_tab_label: &mut Option<String>,
    browser_page: &mut Option<BrowserPage>,
    extra_tabs: &mut Vec<WebView>,
    extra_tab_labels: &mut Vec<Option<String>>,
    active_tab: &mut usize,
    agent_tabs: &mut AgentTabRegistry,
    broker: &TurnBroker,
    pending_responses: &mut HashMap<String, PendingResponsesRequest>,
    response_views: &mut HashMap<String, WebView>,
    chrome: &WebView,
) -> bool {
    if index > extra_tabs.len() {
        return false;
    }

    let previous_active = *active_tab;
    for request_id in agent_tabs.close_tab(index) {
        release_background_focus_guard(&request_id, window, background_focus_guard_requests);
        broker.complete(
            &request_id,
            None,
            Err(
                "The agent's Web2Codex browser tab was closed before its response completed"
                    .to_owned(),
            ),
        );
        pending_responses.remove(&request_id);
        response_views.remove(&request_id);
    }
    if index == 0 {
        if extra_tabs.is_empty() {
            *webview = None;
            *main_tab_label = None;
        } else {
            *webview = Some(extra_tabs.remove(0));
            *main_tab_label = extra_tab_labels.remove(0);
        }
        *browser_page = None;
    } else {
        extra_tabs.remove(index - 1);
        extra_tab_labels.remove(index - 1);
    }

    *active_tab = active_tab_after_close(previous_active, index);
    for tab in extra_tabs.iter() {
        let _ = tab.set_visible(false);
    }
    if let Some(view) = active_browser_view(webview, extra_tabs, *active_tab) {
        let _ = view.set_visible(true);
    }
    sync_browser_chrome(
        chrome,
        webview,
        extra_tabs,
        *active_tab,
        main_tab_label.as_deref(),
        extra_tab_labels,
    );
    true
}

fn active_tab_after_close(active: usize, closed: usize) -> usize {
    if active == closed {
        0
    } else if active > closed {
        active - 1
    } else {
        active
    }
}

fn response_view_for_request<'a>(
    request_id: &str,
    response_views: &'a HashMap<String, WebView>,
    agent_tabs: &AgentTabRegistry,
    main: &'a Option<WebView>,
    extras: &'a [WebView],
) -> Option<&'a WebView> {
    if let Some(view) = response_views.get(request_id) {
        return Some(view);
    }
    let index = agent_tabs.tab_index_for_request(request_id)?;
    if index == 0 {
        main.as_ref()
    } else {
        extras.get(index - 1)
    }
}

fn open_agent_response_tab(
    identity: &AgentRequestIdentity,
    request_id: &str,
    conversation_epoch: Option<String>,
    window: &Window,
    context: &mut WebContext,
    event_proxy: EventLoopProxy<BrowserEvent>,
    main: &mut Option<WebView>,
    main_label: &mut Option<String>,
    browser_page: &mut Option<BrowserPage>,
    extras: &mut Vec<WebView>,
    extra_labels: &mut Vec<Option<String>>,
    active_tab: &mut usize,
    agent_tabs: &mut AgentTabRegistry,
    chrome: &WebView,
) -> anyhow::Result<()> {
    let previous = agent_tabs.tab_for_thread(&identity.thread_id).cloned();
    if previous
        .as_ref()
        .and_then(|tab| tab.active_request_id.as_deref())
        .is_some_and(|active_request| active_request != request_id)
    {
        anyhow::bail!("this subagent already has an active browser response");
    }
    let existing_index = previous.as_ref().map(|tab| tab.tab_index);
    let reuse_existing = previous.as_ref().is_some_and(|tab| {
        tab.conversation_epoch == conversation_epoch
            && agent_tabs.response_request_id_state(tab.tab_index).is_some()
    });
    let request_id_state = if reuse_existing {
        let state = agent_tabs
            .response_request_id_state(existing_index.expect("reused agent tab has an index"))
            .expect("reused agent tab has a response binding");
        if let Ok(mut current) = state.lock() {
            *current = Some(request_id.to_owned());
        }
        state
    } else {
        Arc::new(Mutex::new(Some(request_id.to_owned())))
    };
    let (index, _) = open_visible_response_tab(
        &format!("Agent · {}", identity.name),
        request_id,
        existing_index,
        request_id_state.clone(),
        reuse_existing,
        window,
        context,
        event_proxy,
        main,
        main_label,
        browser_page,
        extras,
        extra_labels,
        active_tab,
        chrome,
        BrowserActivationIntent::BackgroundWork,
    )?;
    agent_tabs.bind_response_request_id_state(index, request_id_state);
    agent_tabs.bind_request_with_epoch(identity, request_id, index, conversation_epoch);
    Ok(())
}

fn open_primary_response_tab(
    request_id: &str,
    conversation_id: Option<&str>,
    conversation_epoch: Option<String>,
    interrupt_existing_response: bool,
    interrupted_request_state: Option<Arc<Mutex<Option<String>>>>,
    window: &Window,
    context: &mut WebContext,
    event_proxy: EventLoopProxy<BrowserEvent>,
    main: &mut Option<WebView>,
    main_label: &mut Option<String>,
    browser_page: &mut Option<BrowserPage>,
    extras: &mut Vec<WebView>,
    extra_labels: &mut Vec<Option<String>>,
    active_tab: &mut usize,
    response_tabs: &mut AgentTabRegistry,
    chrome: &WebView,
) -> anyhow::Result<()> {
    let previous = conversation_id
        .and_then(|thread_id| response_tabs.primary_tab_for_thread(thread_id).cloned());
    let previous_response_still_active = previous
        .as_ref()
        .and_then(|tab| tab.active_request_id.as_deref())
        .is_some_and(|active_request| active_request != request_id);
    // Independent requests get their own Temporary Chat. An explicit user
    // interruption reuses the active chat after its response is stopped.
    let existing_index = if previous_response_still_active && !interrupt_existing_response {
        None
    } else {
        previous
            .as_ref()
            .map(|tab| tab.tab_index)
            .or_else(|| response_tabs.reusable_primary_response_tab())
    };
    let reuse_existing = previous.as_ref().is_some_and(|tab| {
        tab.conversation_epoch == conversation_epoch
            && (response_tabs.response_request_id_state(tab.tab_index).is_some()
                || interrupt_existing_response)
    });
    let request_id_state = if reuse_existing {
        let state = response_tabs
            .response_request_id_state(existing_index.expect("reused chat tab has an index"))
            .or(interrupted_request_state);
        if let Some(state) = state {
            if let Ok(mut current) = state.lock() {
                *current = Some(request_id.to_owned());
            }
            state
        } else {
            Arc::new(Mutex::new(Some(request_id.to_owned())))
        }
    } else {
        Arc::new(Mutex::new(Some(request_id.to_owned())))
    };
    let (index, _) = open_visible_response_tab(
        "ChatGPT",
        request_id,
        existing_index,
        request_id_state.clone(),
        reuse_existing,
        window,
        context,
        event_proxy,
        main,
        main_label,
        browser_page,
        extras,
        extra_labels,
        active_tab,
        chrome,
        BrowserActivationIntent::BackgroundTabSelection,
    )?;
    response_tabs.bind_response_request_id_state(index, request_id_state);
    if let Some(thread_id) = conversation_id {
        response_tabs.bind_primary_thread_request(
            thread_id,
            request_id,
            index,
            conversation_epoch,
        );
    } else {
        response_tabs.bind_primary_response_request(request_id, index);
    }
    Ok(())
}

fn open_setup_smoke_response_tab(
    request_id: &str,
    window: &Window,
    context: &mut WebContext,
    event_proxy: EventLoopProxy<BrowserEvent>,
    main: &mut Option<WebView>,
    main_label: &mut Option<String>,
    browser_page: &mut Option<BrowserPage>,
    extras: &mut Vec<WebView>,
    extra_labels: &mut Vec<Option<String>>,
    active_tab: &mut usize,
    response_tabs: &mut AgentTabRegistry,
    chrome: &WebView,
) -> anyhow::Result<()> {
    if response_tabs
        .setup_smoke_tab
        .as_ref()
        .and_then(|tab| tab.active_request_id.as_deref())
        .is_some_and(|active_request| active_request != request_id)
    {
        anyhow::bail!("the setup smoke test already has an active browser response");
    }

    let request_id_state = Arc::new(Mutex::new(Some(request_id.to_owned())));
    let (index, _) = open_visible_response_tab(
        "Setup · smoke test",
        request_id,
        response_tabs.setup_smoke_tab_index(),
        request_id_state.clone(),
        false,
        window,
        context,
        event_proxy,
        main,
        main_label,
        browser_page,
        extras,
        extra_labels,
        active_tab,
        chrome,
        BrowserActivationIntent::UserAction,
    )?;
    response_tabs.bind_response_request_id_state(index, request_id_state);
    response_tabs.bind_setup_smoke_request(request_id, index);
    Ok(())
}

fn open_visible_response_tab(
    label: &str,
    request_id: &str,
    existing_index: Option<usize>,
    request_id_state: Arc<Mutex<Option<String>>>,
    reuse_existing: bool,
    window: &Window,
    context: &mut WebContext,
    event_proxy: EventLoopProxy<BrowserEvent>,
    main: &mut Option<WebView>,
    main_label: &mut Option<String>,
    browser_page: &mut Option<BrowserPage>,
    extras: &mut Vec<WebView>,
    extra_labels: &mut Vec<Option<String>>,
    active_tab: &mut usize,
    chrome: &WebView,
    activation_intent: BrowserActivationIntent,
) -> anyhow::Result<(usize, bool)> {
    let existing_index = existing_index.filter(|index| {
        if *index == 0 {
            main.is_some()
        } else {
            extras.get(index - 1).is_some()
        }
    });
    if existing_index.is_none() && extras.len() >= 31 {
        anyhow::bail!("the browser has reached its 32 tab limit");
    }
    let index = existing_index.unwrap_or(extras.len() + 1);
    let starts_in_viewport = response_tab_starts_in_viewport(activation_intent, *active_tab, index);

    if reuse_existing {
        if let Some(view) = active_browser_view(main, extras, index) {
            let bind_script = response_bind_script(request_id);
            if view.evaluate_script(&bind_script).is_ok() {
                if index == 0 {
                    *main_label = Some(label.to_owned());
                    *browser_page = Some(BrowserPage::TemporaryChat);
                } else if let Some(slot) = extra_labels.get_mut(index - 1) {
                    *slot = Some(label.to_owned());
                }
                if starts_in_viewport {
                    if *active_tab != index {
                        if let Some(previous) = active_browser_view(main, extras, *active_tab) {
                            let _ = previous.set_visible(false);
                        }
                        *active_tab = index;
                    }
                    if let Some(active) = active_browser_view(main, extras, *active_tab) {
                        activate_browser_tab(window, active, activation_intent);
                    }
                } else if let Some(background) = active_browser_view(main, extras, index) {
                    let _ = background.set_visible(false);
                }
                sync_browser_chrome(
                    chrome,
                    main,
                    extras,
                    *active_tab,
                    main_label.as_deref(),
                    extra_labels,
                );
                return Ok((index, true));
            }
        }
    }

    if main.is_none() {
        let view = build_webview(
            window,
            context,
            BrowserPage::TemporaryChat,
            event_proxy.clone(),
            None,
            false,
            None,
        )?;
        main.replace(view);
        *main_label = None;
        *browser_page = Some(BrowserPage::TemporaryChat);
    }

    let view = build_webview(
        window,
        context,
        BrowserPage::TemporaryChat,
        event_proxy,
        Some(request_id.to_owned()),
        starts_in_viewport,
        Some(request_id_state),
    )?;
    if let Some(existing_index) = existing_index {
        if existing_index == 0 {
            *main = Some(view);
            *main_label = Some(label.to_owned());
            *browser_page = Some(BrowserPage::TemporaryChat);
        } else {
            let slot = existing_index - 1;
            extras[slot] = view;
            extra_labels[slot] = Some(label.to_owned());
        }
    } else {
        extras.push(view);
        extra_labels.push(Some(label.to_owned()));
    }
    if starts_in_viewport {
        if *active_tab != index {
            if let Some(previous) = active_browser_view(main, extras, *active_tab) {
                let _ = previous.set_visible(false);
            }
            *active_tab = index;
        }
        if let Some(active) = active_browser_view(main, extras, *active_tab) {
            activate_browser_tab(window, active, activation_intent);
        }
    } else if let Some(background) = active_browser_view(main, extras, index) {
        let _ = background.set_visible(false);
    }
    sync_browser_chrome(
        chrome,
        main,
        extras,
        *active_tab,
        main_label.as_deref(),
        extra_labels,
    );
    Ok((index, false))
}

fn build_webview(
    window: &Window,
    context: &mut WebContext,
    page: BrowserPage,
    event_proxy: EventLoopProxy<BrowserEvent>,
    response_id: Option<String>,
    response_visible_tab: bool,
    response_request_id: Option<Arc<Mutex<Option<String>>>>,
) -> anyhow::Result<WebView> {
    let response_request_id = response_request_id.or_else(|| {
        response_id
            .as_ref()
            .map(|request_id| Arc::new(Mutex::new(Some(request_id.clone()))))
    });
    let view_id = Uuid::new_v4().to_string();
    let ipc_view_id = view_id.clone();
    let builder = WebViewBuilder::new_with_web_context(context).with_id(&view_id);
    let builder = if response_id.is_some() {
        builder.with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
    } else {
        let proxy = event_proxy.clone();
        builder.with_new_window_req_handler(move |url, _| {
            let _ = proxy.send_event(BrowserEvent::NewTab(url));
            wry::NewWindowResponse::Deny
        })
    };
    #[cfg(target_os = "macos")]
    let builder = builder.with_data_store_identifier(PROFILE_DATA_STORE_ID);

    let ipc_response_id = response_request_id.clone();
    let navigation_request = response_request_id.clone();
    let navigation_proxy = event_proxy.clone();
    let builder = builder
        // Keep the WebView controller rendering even while its parent window is
        // hidden. A hidden controller can report zero-sized DOM elements and
        // prevents the model picker from being discovered in background turns.
        .with_visible(true)
        .with_bounds(content_bounds(
            window,
            response_id.is_some() && !response_visible_tab,
        ))
        .with_initialization_script(if response_id.is_none() {
            "window.__web2codexDeepProbe = true;"
        } else {
            "window.__web2codexDeepProbe = false;"
        })
        .with_initialization_script(ACCOUNT_CAPABILITY_PROBE_SCRIPT)
        .with_navigation_handler(move |target| {
            if let Some(request_binding) = &navigation_request {
                let request_id = request_binding
                    .lock()
                    .ok()
                    .and_then(|request_id| request_id.clone());
                let temporary = url::Url::parse(&target).is_ok_and(|url| {
                    url.origin().ascii_serialization() == "https://chatgpt.com"
                        && url.path() == "/"
                        && url.query_pairs().find(|(key, _)| key == "temporary-chat")
                            .is_some_and(|(_, value)| value == "true")
                });
                if !temporary {
                    if let Some(request_id) = request_id {
                        if is_chatgpt_auth_url(&target) {
                            let _ = navigation_proxy.send_event(
                                BrowserEvent::ResponsesAuthRequired(request_id),
                            );
                        } else {
                            let _ = navigation_proxy.send_event(BrowserEvent::ResponsesResult {
                                request_id,
                                revision: None,
                                result: Err("ChatGPT tried to leave Temporary Chat. Use /web login in the terminal if your session has expired.".into()),
                            });
                        }
                    }
                }
                return temporary;
            }
            true
        })
        .with_ipc_handler(move |request| {
            let body = request.body().as_str();
            let tier = match body {
                "free-go" => Some(AccountTierChoice::FreeGo),
                "plus" => Some(AccountTierChoice::Plus),
                "pro" => Some(AccountTierChoice::Pro),
                _ => None,
            };
            if let Some(tier) = tier.filter(|_| page == BrowserPage::AccountChooser) {
                let _ = event_proxy.send_event(BrowserEvent::ContinueLogin(tier));
                return;
            }
            let Ok(message) = serde_json::from_str::<serde_json::Value>(body) else {
                return;
            };
            if request.uri().scheme_str() != Some("https")
                || request.uri().host() != Some("chatgpt.com")
                || request.uri().port_u16().is_some_and(|port| port != 443)
            {
                return;
            }
            if message.get("type").and_then(serde_json::Value::as_str)
                == Some("set-power-slider")
            {
                if let (Some(index), Some(request_id)) = (message
                    .get("index")
                    .and_then(serde_json::Value::as_u64)
                    .filter(|index| *index <= 2), message
                    .get("request_id")
                    .and_then(serde_json::Value::as_str)
                    .filter(|request_id| !request_id.is_empty() && request_id.len() <= 96)
                    .map(str::to_owned))
                {
                    let _ = event_proxy.send_event(BrowserEvent::SetPowerSliderValue {
                        view_id: ipc_view_id.clone(),
                        request_id,
                        index: index as u8,
                        allow_focus: power_slider_focus_requested(&message),
                    });
                }
                return;
            }
            match message.get("type").and_then(serde_json::Value::as_str) {
                Some("responses-document-started") | Some("responses-bind-rejected") => {
                    let Some(binding) = ipc_response_id.as_ref() else { return; };
                    let Some(request_id) = binding.lock().ok().and_then(|state| state.clone()) else { return; };
                    let reported_id = message.get("request_id").and_then(serde_json::Value::as_str);
                    let document_reloaded = message.get("type").and_then(serde_json::Value::as_str) == Some("responses-document-started");
                    let error = if document_reloaded && reported_id.is_some_and(|id| id != request_id) {
                        "The retained Temporary Chat reloaded during a request. Check the previous turn before retrying; the next turn will open a fresh Temporary Chat."
                    } else if !document_reloaded && reported_id == Some(request_id.as_str()) {
                        "ChatGPT response bridge rejected the retained-page binding. The next turn will open a fresh Temporary Chat."
                    } else {
                        return;
                    };
                    let _ = event_proxy.send_event(BrowserEvent::ResponsesBridgeUnavailable {
                        request_id,
                        binding: binding.clone(),
                        document_reloaded,
                        error: error.to_owned(),
                    });
                    return;
                }
                Some("responses-bridge-ready")
                | Some("responses-prompt-submitted")
                | Some("responses-auth-required")
                | Some("responses-tool-state")
                | Some("responses-text")
                | Some("responses-reasoning-heading")
                | Some("responses-result") => {
                    let Some(request_id) = ipc_response_id
                        .as_ref()
                        .and_then(|state| state.lock().ok().and_then(|state| state.clone()))
                    else {
                        return;
                    };
                    if message
                        .get("request_id")
                        .and_then(serde_json::Value::as_str)
                        != Some(request_id.as_str())
                    {
                        return;
                    }
                    match message.get("type").and_then(serde_json::Value::as_str) {
                        Some("responses-bridge-ready") => {
                            let _ = event_proxy.send_event(BrowserEvent::ResponsesBridgeReady {
                                request_id: request_id.clone(),
                                conversation_has_history: message
                                    .get("conversation_has_history")
                                    .and_then(serde_json::Value::as_bool)
                                    .unwrap_or(false),
                            });
                        }
                        Some("responses-prompt-submitted") => {
                            let _ = event_proxy.send_event(
                                BrowserEvent::ResponsesPromptSubmitted(request_id.clone()),
                            );
                        }
                        Some("responses-auth-required") => {
                            let _ = event_proxy.send_event(BrowserEvent::ResponsesAuthRequired(
                                request_id.clone(),
                            ));
                        }
                        Some("responses-tool-state") => {
                            let _ = event_proxy
                                .send_event(BrowserEvent::ResponsesToolState(request_id.clone()));
                        }
                        Some("responses-text") => {
                            if let Some(text) = message
                                .get("delta")
                                .and_then(serde_json::Value::as_str)
                                .filter(|text| text.len() <= MAX_RESPONSE_TEXT_BYTES)
                            {
                                let _ = event_proxy.send_event(BrowserEvent::ResponsesText {
                                    request_id: request_id.clone(),
                                    delta: text.to_owned(),
                                });
                            }
                        }
                        Some("responses-reasoning-heading") => {
                            if let Some(heading) = message
                                .get("heading")
                                .and_then(serde_json::Value::as_str)
                                .filter(|heading| heading.len() <= 1024)
                            {
                                let _ = event_proxy.send_event(
                                    BrowserEvent::ResponsesReasoningHeading {
                                        request_id: request_id.clone(),
                                        heading: heading.to_owned(),
                                    },
                                );
                            }
                        }
                        _ => {
                            let result = if let Some(text) = message
                                .get("text")
                                .and_then(serde_json::Value::as_str)
                                .filter(|text| text.len() <= MAX_RESPONSE_TEXT_BYTES)
                            {
                                Ok(text.to_owned())
                            } else {
                                Err(message
                                    .get("error")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or("ChatGPT returned an invalid or oversized response")
                                    .chars()
                                    .take(1024)
                                    .collect())
                            };
                            let _ = event_proxy.send_event(BrowserEvent::ResponsesResult {
                                request_id: request_id.clone(),
                                revision: message
                                    .get("tool_revision")
                                    .and_then(serde_json::Value::as_u64),
                                result,
                            });
                        }
                    }
                    return;
                }
                Some("account-capabilities") => {}
                _ => return,
            }
            if message
                .get("probe_version")
                .and_then(serde_json::Value::as_u64)
                != Some(5)
            {
                return;
            }
            let Some(picker_options) = message
                .get("picker_options")
                .and_then(serde_json::Value::as_array)
            else {
                return;
            };
            if picker_options.len() > 40 {
                return;
            }
            let picker_options = picker_options
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|option| !option.is_empty() && option.len() <= 120)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            // A background turn can probe while ChatGPT is still loading or
            // temporarily unavailable. Its empty result must not erase the
            // account's model list while other turns are running.
            if ipc_response_id.is_some() && picker_options.is_empty() {
                return;
            }
            let detected_at_unix_seconds = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let reasoning_by_model = message
                .get("reasoning_by_model")
                .and_then(serde_json::Value::as_object)
                .into_iter()
                .flatten()
                .filter(|(label, _)| picker_options.contains(label))
                .filter_map(|(label, levels)| {
                    let levels = levels.as_array()?;
                    if levels.len() > 5 {
                        return None;
                    }
                    let levels = levels
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .filter(|level| {
                            matches!(*level, "low" | "medium" | "high" | "xhigh" | "max")
                        })
                        .map(str::to_owned)
                        .collect::<Vec<_>>();
                    (!levels.is_empty()).then(|| (label.clone(), levels))
                })
                .collect();
            let _ = event_proxy.send_event(BrowserEvent::AccountCapabilities(
                AccountCapabilities {
                    probe_version: 5,
                    picker_options,
                    reasoning_by_model,
                    detected_at_unix_seconds,
                },
                ipc_response_id.is_some(),
            ));
        });

    let builder = if page == BrowserPage::TemporaryChat && response_id.is_some() {
        let builder = builder.with_initialization_script(TEMPORARY_CHAT_ENFORCEMENT_SCRIPT);
        if let Some(request_id) = response_id {
            let connector_name = data_directory()
                .ok()
                .and_then(|path| fs::read_to_string(path.join("connector-name.txt")).ok())
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "Web2Codex".to_owned());
            builder
                .with_initialization_script(&format!(
                    "if (window === window.top && location.origin === 'https://chatgpt.com') {{ window.__web2codexRequestId={}; window.__web2codexConnectorName={}; window.ipc.postMessage(JSON.stringify({{type:'responses-document-started', request_id:window.__web2codexRequestId}})); }}",
                    serde_json::json!(request_id), serde_json::json!(connector_name)
                ))
                .with_initialization_script(RESPONSES_BRIDGE_SCRIPT)
        } else {
            builder
        }
    } else {
        builder
    };

    let builder = match page {
        BrowserPage::SignIn => builder.with_url(CHATGPT_SIGN_IN_URL),
        BrowserPage::TemporaryChat => builder.with_url(CHATGPT_TEMPORARY_CHAT_URL),
        BrowserPage::AccountChooser => builder.with_html(ACCOUNT_CHOOSER_HTML),
    };

    builder
        .build_as_child(window)
        .context(WEBVIEW_CREATION_CONTEXT)
}

fn is_chatgpt_auth_url(target: &str) -> bool {
    let Ok(url) = url::Url::parse(target) else {
        return false;
    };
    if url.scheme() != "https" || url.port_or_known_default() != Some(443) {
        return false;
    }
    let path = url.path().to_ascii_lowercase();
    match url.host_str() {
        Some("auth.openai.com" | "auth0.openai.com" | "login.openai.com") => true,
        Some("chatgpt.com" | "chat.openai.com") => {
            path.starts_with("/auth/")
                || matches!(
                    path.as_str(),
                    "/login" | "/log-in" | "/signin" | "/sign-in" | "/create-account"
                )
        }
        Some("openai.com") => path.starts_with("/auth/") || path == "/log-in",
        _ => false,
    }
}

fn store_account_tier(path: &Path, tier: AccountTierChoice) -> anyhow::Result<()> {
    write_private_json(path, &tier)
}

fn store_account_capabilities(
    path: &Path,
    mut capabilities: AccountCapabilities,
    partial: bool,
) -> anyhow::Result<()> {
    if partial
        && let Ok(contents) = fs::read(path)
        && let Ok(previous) = serde_json::from_slice::<AccountCapabilities>(&contents)
        && previous.probe_version == capabilities.probe_version
    {
        if capabilities.picker_options.is_empty() {
            capabilities.picker_options = previous.picker_options.clone();
        }
        for (model, levels) in previous.reasoning_by_model {
            if capabilities.picker_options.contains(&model) {
                capabilities
                    .reasoning_by_model
                    .entry(model)
                    .or_insert(levels);
            }
        }
    }
    write_private_json(path, &capabilities)
}

fn prepare_profile_directory(profile_dir: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(profile_dir).with_context(|| {
        format!(
            "failed to create the private browser profile at {}",
            profile_dir.display()
        )
    })?;
    restrict_permissions(profile_dir)
}

fn acquire_lock(lock_path: &Path, state_path: &Path) -> anyhow::Result<PathBuf> {
    for attempt in 0..50 {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)
        {
            Ok(mut lock) => {
                writeln!(lock, "{}", std::process::id())?;
                restrict_file_permissions(lock_path)?;
                return Ok(lock_path.to_path_buf());
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if send_control(state_path, "/status").is_ok() {
                    anyhow::bail!("another Web2Codex browser process is already running");
                }
                thread::sleep(Duration::from_millis(100));
                if attempt == 49 {
                    let stale = fs::metadata(lock_path)
                        .and_then(|metadata| metadata.modified())
                        .ok()
                        .and_then(|modified| modified.elapsed().ok())
                        .is_some_and(|age| age >= STALE_LOCK_AGE);
                    if stale {
                        let _ = fs::remove_file(state_path);
                        let _ = fs::remove_file(lock_path);
                    } else {
                        anyhow::bail!("another Web2Codex browser process is still starting");
                    }
                }
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to create Web2Codex browser lock file {}",
                        lock_path.display()
                    )
                });
            }
        }
    }

    let mut lock = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(lock_path)
        .with_context(|| {
            format!(
                "could not acquire Web2Codex browser lock {}",
                lock_path.display()
            )
        })?;
    writeln!(lock, "{}", std::process::id())?;
    restrict_file_permissions(lock_path)?;
    Ok(lock_path.to_path_buf())
}

fn write_state(state_path: &Path, state: &BrowserState) -> anyhow::Result<()> {
    write_private_json(state_path, state)
}

/// Publish a complete snapshot so concurrent CLI readers never observe a
/// truncated catalog or half-written bearer credential.
fn write_private_json(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    let parent = path
        .parent()
        .context("browser snapshot has no parent directory")?;
    let temporary = parent.join(format!(".web2codex-{}.tmp", Uuid::new_v4().simple()));
    let result = (|| -> anyhow::Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.with_context(|| format!("failed to publish browser state at {}", path.display()))
}

fn serve_control_requests(
    listener: TcpListener,
    token: String,
    account_capabilities_path: PathBuf,
    event_proxy: EventLoopProxy<BrowserEvent>,
    broker: Arc<TurnBroker>,
) {
    for connection in listener.incoming() {
        let Ok(stream) = connection else {
            continue;
        };
        let token = token.clone();
        let account_capabilities_path = account_capabilities_path.clone();
        let event_proxy = event_proxy.clone();
        let broker = Arc::clone(&broker);
        let _ = thread::Builder::new()
            .name("codex-web-request".to_string())
            .spawn(move || {
                handle_control_request(
                    stream,
                    &token,
                    &account_capabilities_path,
                    &event_proxy,
                    broker,
                );
            });
    }
}

fn handle_control_request(
    mut stream: TcpStream,
    token: &str,
    account_capabilities_path: &Path,
    event_proxy: &EventLoopProxy<BrowserEvent>,
    broker: Arc<TurnBroker>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(INFERENCE_TIMEOUT + Duration::from_secs(5)));
    let request = match parse_http_request(&mut stream) {
        Ok(request) => request,
        Err(error) => {
            let status = if error.to_string().contains("request body exceeds") {
                "413 Payload Too Large"
            } else {
                "400 Bad Request"
            };
            let code = if status.starts_with("413") {
                "request_too_large"
            } else {
                "malformed_request"
            };
            let _ = write_json_response(
                &mut stream,
                status,
                &json_error(
                    "invalid_request_error",
                    code,
                    "invalid or oversized HTTP request",
                ),
            );
            return;
        }
    };
    if request.authorization != format!("Bearer {token}") {
        let _ = write_json_response(
            &mut stream,
            "401 Unauthorized",
            &json_error(
                "authentication_error",
                "invalid_api_key",
                "valid local browser bearer token required",
            ),
        );
        return;
    }

    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/models") => {
            let catalog = model_catalog_from_picker_snapshot(account_capabilities_path);
            let _ = write_json_response(&mut stream, "200 OK", &catalog);
        }
        ("GET", "/browser/tabs") => {
            let (reply, received) = mpsc::sync_channel(1);
            if event_proxy
                .send_event(BrowserEvent::ListTabs(reply))
                .is_err()
            {
                let _ = write_response(&mut stream, "503 Service Unavailable", "browser stopped");
                return;
            }
            match received.recv_timeout(Duration::from_secs(5)) {
                Ok(tabs) => {
                    let _ = write_json_response(
                        &mut stream,
                        "200 OK",
                        &serde_json::json!({"tabs":tabs}),
                    );
                }
                Err(_) => {
                    let _ = write_response(&mut stream, "504 Gateway Timeout", "browser timed out");
                }
            }
        }
        ("POST", "/browser/tabs") => {
            let body = serde_json::from_slice::<serde_json::Value>(&request.body).ok();
            if body
                .as_ref()
                .and_then(|value| value.get("action"))
                .and_then(serde_json::Value::as_str)
                == Some("close-agent")
            {
                let thread_id = body
                    .as_ref()
                    .and_then(|value| value.get("thread_id"))
                    .and_then(serde_json::Value::as_str)
                    .filter(|thread_id| !thread_id.trim().is_empty() && thread_id.len() <= 256);
                let Some(thread_id) = thread_id else {
                    let _ = write_json_response(
                        &mut stream,
                        "400 Bad Request",
                        &json_error(
                            "invalid_request_error",
                            "invalid_agent_thread_id",
                            "A non-empty agent thread ID is required",
                        ),
                    );
                    return;
                };
                let agent_path = body
                    .as_ref()
                    .and_then(|value| value.get("agent_path"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|path| !path.is_empty() && path.len() <= 256)
                    .map(str::to_owned);
                let (reply, received) = mpsc::sync_channel(1);
                if event_proxy
                    .send_event(BrowserEvent::CloseAgentTab(
                        thread_id.to_owned(),
                        agent_path,
                        reply,
                    ))
                    .is_err()
                {
                    let _ =
                        write_response(&mut stream, "503 Service Unavailable", "browser stopped");
                    return;
                }
                match received.recv_timeout(Duration::from_secs(5)) {
                    Ok(AgentTabCloseOutcome::Closed) => {
                        let _ = write_response(&mut stream, "204 No Content", "");
                    }
                    Ok(AgentTabCloseOutcome::Deferred) => {
                        let _ = write_response(
                            &mut stream,
                            "202 Accepted",
                            "close deferred until the agent request finishes",
                        );
                    }
                    Ok(AgentTabCloseOutcome::NotFound) => {
                        let _ =
                            write_response(&mut stream, "404 Not Found", "agent tab was not found");
                    }
                    Err(_) => {
                        let _ =
                            write_response(&mut stream, "504 Gateway Timeout", "browser timed out");
                    }
                }
                return;
            }
            let url = body.and_then(|value| {
                value
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .and_then(browser_address)
            });
            let Some(url) = url else {
                let _ = write_json_response(
                    &mut stream,
                    "400 Bad Request",
                    &json_error(
                        "invalid_request_error",
                        "invalid_url",
                        "A valid HTTP or HTTPS URL is required",
                    ),
                );
                return;
            };
            let (reply, received) = mpsc::sync_channel(1);
            if event_proxy
                .send_event(BrowserEvent::BackgroundTab(url, reply))
                .is_err()
            {
                let _ = write_response(&mut stream, "503 Service Unavailable", "browser stopped");
                return;
            }
            match received.recv_timeout(Duration::from_secs(15)) {
                Ok(Ok(index)) => {
                    let _ = write_json_response(
                        &mut stream,
                        "200 OK",
                        &serde_json::json!({"index":index}),
                    );
                }
                Ok(Err(error)) => {
                    let _ = write_json_response(
                        &mut stream,
                        "500 Internal Server Error",
                        &json_error("server_error", "tab_creation_failed", &error),
                    );
                }
                Err(_) => {
                    let _ = write_response(&mut stream, "504 Gateway Timeout", "browser timed out");
                }
            }
        }
        ("POST", "/status") => {
            let _ = write_response(&mut stream, "204 No Content", "");
        }
        ("POST", "/lease") => {
            // EOF also covers a crashed process or a closed terminal window.
            let (ack, accepted) = mpsc::sync_channel(0);
            if event_proxy
                .send_event(BrowserEvent::ClientConnected(ack))
                .is_err()
                || accepted.recv_timeout(Duration::from_secs(5)).is_err()
            {
                let _ = write_response(&mut stream, "503 Service Unavailable", "browser stopped");
                return;
            }
            if stream.set_read_timeout(None).is_ok()
                && stream.write_all(b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: web2codex-lease\r\n\r\n").is_ok()
            {
                let mut byte = [0_u8; 1];
                loop {
                    match stream.read(&mut byte) {
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        _ => break,
                    }
                }
            }
            let _ = event_proxy.send_event(BrowserEvent::ClientDisconnected);
        }
        ("POST", "/open" | "/login" | "/session/open" | "/session/login") => {
            let (reply, result) = mpsc::sync_channel(1);
            let page = if request.path.ends_with("/login") {
                BrowserPage::AccountChooser
            } else {
                BrowserPage::TemporaryChat
            };
            if event_proxy
                .send_event(BrowserEvent::Open(
                    page,
                    request.path.starts_with("/session/"),
                    Some(reply),
                ))
                .is_err()
            {
                let _ = write_response(&mut stream, "503 Service Unavailable", "browser stopped");
                return;
            }
            match result.recv_timeout(Duration::from_secs(15)) {
                Ok(Ok(())) => {
                    let _ = write_response(&mut stream, "204 No Content", "");
                }
                Ok(Err(error)) => {
                    let _ = write_response(&mut stream, "500 Internal Server Error", &error);
                }
                Err(_) => {
                    let _ = write_response(&mut stream, "504 Gateway Timeout", "browser timed out");
                }
            }
        }
        ("POST", "/hide") => {
            let status = if event_proxy.send_event(BrowserEvent::Hide).is_ok() {
                "204 No Content"
            } else {
                "503 Service Unavailable"
            };
            let _ = write_response(&mut stream, status, "");
        }
        ("POST", "/stop") => {
            let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
            if let Err(error) = event_proxy.send_event(BrowserEvent::Stop(Some(stream)))
                && let BrowserEvent::Stop(Some(mut stream)) = error.0
            {
                let _ = write_response(&mut stream, "503 Service Unavailable", "browser stopped");
            }
        }
        ("POST", "/v1/responses") => {
            responses_http::handle(&mut stream, &request, event_proxy, &broker);
        }
        ("POST", "/bridge/tools" | "/bridge/invoke" | "/bridge/tool-result") => {
            responses_http::handle_connector(&mut stream, &request, &broker);
        }
        _ => {
            let _ = write_json_response(
                &mut stream,
                "404 Not Found",
                &json_error("invalid_request_error", "not_found", "route not found"),
            );
        }
    }
}

fn client_connection_closed(stream: &TcpStream) -> bool {
    if stream.set_nonblocking(true).is_err() {
        return false;
    }
    let mut probe = [0_u8; 1];
    let closed = match stream.peek(&mut probe) {
        // EOF only means the peer stopped sending. It may still be reading
        // the HTTP response; a closed receive side is detected by writes.
        Err(error) => !matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
        ),
        _ => false,
    };
    let _ = stream.set_nonblocking(false);
    closed
}

struct TextOnlyResponsesRequest {
    model: String,
    conversation_id: Option<String>,
    conversation_epoch: Option<String>,
    agent: Option<AgentRequestIdentity>,
    setup_smoke_test: bool,
    compaction_stages: Vec<CompactionStage>,
    reasoning_effort: Option<String>,
    connector_tool_choice: ToolChoice,
    prompt: String,
    resume_prompt: String,
    attachments: Vec<MediaAttachment>,
    resume_attachments: Vec<MediaAttachment>,
    interruption_prompt: Option<String>,
    interruption_attachment_names: Vec<String>,
    stream: bool,
    tools: Vec<CallableTool>,
    connector_required: bool,
    mcp_required: bool,
    mcp_action_required: bool,
    tool_work_expected: bool,
    tool_choice: HarnessToolChoice,
    outputs: Vec<(String, serde_json::Value)>,
    expects_continuation: bool,
}

struct CompactionStage {
    prompt: String,
    acknowledgement: String,
}

type RequestParseError = (&'static str, &'static str, String);

fn supported_attachment_extension(kind: &str, mime: &str) -> Option<&'static str> {
    match (kind, mime) {
        ("image", "image/png") => Some("png"),
        ("image", "image/jpeg") => Some("jpg"),
        ("image", "image/webp") => Some("webp"),
        ("image", "image/gif") => Some("gif"),
        ("audio", "audio/wav" | "audio/x-wav") => Some("wav"),
        ("audio", "audio/mpeg" | "audio/mp3") => Some("mp3"),
        ("audio", "audio/mp4" | "audio/x-m4a") => Some("m4a"),
        ("audio", "audio/aac") => Some("aac"),
        ("audio", "audio/ogg") => Some("ogg"),
        ("audio", "audio/webm") => Some("webm"),
        ("audio", "audio/flac" | "audio/x-flac") => Some("flac"),
        _ => None,
    }
}

fn append_data_url_attachment(
    data_url: Option<&str>,
    kind: &str,
    attachments: &mut Vec<MediaAttachment>,
    text: &mut String,
) -> Result<(), RequestParseError> {
    let (status, code, label) = match kind {
        "image" => (
            "422 Unprocessable Content",
            "input_image_unsupported",
            "image",
        ),
        _ => (
            "422 Unprocessable Content",
            "input_audio_unsupported",
            "audio",
        ),
    };
    let unsupported = |message: &str| (status, code, message.to_string());
    let data_url = data_url.ok_or(unsupported(match kind {
        "image" => "images must contain an inline data URL",
        _ => "audio must contain an inline data URL",
    }))?;
    let (header, data) = data_url
        .split_once(',')
        .ok_or(unsupported("attachments must use a base64 data URL"))?;
    let mime = header
        .strip_prefix("data:")
        .and_then(|header| header.strip_suffix(";base64"))
        .ok_or(unsupported("attachments must use a base64 data URL"))?;
    let extension = supported_attachment_extension(kind, mime).ok_or(unsupported(match kind {
        "image" => "use an inline PNG, JPEG, WebP, or GIF image",
        _ => "use an inline WAV, MP3, M4A, AAC, OGG, WebM, or FLAC audio file",
    }))?;
    if data.is_empty() {
        return Err(unsupported(
            "media attachments must contain non-empty base64 data",
        ));
    }
    let name = if let Some(existing) = attachments.iter().find(|file| file.data_url == data_url) {
        existing.name.clone()
    } else {
        if attachments.len() >= 8 {
            return Err(unsupported(
                "provide up to eight distinct media attachments",
            ));
        }
        let name = format!("web2codex-{label}-{}.{}", attachments.len() + 1, extension);
        attachments.push(MediaAttachment {
            name: name.clone(),
            mime: mime.to_string(),
            data_url: data_url.to_string(),
        });
        name
    };
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(&format!("[Attached {label}: {name}]"));
    Ok(())
}

fn valid_mime_type(mime: &str) -> bool {
    let Some((type_, subtype)) = mime.split_once('/') else {
        return false;
    };
    !type_.is_empty()
        && !subtype.is_empty()
        && !subtype.contains('/')
        && [type_, subtype].iter().all(|part| {
            part.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(byte, b'!' | b'#'..=b'$' | b'&'..=b'\'' | b'*' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~')
            })
        })
}

fn safe_attachment_filename(filename: &str, fallback_index: usize) -> String {
    let basename = filename.rsplit(['/', '\\']).next().unwrap_or_default();
    let sanitized = basename
        .chars()
        .filter(|character| {
            character.is_alphanumeric() || matches!(character, ' ' | '.' | '-' | '_')
        })
        .take(120)
        .collect::<String>();
    let sanitized = sanitized.trim().trim_matches('.');
    if sanitized.is_empty() {
        format!("web2codex-file-{fallback_index}.bin")
    } else {
        sanitized.to_string()
    }
}

fn append_data_url_file(
    data_url: Option<&str>,
    filename: Option<&str>,
    attachments: &mut Vec<MediaAttachment>,
    text: &mut String,
) -> Result<(), RequestParseError> {
    let unsupported = |message: &str| {
        (
            "422 Unprocessable Content",
            "input_file_unsupported",
            message.to_string(),
        )
    };
    let data_url = data_url.ok_or(unsupported(
        "file attachments must contain an inline data URL in a Temporary Chat",
    ))?;
    let (header, data) = data_url
        .split_once(',')
        .ok_or(unsupported("file attachments must use a base64 data URL"))?;
    let header = header
        .strip_prefix("data:")
        .filter(|header| header.ends_with(";base64"))
        .ok_or(unsupported("file attachments must use a base64 data URL"))?;
    let mime = header
        .strip_suffix(";base64")
        .and_then(|header| header.split(';').next())
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if !valid_mime_type(&mime) {
        return Err(unsupported(
            "file attachments must include a valid MIME type",
        ));
    }
    if data.is_empty() {
        return Err(unsupported(
            "file attachments must contain non-empty base64 data",
        ));
    }

    let existing = attachments.iter().find(|file| file.data_url == data_url);
    let name = if let Some(existing) = existing {
        existing.name.clone()
    } else {
        if attachments.len() >= 8 {
            return Err(unsupported("provide up to eight distinct attachments"));
        }
        let name = safe_attachment_filename(filename.unwrap_or_default(), attachments.len() + 1);
        attachments.push(MediaAttachment {
            name: name.clone(),
            mime,
            data_url: data_url.to_string(),
        });
        name
    };
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(&format!("[Attached file: {name}]"));
    Ok(())
}

fn attach_tool_media(
    kind: &str,
    mime: &str,
    data: &str,
    attachments: &mut Vec<MediaAttachment>,
) -> Option<String> {
    supported_attachment_extension(kind, mime)?;
    if data.is_empty() {
        return None;
    }
    let data_url = format!("data:{mime};base64,{data}");
    let mut marker = String::new();
    append_data_url_attachment(Some(&data_url), kind, attachments, &mut marker).ok()?;
    Some(marker)
}

fn attach_tool_file(
    mime: &str,
    data: &str,
    filename: &str,
    attachments: &mut Vec<MediaAttachment>,
) -> Option<String> {
    if data.is_empty() {
        return None;
    }
    let data_url = format!("data:{mime};base64,{data}");
    let mut marker = String::new();
    append_data_url_file(Some(&data_url), Some(filename), attachments, &mut marker).ok()?;
    Some(marker)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CallableToolKind {
    Function,
    Custom,
    ToolSearch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ToolChoice {
    Auto,
    None,
    Required,
    Named {
        kind: CallableToolKind,
        namespace: String,
        name: String,
    },
}

fn parse_tool_choice(
    value: Option<&serde_json::Value>,
    tools: &[CallableTool],
) -> Result<ToolChoice, RequestParseError> {
    let unsupported = |message: &str| {
        (
            "422 Unprocessable Content",
            "tool_choice_unsupported",
            message.to_owned(),
        )
    };
    let Some(value) = value else {
        return Ok(ToolChoice::Auto);
    };
    if let Some(choice) = value.as_str() {
        return match choice {
            "auto" => Ok(ToolChoice::Auto),
            "none" => Ok(ToolChoice::None),
            "required" if !tools.is_empty() => Ok(ToolChoice::Required),
            "required" => Err(unsupported(
                "tool_choice required needs at least one callable tool",
            )),
            _ => Err(unsupported(
                "tool_choice must be auto, none, required, or a named tool",
            )),
        };
    }
    let kind = match value.get("type").and_then(serde_json::Value::as_str) {
        Some("function") => CallableToolKind::Function,
        Some("custom") => CallableToolKind::Custom,
        _ => {
            return Err(unsupported(
                "named tool_choice must target a function or custom tool",
            ));
        }
    };
    let name = value
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.trim().is_empty() && name.len() <= 257)
        .ok_or(unsupported(
            "named tool_choice must include a bounded tool name",
        ))?;
    let matches = tools
        .iter()
        .filter(|tool| {
            tool.kind == kind
                && (tool.name == name || format!("{}.{}", tool.namespace, tool.name) == name)
        })
        .collect::<Vec<_>>();
    let [tool] = matches.as_slice() else {
        return Err(unsupported(if matches.is_empty() {
            "named tool_choice does not match a declared callable tool"
        } else {
            "named tool_choice is ambiguous; use its namespace-qualified name"
        }));
    };
    Ok(ToolChoice::Named {
        kind,
        namespace: tool.namespace.clone(),
        name: tool.name.clone(),
    })
}

#[derive(Clone)]
struct CallableTool {
    kind: CallableToolKind,
    namespace: String,
    name: String,
    declaration: serde_json::Value,
}

enum HarnessToolChoice {
    Auto,
    None,
    Required,
    Named {
        kind: CallableToolKind,
        namespace: String,
        name: String,
    },
}

impl HarnessToolChoice {
    fn requires_tool_work(&self) -> bool {
        matches!(self, Self::Required | Self::Named { .. })
    }

    fn select(
        value: Option<&serde_json::Value>,
        tools: &mut Vec<CallableTool>,
    ) -> Result<Self, RequestParseError> {
        let invalid = |message: &str| {
            (
                "422 Unprocessable Content",
                "tool_choice_unsupported",
                message.to_string(),
            )
        };
        let Some(value) = value.filter(|value| !value.is_null()) else {
            return Ok(Self::Auto);
        };
        match value.as_str() {
            Some("auto") => return Ok(Self::Auto),
            Some("none") => {
                tools.clear();
                return Ok(Self::None);
            }
            Some("required") if tools.is_empty() => {
                return Err(invalid(
                    "required tool choice needs a callable harness tool",
                ));
            }
            Some("required") => return Ok(Self::Required),
            _ => {}
        }

        let kind = match value["type"].as_str() {
            Some("function") => CallableToolKind::Function,
            Some("custom") => CallableToolKind::Custom,
            _ => {
                return Err(invalid(
                    "expected auto, none, required, or a named function/custom tool",
                ));
            }
        };
        let name = value["name"]
            .as_str()
            .ok_or(invalid("named tool choice requires a name"))?;
        let namespace = value
            .get("namespace")
            .map(|namespace| {
                namespace
                    .as_str()
                    .ok_or(invalid("tool choice namespace must be a string"))
            })
            .transpose()?;
        tools.retain(|tool| {
            tool.kind == kind
                && tool.name == name
                && namespace.is_none_or(|namespace| namespace == tool.namespace.as_str())
        });
        let [selected] = tools.as_slice() else {
            return Err(invalid(
                "named tool choice must match exactly one declared tool",
            ));
        };
        Ok(Self::Named {
            kind,
            namespace: selected.namespace.clone(),
            name: selected.name.clone(),
        })
    }

    fn instructions(&self) -> &'static str {
        match self {
            Self::Auto => "Use the selected Web2Codex connector when tools are needed.",
            Self::None => "Answer directly without calling tools.",
            Self::Required | Self::Named { .. } => {
                "Use the selected Web2Codex connector to discover and invoke an available harness tool before completing this request."
            }
        }
    }

    // A parallel call queued by the previous response may predate this response's
    // filtered registry. Recheck it before handing it to the native executor.
    fn check_output(&self, item: &serde_json::Value, end_turn: bool) -> Result<(), &'static str> {
        match self {
            Self::None if !end_turn => Err("This response disabled native tool calls"),
            Self::Required | Self::Named { .. } if end_turn => {
                Err("ChatGPT completed the response without making the required native tool call")
            }
            Self::Named {
                kind,
                namespace,
                name,
            } if item["namespace"].as_str() != Some(namespace.as_str())
                || item["name"].as_str() != Some(name.as_str())
                || item["type"].as_str()
                    != Some(match kind {
                        CallableToolKind::Function => "function_call",
                        CallableToolKind::Custom => "custom_tool_call",
                        CallableToolKind::ToolSearch => "tool_search_call",
                    }) =>
            {
                Err("The pending call does not match this response's selected tool")
            }
            _ => Ok(()),
        }
    }
}

enum BridgeCompletion {
    Message(String),
    FunctionCall {
        namespace: String,
        name: String,
        arguments: serde_json::Value,
    },
    CustomToolCall {
        namespace: String,
        name: String,
        input: String,
    },
    ToolSearchCall {
        arguments: serde_json::Value,
    },
}

fn collect_callable_tool(
    tools: &mut Vec<CallableTool>,
    kind: CallableToolKind,
    namespace: &str,
    spec: &serde_json::Value,
) -> Result<(), RequestParseError> {
    let unsupported =
        |code, message: &str| ("422 Unprocessable Content", code, message.to_string());
    let name = spec
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty() && name.len() <= 128)
        .ok_or(unsupported(
            "tool_schema_unsupported",
            "callable tool schemas must include a bounded name",
        ))?;
    if namespace.is_empty() || namespace.len() > 128 {
        return Err(unsupported(
            "tool_schema_unsupported",
            "callable tool namespaces must be non-empty and bounded",
        ));
    }
    let description = spec
        .get("description")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let declaration = match kind {
        CallableToolKind::Function => {
            let parameters = spec.get("parameters").cloned().ok_or(unsupported(
                "tool_schema_unsupported",
                "function tool schemas must include parameters",
            ))?;
            serde_json::json!({
                "kind": "function",
                "namespace": namespace,
                "name": name,
                "description": description,
                "parameters": parameters,
                "strict": spec.get("strict").cloned().unwrap_or(serde_json::Value::Bool(false)),
            })
        }
        CallableToolKind::Custom => {
            let format = spec.get("format").cloned().ok_or(unsupported(
                "tool_schema_unsupported",
                "custom tool schemas must include a format",
            ))?;
            serde_json::json!({
                "kind": "custom",
                "namespace": namespace,
                "name": name,
                "description": description,
                "format": format,
            })
        }
        CallableToolKind::ToolSearch => {
            if spec.get("execution").and_then(serde_json::Value::as_str) != Some("client") {
                return Err(unsupported(
                    "tool_schema_unsupported",
                    "only client-side tool search can be routed by this bridge",
                ));
            }
            let parameters = spec.get("parameters").cloned().ok_or(unsupported(
                "tool_schema_unsupported",
                "tool search schemas must include parameters",
            ))?;
            serde_json::json!({
                "kind": "tool_search",
                "namespace": namespace,
                "name": name,
                "description": description,
                "parameters": parameters,
            })
        }
    };
    tools.push(CallableTool {
        kind,
        namespace: namespace.to_string(),
        name: name.to_string(),
        declaration,
    });
    Ok(())
}

fn parse_callable_tools(
    value: Option<&serde_json::Value>,
) -> Result<Vec<CallableTool>, RequestParseError> {
    let unsupported =
        |code, message: &str| ("422 Unprocessable Content", code, message.to_string());
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(Vec::new());
    };
    let Some(specs) = value.as_array() else {
        return Err(unsupported(
            "tool_schema_unsupported",
            "tools must be an array",
        ));
    };
    if specs.len() > MAX_TOOL_COUNT {
        return Err(unsupported(
            "too_many_tools",
            "the request contains more tool schemas than this bridge can safely accept",
        ));
    }
    let serialized = serde_json::to_vec(specs).map_err(|_| {
        unsupported(
            "tool_schema_unsupported",
            "tool schemas could not be serialized",
        )
    })?;
    if serialized.len() > MAX_TOOL_SCHEMAS_BYTES {
        return Err((
            "413 Payload Too Large",
            "tool_schemas_too_large",
            "tool declarations exceed the WebView bridge size limit".to_string(),
        ));
    }

    let mut tools = Vec::new();
    for spec in specs {
        match spec.get("type").and_then(serde_json::Value::as_str) {
            Some("function") => {
                collect_callable_tool(&mut tools, CallableToolKind::Function, "functions", spec)?
            }
            Some("custom") => {
                collect_callable_tool(&mut tools, CallableToolKind::Custom, "functions", spec)?
            }
            Some("tool_search") => {
                let mut search_spec = spec.clone();
                if let Some(object) = search_spec.as_object_mut() {
                    object.insert(
                        "name".to_string(),
                        serde_json::Value::String("tool_search".to_string()),
                    );
                }
                collect_callable_tool(
                    &mut tools,
                    CallableToolKind::ToolSearch,
                    "functions",
                    &search_spec,
                )?;
            }
            Some("namespace") => {
                let namespace = spec
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .filter(|name| !name.is_empty() && name.len() <= 128)
                    .ok_or(unsupported(
                        "tool_schema_unsupported",
                        "tool namespaces must include a bounded name",
                    ))?;
                let nested = spec
                    .get("tools")
                    .and_then(serde_json::Value::as_array)
                    .ok_or(unsupported(
                        "tool_schema_unsupported",
                        "tool namespaces must include a tool array",
                    ))?;
                for tool in nested {
                    let kind = match tool.get("type").and_then(serde_json::Value::as_str) {
                        Some("function") => CallableToolKind::Function,
                        Some("custom") => CallableToolKind::Custom,
                        _ => continue,
                    };
                    collect_callable_tool(&mut tools, kind, namespace, tool)?;
                    if tools.len() > MAX_TOOL_COUNT {
                        return Err(unsupported(
                            "too_many_tools",
                            "the request contains more callable tools than this bridge can safely accept",
                        ));
                    }
                }
            }
            // The ChatGPT Web composer, rather than the harness tool router, owns
            // these provider-side tool types.
            _ => {}
        }
        if tools.len() > MAX_TOOL_COUNT {
            return Err(unsupported(
                "too_many_tools",
                "the request contains more callable tools than this bridge can safely accept",
            ));
        }
    }
    Ok(tools)
}

fn extend_callable_tools_from_input(
    tools: &mut Vec<CallableTool>,
    input: &serde_json::Value,
) -> Result<(), RequestParseError> {
    let Some(items) = input.as_array() else {
        return Ok(());
    };
    let additional_specs = items
        .iter()
        .filter_map(
            |item| match item.get("type").and_then(serde_json::Value::as_str) {
                Some("additional_tools") | Some("tool_search_output") => {
                    item.get("tools").and_then(serde_json::Value::as_array)
                }
                _ => None,
            },
        )
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    if additional_specs.is_empty() {
        return Ok(());
    }
    let additional_tools = parse_callable_tools(Some(&serde_json::Value::Array(additional_specs)))?;
    for tool in additional_tools {
        if !tools.iter().any(|existing| {
            existing.kind == tool.kind
                && existing.namespace == tool.namespace
                && existing.name == tool.name
        }) {
            tools.push(tool);
        }
        if tools.len() > MAX_TOOL_COUNT {
            return Err((
                "422 Unprocessable Content",
                "too_many_tools",
                "the request contains more callable tools than this bridge can safely accept"
                    .to_string(),
            ));
        }
    }
    let declarations = tools
        .iter()
        .map(|tool| tool.declaration.clone())
        .collect::<Vec<_>>();
    let serialized = serde_json::to_vec(&declarations).map_err(|_| {
        (
            "422 Unprocessable Content",
            "tool_schema_unsupported",
            "callable tool schemas could not be serialized".to_string(),
        )
    })?;
    if serialized.len() > MAX_TOOL_SCHEMAS_BYTES {
        return Err((
            "413 Payload Too Large",
            "tool_schemas_too_large",
            "callable tool declarations exceed the WebView bridge size limit".to_string(),
        ));
    }
    Ok(())
}

fn input_content_to_text(
    content: &serde_json::Value,
    attachments: &mut Vec<MediaAttachment>,
    required_media: bool,
    include_images: bool,
) -> Result<String, RequestParseError> {
    let unsupported =
        |code, message: &str| ("422 Unprocessable Content", code, message.to_string());
    match content {
        serde_json::Value::String(text) => Ok(text.clone()),
        serde_json::Value::Array(parts) => {
            let mut text = String::new();
            for part in parts {
                let kind = part
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if kind == "input_image" && !include_images {
                    continue;
                }
                match kind {
                    "input_text" | "output_text" => {
                        let part_text = part
                            .get("text")
                            .and_then(serde_json::Value::as_str)
                            .ok_or(unsupported(
                                "input_format_unsupported",
                                "text content parts must contain text",
                            ))?;
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(part_text);
                    }
                    "input_image" | "input_audio" => {
                        let (key, media) = if kind == "input_image" {
                            ("image_url", "image")
                        } else {
                            ("audio_url", "audio")
                        };
                        let reference = part.get(key).and_then(serde_json::Value::as_str);
                        let attachment_result = match reference {
                            Some(data_url) if data_url.starts_with("data:") => {
                                append_data_url_attachment(
                                    Some(data_url),
                                    media,
                                    attachments,
                                    &mut text,
                                )
                            }
                            Some(_) => Err(unsupported(
                                if media == "image" {
                                    "input_image_unsupported"
                                } else {
                                    "input_audio_unsupported"
                                },
                                if media == "image" {
                                    "remote image URLs cannot be fetched by this local bridge; attach a local image or provide an inline data URL"
                                } else {
                                    "remote audio URLs cannot be fetched by this local bridge; attach a local audio file or provide an inline data URL"
                                },
                            )),
                            None if part
                                .get("file_id")
                                .and_then(serde_json::Value::as_str)
                                .is_some() =>
                            {
                                Err(unsupported(
                                    if media == "image" {
                                        "input_image_unsupported"
                                    } else {
                                        "input_audio_unsupported"
                                    },
                                    if media == "image" {
                                        "image file_id references cannot be resolved in a Temporary Chat; resend the image as an inline attachment or local file"
                                    } else {
                                        "audio file_id references cannot be resolved in a Temporary Chat; resend the audio as an inline attachment or local file"
                                    },
                                ))
                            }
                            None => append_data_url_attachment(None, media, attachments, &mut text),
                        };
                        if let Err(error) = attachment_result {
                            if required_media {
                                return Err(error);
                            }
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            let reason = if part
                                .get("file_id")
                                .and_then(serde_json::Value::as_str)
                                .is_some()
                            {
                                "file reference cannot be resolved"
                            } else {
                                "attachment is unavailable"
                            };
                            text.push_str(&format!(
                                "[Earlier {media} is unavailable in this Temporary Chat: {reason}]"
                            ));
                        }
                    }
                    // This opaque payload is local history metadata, not readable prompt text.
                    "encrypted_content" => {}
                    _ => {
                        return Err(unsupported(
                            "input_format_unsupported",
                            &format!(
                                "the conversation contains an unsupported content part type '{}'",
                                part.get("type")
                                    .and_then(serde_json::Value::as_str)
                                    .unwrap_or("<missing>")
                            ),
                        ));
                    }
                }
            }
            Ok(text)
        }
        _ => Err(unsupported(
            "input_format_unsupported",
            "message content must be text or a list of text parts",
        )),
    }
}

fn tool_output_to_context(
    output: &serde_json::Value,
    include_media: bool,
    attachments: &mut Vec<MediaAttachment>,
) -> String {
    const FAILED_MARKER: &str = "web2codex://tool-result/failed/v1";
    let mut preserved = Vec::new();
    let content = match output {
        serde_json::Value::Array(parts) => Some(parts.as_slice()),
        serde_json::Value::Object(object) => object
            .get("content")
            .and_then(|content| content.as_array())
            .map(Vec::as_slice),
        _ => None,
    };

    if let Some(content) = content {
        for (index, part) in content.iter().enumerate() {
            match part.get("type").and_then(serde_json::Value::as_str) {
                Some("input_text" | "output_text" | "text") => {
                    if let Some(text) = part.get("text").and_then(serde_json::Value::as_str) {
                        if index == 0 && part["type"] == "input_text" && text == FAILED_MARKER {
                            preserved.push("The previous tool returned an error.".to_string());
                        } else {
                            preserved.push(text.to_string());
                        }
                    }
                }
                Some("input_image") if include_media => {
                    let data_url = part.get("image_url").and_then(serde_json::Value::as_str);
                    let attachment = data_url.and_then(|data_url| {
                        let (header, data) = data_url.split_once(',')?;
                        let mime = header.strip_prefix("data:")?.strip_suffix(";base64")?;
                        attach_tool_media("image", mime, data, attachments)
                    });
                    preserved.push(attachment.unwrap_or_else(|| {
                        if part.get("file_id").and_then(serde_json::Value::as_str).is_some() {
                            "[An earlier tool returned an image file reference that cannot be resolved in this Temporary Chat]".to_string()
                        } else {
                            "[An earlier tool returned an image that could not be attached to this chat]".to_string()
                        }
                    }));
                }
                Some("image") if include_media => {
                    let mime = part
                        .get("mimeType")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    let data = part
                        .get("data")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    preserved.push(attach_tool_media("image", mime, data, attachments).unwrap_or_else(|| {
                        format!("[An earlier tool returned an image that could not be attached to this chat ({mime})]")
                    }));
                }
                Some("input_image" | "image") => {}
                Some("input_audio") if include_media => {
                    let data_url = part.get("audio_url").and_then(serde_json::Value::as_str);
                    let attachment = data_url.and_then(|data_url| {
                        let (header, data) = data_url.split_once(',')?;
                        let mime = header.strip_prefix("data:")?.strip_suffix(";base64")?;
                        attach_tool_media("audio", mime, data, attachments)
                    });
                    preserved.push(attachment.unwrap_or_else(|| {
                        if part.get("file_id").and_then(serde_json::Value::as_str).is_some() {
                            "[An earlier tool returned an audio file reference that cannot be resolved in this Temporary Chat]".to_string()
                        } else {
                            "[An earlier tool returned audio that could not be attached to this chat]".to_string()
                        }
                    }));
                }
                Some("audio") if include_media => {
                    let mime = part
                        .get("mimeType")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown audio type");
                    let data = part
                        .get("data")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    preserved.push(attach_tool_media("audio", mime, data, attachments).unwrap_or_else(|| {
                        format!("[An earlier tool returned audio that could not be attached to this chat ({mime})]")
                    }));
                }
                Some("input_audio" | "audio") => {}
                Some("input_file") if include_media => {
                    let file_url = part.get("file_url").and_then(serde_json::Value::as_str);
                    let filename = part
                        .get("filename")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    let mut marker = String::new();
                    let attached =
                        append_data_url_file(file_url, Some(filename), attachments, &mut marker)
                            .is_ok();
                    preserved.push(if attached {
                        marker
                    } else {
                        format!(
                            "[An earlier tool returned a file ({}) that could not be attached to this Temporary Chat]",
                            safe_attachment_filename(filename, attachments.len() + 1)
                        )
                    });
                }
                Some("input_file") => {}
                Some("resource" | "resource_link") => {
                    let resource = part.get("resource").unwrap_or(part);
                    let uri = resource["uri"].as_str().unwrap_or_default();
                    // A data URI already contains the blob. Keep only its metadata here.
                    let uri = if uri.starts_with("data:") {
                        "(inline resource)"
                    } else {
                        uri
                    };
                    let label = resource["name"].as_str().unwrap_or("MCP resource");
                    preserved.push(format!("{label}: {uri}"));
                    for field in ["description", "text"] {
                        if let Some(text) = resource[field].as_str() {
                            preserved.push(text.to_owned());
                        }
                    }
                    if let Some(blob) = resource["blob"].as_str() {
                        let mime = resource["mimeType"]
                            .as_str()
                            .unwrap_or("application/octet-stream");
                        let attachment = if include_media {
                            let filename = resource["name"].as_str().unwrap_or(label);
                            attach_tool_file(mime, blob, filename, attachments)
                        } else {
                            None
                        };
                        preserved.push(
                            attachment.unwrap_or_else(|| format!("[Binary resource: {mime}]")),
                        );
                    }
                }
                Some("encrypted_content") => {}
                Some(kind) => preserved.push(format!("[Unsupported tool result block: {kind}]")),
                None => {}
            }
        }
    }

    if let serde_json::Value::Object(object) = output {
        if object.get("isError").and_then(serde_json::Value::as_bool) == Some(true) {
            preserved.insert(0, "The previous MCP tool returned an error.".to_string());
        }
        // Structured MCP data has already been delivered through the connector.
        // Do not serialize it into a later ChatGPT prompt as JSON.
    }
    if preserved.is_empty() && content.is_none() {
        match output {
            serde_json::Value::String(text) => text.clone(),
            serde_json::Value::Null | serde_json::Value::Object(_) => String::new(),
            serde_json::Value::Bool(_) | serde_json::Value::Number(_) => output.to_string(),
            serde_json::Value::Array(_) => String::new(),
        }
    } else {
        preserved.join("\n")
    }
}

fn responses_input_to_prompt(
    input: &serde_json::Value,
    attachments: &mut Vec<MediaAttachment>,
) -> Result<String, RequestParseError> {
    responses_input_to_prompt_with_policy(input, attachments, false)
}

fn responses_input_to_prompt_with_policy(
    input: &serde_json::Value,
    attachments: &mut Vec<MediaAttachment>,
    compaction: bool,
) -> Result<String, RequestParseError> {
    let unsupported =
        |code, message: &str| ("422 Unprocessable Content", code, message.to_string());
    if let Some(text) = input.as_str() {
        return Ok(text.to_string());
    }
    let Some(items) = input.as_array() else {
        return Err(unsupported(
            "input_format_unsupported",
            "input must be text or a list of conversation items",
        ));
    };

    let mut sections = Vec::new();
    let mut seen_instruction_text = HashSet::new();
    // A fresh Temporary Chat needs earlier tool images as well as their text.
    // Compaction intentionally omits images; a tool continuation already has them.
    let new_user_turn = items
        .iter()
        .rev()
        .find(|item| {
            !matches!(
                item.get("type").and_then(serde_json::Value::as_str),
                Some("reasoning" | "additional_tools" | "configuration_update")
            )
        })
        .is_some_and(|item| {
            item.get("type").and_then(serde_json::Value::as_str) == Some("message")
                && item.get("role").and_then(serde_json::Value::as_str) == Some("user")
        });
    // Attach the current request first. Older images must not exhaust the
    // composer's upload slots before the user's latest photo can be attached.
    let latest_input = items.iter().rposition(|item| {
        item["type"] == "agent_message" || (item["type"] == "message" && item["role"] == "user")
    });
    for (index, item) in items.iter().enumerate().rev() {
        let Some(kind) = item.get("type").and_then(serde_json::Value::as_str) else {
            return Err(unsupported(
                "input_format_unsupported",
                "conversation items must include a type",
            ));
        };
        match kind {
            "message" => {
                let role = item
                    .get("role")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if compaction && matches!(role, "system" | "developer") {
                    continue;
                }
                let heading = match role {
                    "system" => "System instructions",
                    "developer" => "Developer instructions",
                    "user" => "User",
                    "assistant" => "Assistant",
                    "tool" => "Tool result",
                    _ => {
                        return Err(unsupported(
                            "input_role_unsupported",
                            "conversation contains an unsupported message role",
                        ));
                    }
                };
                let content = item.get("content").ok_or(unsupported(
                    "input_format_unsupported",
                    "conversation message is missing content",
                ))?;
                let text = input_content_to_text(
                    content,
                    attachments,
                    Some(index) == latest_input,
                    !compaction,
                )?;
                let text = if matches!(role, "system" | "developer") {
                    compact_default_base_instructions(&text)
                } else {
                    text
                };
                if matches!(role, "system" | "developer")
                    && !seen_instruction_text.insert(text.trim().to_owned())
                {
                    continue;
                }
                if !text.trim().is_empty() {
                    sections.push(format!("{heading}:\n{text}"));
                }
            }
            "agent_message" => {
                let author = item
                    .get("author")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("agent");
                let Some(content) = item.get("content") else {
                    continue;
                };
                let text = input_content_to_text(
                    content,
                    attachments,
                    Some(index) == latest_input,
                    !compaction,
                )?;
                if !text.trim().is_empty() {
                    sections.push(format!("Agent ({author}):\n{text}"));
                }
            }
            "function_call_output" | "custom_tool_call_output" | "mcp_tool_call_output" => {
                // A new independent user turn opens a Temporary Chat. Preserve textual
                // results from earlier tool rounds so the next turn retains
                // the work already done; schemas still travel only via MCP.
                if let Some(output) = item.get("output") {
                    let text = tool_output_to_context(
                        output,
                        new_user_turn && !compaction,
                        attachments,
                    );
                    if !text.trim().is_empty() {
                        sections.push(format!("Earlier tool result:\n{text}"));
                    }
                }
            }
            "function_call" | "custom_tool_call" | "mcp_call" => {
                let name = item["name"].as_str().unwrap_or("tool");
                let namespace = item["namespace"].as_str().unwrap_or("");
                let wire_name = if namespace.is_empty() {
                    name.to_string()
                } else {
                    format!("{namespace}.{name}")
                };
                // The native call already ran through MCP. Never copy its JSON
                // arguments (or freeform tool input) into the next chat prompt.
                sections.push(format!(
                    "Earlier tool call ({wire_name}) was executed through MCP."
                ));
            }
            "local_shell_call" => {
                if item.get("action").is_some() {
                    // Shell actions can be structured objects; retain only the
                    // fact that the action ran, not a JSON serialization of it.
                    sections.push("An earlier local shell action was executed.".to_string());
                }
            }
            "web_search_call" => {}
            kind if kind.ends_with("_output") && kind != "tool_search_output" => {}
            // These entries carry hidden control data or tool declarations rather than
            // conversation text. The visible ChatGPT UI manages its own MCP tools.
            "additional_tools"
            | "reasoning"
            | "configuration_update"
            | "compaction"
            | "compaction_trigger"
            | "context_compaction"
            | "tool_search_call"
            | "tool_search_output"
            | "image_generation_call"
            | "other" => {}
            _ => {
                return Err(unsupported(
                    "input_item_unsupported",
                    "conversation contains an unsupported Responses API item",
                ));
            }
        }
    }
    sections.reverse();
    Ok(sections.join("\n\n"))
}

fn latest_user_message_for_interrupt(
    input: &serde_json::Value,
) -> Result<Option<(String, Vec<String>)>, RequestParseError> {
    let Some(items) = input.as_array() else {
        return Ok(input.as_str().map(|text| (text.to_owned(), Vec::new())));
    };
    let Some(message) = items.iter().rev().find(|item| {
        item.get("type").and_then(serde_json::Value::as_str) == Some("message")
            && item.get("role").and_then(serde_json::Value::as_str) == Some("user")
    }) else {
        return Ok(None);
    };
    let content = message.get("content").ok_or((
        "422 Unprocessable Content",
        "input_format_unsupported",
        "the latest user message is missing content".to_string(),
    ))?;
    let mut attachments = Vec::new();
    let text = input_content_to_text(content, &mut attachments, true, true)?;
    let attachment_names = attachments
        .into_iter()
        .map(|attachment| attachment.name)
        .collect();
    Ok(Some((text, attachment_names)))
}

fn compact_default_base_instructions(instructions: &str) -> String {
    let default = DEFAULT_BASE_INSTRUCTIONS.trim();
    if instructions.trim() == default {
        return COMPACT_BASE_INSTRUCTIONS.to_owned();
    }
    // The default prompt can arrive combined with project instructions. Replace
    // that built-in block in place while preserving all project-specific text.
    if instructions.contains(default) {
        return instructions.replace(default, COMPACT_BASE_INSTRUCTIONS);
    }
    let normalized = instructions.replace("\r\n", "\n");
    let normalized_default = default.replace("\r\n", "\n");
    normalized.replace(&normalized_default, COMPACT_BASE_INSTRUCTIONS)
}

fn split_compaction_context_into_four(context: &str) -> [String; 4] {
    let char_offsets = context
        .char_indices()
        .map(|(offset, _)| offset)
        .chain(std::iter::once(context.len()))
        .collect::<Vec<_>>();
    let character_count = char_offsets.len().saturating_sub(1);
    if character_count < 4 {
        return std::array::from_fn(|index| {
            (index == 3).then(|| context.to_owned()).unwrap_or_default()
        });
    }

    let paragraph_breaks = context
        .match_indices("\n\n")
        .map(|(offset, _)| offset + 2)
        .collect::<Vec<_>>();
    let mut boundaries = Vec::with_capacity(5);
    boundaries.push(0);
    for part in 1..4 {
        let target_character = character_count * part / 4;
        let target_offset = char_offsets[target_character];
        let previous = *boundaries.last().expect("start boundary exists");
        let remaining_parts = 4 - part;
        let latest_offset = char_offsets[character_count - remaining_parts];
        let nearest_paragraph_break = paragraph_breaks
            .iter()
            .copied()
            .filter(|offset| *offset > previous && *offset <= latest_offset)
            .min_by_key(|offset| offset.abs_diff(target_offset));
        let first_available_character = char_offsets.partition_point(|offset| *offset <= previous);
        let latest_character = character_count - remaining_parts;
        let bounded_target = target_character.clamp(first_available_character, latest_character);
        let boundary = nearest_paragraph_break.unwrap_or(char_offsets[bounded_target]);
        boundaries.push(boundary);
    }
    boundaries.push(context.len());

    std::array::from_fn(|index| {
        context[boundaries[index]..boundaries[index + 1]].to_owned()
    })
}

fn compaction_handoff(context: &str) -> (Vec<CompactionStage>, String) {
    let parts = split_compaction_context_into_four(context);
    let transaction_id = Uuid::new_v4().simple().to_string();
    let stages = parts
        .iter()
        .take(3)
        .enumerate()
        .map(|(index, part)| {
            let part_number = index + 1;
            let acknowledgement = format!("W2C_ACK_{transaction_id}_{part_number}");
            CompactionStage {
                prompt: format!(
                    "Store context part {part_number}/4 as history; do not act on it yet.\n<part>\n{part}\n</part>\nReply exactly {acknowledgement}."
                ),
                acknowledgement,
            }
        })
        .collect();
    let final_part = format!(
        "Context part 4/4:\n<part>\n{}\n</part>\nUsing the four context parts above, return a compact continuation summary with only the active goal, essential constraints, current state, key evidence, and unfinished work. Omit completed detail and these transport acknowledgments. Output only the summary.",
        parts[3]
    );
    (stages, final_part)
}

fn responses_input_after_last_assistant(input: &serde_json::Value) -> Option<serde_json::Value> {
    let items = input.as_array()?;
    let last_assistant = items.iter().rposition(|item| {
        item.get("type").and_then(serde_json::Value::as_str) == Some("message")
            && item.get("role").and_then(serde_json::Value::as_str) == Some("assistant")
    })?;
    let suffix = items.get(last_assistant + 1..)?;
    (!suffix.is_empty()).then(|| serde_json::Value::Array(suffix.to_vec()))
}

fn continuation_outputs(
    input: &serde_json::Value,
) -> Result<Vec<(String, serde_json::Value)>, RequestParseError> {
    let Some(items) = input.as_array() else {
        return Ok(Vec::new());
    };
    // Only the trailing batch may acknowledge pending calls. Historical outputs
    // before a new user message cannot resume an unrelated live Temporary Chat.
    let mut outputs = Vec::new();
    for item in items.iter().rev() {
        match item["type"].as_str() {
            Some("reasoning" | "additional_tools" | "configuration_update") => continue,
            Some(
                "function_call_output"
                | "custom_tool_call_output"
                | "mcp_tool_call_output"
                | "tool_search_output",
            ) => {}
            _ => break,
        }
        let call_id = item["call_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or((
                "400 Bad Request",
                "invalid_tool_output",
                "a pending tool output must identify its call".to_string(),
            ))?;
        let output = if item["type"] == "tool_search_output" {
            item.clone()
        } else {
            item.get("output").cloned().ok_or((
                "400 Bad Request",
                "invalid_tool_output",
                "a pending tool result must contain output".to_string(),
            ))?
        };
        outputs.push((call_id.to_owned(), output));
    }
    outputs.reverse();
    Ok(outputs)
}

fn parse_text_only_responses_request(
    body: &[u8],
) -> Result<TextOnlyResponsesRequest, RequestParseError> {
    let request: serde_json::Value = serde_json::from_slice(body).map_err(|_| {
        (
            "400 Bad Request",
            "invalid_json",
            "request body must be valid JSON".to_string(),
        )
    })?;
    let object = request.as_object().ok_or((
        "400 Bad Request",
        "invalid_request",
        "request body must be a JSON object".to_string(),
    ))?;
    let agent = parse_agent_request_identity(object.get("client_metadata"));
    let conversation_id = parse_conversation_thread_id(object.get("client_metadata"));
    let setup_smoke_test = is_setup_smoke_test_metadata(object.get("client_metadata"));
    let client_metadata = object.get("client_metadata");
    let is_compaction = client_metadata
        .and_then(|metadata| metadata.get("x-codex-turn-metadata"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| value.len() <= MAX_TOOL_SCHEMAS_BYTES)
        .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
        .and_then(|metadata| metadata.get("request_kind").and_then(serde_json::Value::as_str).map(str::to_owned))
        .as_deref()
        == Some("compaction");
    let unsupported =
        |code, message: &str| ("422 Unprocessable Content", code, message.to_string());
    let model = object
        .get("model")
        .and_then(serde_json::Value::as_str)
        .filter(|model| !model.trim().is_empty() && model.len() <= 128)
        .ok_or((
            "400 Bad Request",
            "invalid_model",
            "request must include a model identifier".to_string(),
        ))?
        .to_string();
    let stream = object
        .get("stream")
        .map(|value| {
            value.as_bool().ok_or((
                "400 Bad Request",
                "invalid_stream_option",
                "stream must be a boolean".to_string(),
            ))
        })
        .transpose()?
        .unwrap_or(false);
    let reasoning_effort = match object
        .get("reasoning")
        .and_then(|reasoning| reasoning.get("effort"))
        .and_then(serde_json::Value::as_str)
    {
        None | Some("none") => None,
        Some(effort @ ("low" | "medium" | "high" | "xhigh" | "max")) => Some(effort.to_owned()),
        Some(_) => {
            return Err(unsupported(
                "reasoning_effort_unsupported",
                "unsupported ChatGPT Web reasoning effort",
            ));
        }
    };
    let input = object.get("input").ok_or(unsupported(
        "input_format_unsupported",
        "request is missing input",
    ))?;
    let (interruption_prompt, interruption_attachment_names) = if is_compaction {
        (None, Vec::new())
    } else {
        latest_user_message_for_interrupt(input)?
            .map(|(text, attachment_names)| (Some(text), attachment_names))
            .unwrap_or_default()
    };
    // A compaction handoff must start in a clean Temporary Chat so its four
    // messages do not inherit the already-large chat history being compacted.
    let conversation_epoch = if is_compaction {
        Some(format!("compaction:{}", Uuid::new_v4().simple()))
    } else {
        parse_conversation_epoch(input)
    };
    let mut tools = parse_callable_tools(object.get("tools"))?;
    extend_callable_tools_from_input(&mut tools, input)?;
    if is_compaction {
        tools.clear();
    }
    let connector_tool_choice = parse_tool_choice(object.get("tool_choice"), &tools)?;
    let mut tool_choice = HarnessToolChoice::select(object.get("tool_choice"), &mut tools)?;
    if let Some(text) = object.get("text")
        && text.get("format").is_some_and(|format| {
            format
                .get("type")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|kind| kind != "text")
        })
    {
        return Err(unsupported(
            "output_format_unsupported",
            "only plain text output is supported by the ChatGPT composer bridge",
        ));
    }

    let mut attachments = Vec::new();
    let outputs = continuation_outputs(input)?;
    let expects_continuation = !outputs.is_empty();
    let mcp_required = !is_compaction
        && !expects_continuation
        && (user_request_requires_mcp(input) || agent.is_some());
    let mcp_action_required =
        mcp_required && (agent.is_some() || user_request_requires_mcp_action(input));
    if mcp_action_required
        && !tools.is_empty()
        && matches!(&tool_choice, HarnessToolChoice::Auto)
    {
        tool_choice = HarnessToolChoice::Required;
    }
    let mut prompt = if expects_continuation {
        String::new()
    } else {
        responses_input_to_prompt_with_policy(input, &mut attachments, is_compaction)?
    };
    let mut resume_attachments = Vec::new();
    let mut resume_prompt = if expects_continuation || is_compaction {
        String::new()
    } else if let Some(suffix) = responses_input_after_last_assistant(input) {
        responses_input_to_prompt_with_policy(&suffix, &mut resume_attachments, false)?
    } else {
        resume_attachments = attachments.clone();
        prompt.clone()
    };
    let mut developer_instructions = if is_compaction {
        None
    } else {
        object
            .get("instructions")
            .and_then(serde_json::Value::as_str)
            .filter(|instructions| !instructions.trim().is_empty())
            .map(compact_default_base_instructions)
            .filter(|instructions| !prompt.contains(instructions.trim()))
    };
    if !expects_continuation && !is_compaction {
        prompt = format!(
            "{prompt}\n\n{TUI_TEXT_RENDERING_INSTRUCTIONS}\n{MCP_CHAT_OUTPUT_INSTRUCTIONS}"
        );
    }

    if mcp_required {
        let mcp_policy = if mcp_action_required {
            if agent.is_some() {
                MCP_AGENT_TASK_INSTRUCTIONS.to_owned()
            } else {
                format!("{MCP_TOOL_ROUTING_INSTRUCTIONS} {TOOL_COMPLETION_INSTRUCTIONS}")
            }
        } else {
            MCP_TOOL_ROUTING_INSTRUCTIONS.to_owned()
        };
        developer_instructions = Some(match developer_instructions {
            Some(instructions) => format!("{mcp_policy}\n\n{instructions}"),
            None => mcp_policy,
        });
    }

    if !expects_continuation && let Some(instructions) = developer_instructions {
        prompt = format!("Developer instructions:\n{instructions}\n\n{prompt}");
    }
    if prompt.trim().is_empty() && outputs.is_empty() {
        return Err((
            "400 Bad Request",
            "empty_input",
            "input must contain non-empty text".to_string(),
        ));
    }
    let (compaction_stages, prompt) = if is_compaction && !prompt.trim().is_empty() {
        let (stages, final_part) = compaction_handoff(&prompt);
        resume_prompt.clear();
        (stages, final_part)
    } else {
        (Vec::new(), prompt)
    };
    if prompt.len() > MAX_PROMPT_TEXT_BYTES
        || compaction_stages
            .iter()
            .any(|stage| stage.prompt.len() > MAX_PROMPT_TEXT_BYTES)
    {
        return Err((
            "400 Bad Request",
            "context_length_exceeded",
            "a conversation or compaction part exceeds the WebView bridge limit".to_string(),
        ));
    }
    let connector_required = !is_compaction && (!tools.is_empty() || mcp_required);
    let tool_work_expected = !is_compaction
        && (mcp_required
            || agent.is_some()
            || tool_choice.requires_tool_work()
            || (!tools.is_empty() && user_request_may_require_tools(input)));
    Ok(TextOnlyResponsesRequest {
        model,
        conversation_id,
        conversation_epoch,
        agent,
        setup_smoke_test,
        compaction_stages,
        reasoning_effort,
        connector_tool_choice,
        prompt,
        resume_prompt,
        attachments,
        resume_attachments,
        interruption_prompt,
        interruption_attachment_names,
        stream,
        tools,
        connector_required,
        mcp_required,
        mcp_action_required,
        tool_work_expected,
        tool_choice,
        outputs,
        expects_continuation,
    })
}

fn parse_conversation_epoch(input: &serde_json::Value) -> Option<String> {
    let items = input.as_array()?;
    let (index, checkpoint) = items.iter().enumerate().rev().find(|(_, item)| {
        matches!(
            item.get("type").and_then(serde_json::Value::as_str),
            Some("compaction" | "compaction_summary" | "context_compaction")
        )
    })?;
    let checkpoint_id = checkpoint
        .get("id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .unwrap_or("checkpoint");
    Some(format!("{index}:{checkpoint_id}"))
}

fn user_request_requires_mcp(input: &serde_json::Value) -> bool {
    let text = if let Some(text) = input.as_str() {
        text.to_owned()
    } else {
        let Some(message) = input.as_array().and_then(|items| {
            items.iter().rev().find(|item| {
                item.get("type").and_then(serde_json::Value::as_str) == Some("message")
                    && item.get("role").and_then(serde_json::Value::as_str) == Some("user")
            })
        }) else {
            return false;
        };
        let mut text = String::new();
        collect_user_text(message.get("content"), &mut text);
        text
    };

    let text = text.to_lowercase();
    if !text.contains("mcp") && !text.contains("connector") {
        return false;
    }
    if [
        "don't use mcp",
        "do not use mcp",
        "не используй mcp",
        "без mcp",
    ]
    .iter()
    .any(|phrase| text.contains(phrase))
    {
        return false;
    }
    [
        "only",
        "strictly",
        "through",
        "via",
        "using",
        "use ",
        "call ",
        "invoke",
        "read ",
        "write ",
        "create ",
        "run ",
        "execute",
        "только",
        "строго",
        "через",
        "используй",
        "используя",
        "вызови",
        "прочитай",
        "прочти",
        "создай",
        "запусти",
        "выполни",
        "проверь",
        "тестируй",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn user_request_may_require_tools(input: &serde_json::Value) -> bool {
    let text = if let Some(text) = input.as_str() {
        text.to_owned()
    } else {
        let Some(message) = input.as_array().and_then(|items| {
            items.iter().rev().find(|item| {
                item.get("type").and_then(serde_json::Value::as_str) == Some("message")
                    && item.get("role").and_then(serde_json::Value::as_str) == Some("user")
            })
        }) else {
            return false;
        };
        let mut text = String::new();
        collect_user_text(message.get("content"), &mut text);
        text
    };
    let text = text.to_lowercase();
    [
        "agent",
        "subagent",
        "sub-agent",
        "parallel",
        "file",
        "folder",
        "directory",
        "repository",
        "workspace",
        "run ",
        "execute",
        "command",
        "test",
        "build",
        "compile",
        "create",
        "make ",
        "implement",
        "fix ",
        "edit ",
        "change ",
        "update ",
        "delete",
        "remove",
        "rename",
        "move ",
        "copy ",
        "inspect",
        "read ",
        "write ",
        "search ",
        "verify ",
        "создай",
        "сделай",
        "субагент",
        "суб-агент",
        "агент",
        "параллел",
        "файл",
        "папк",
        "репозитор",
        "проект",
        "код",
        "запусти",
        "выполни",
        "команд",
        "тест",
        "собери",
        "сборк",
        "реализуй",
        "исправ",
        "измени",
        "обнови",
        "посмотри",
        "разберись",
        "прочитай",
        "прочти",
        "напиши",
        "проверь",
        "найди",
        "открой",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn user_request_requires_mcp_action(input: &serde_json::Value) -> bool {
    let text = if let Some(text) = input.as_str() {
        text.to_owned()
    } else {
        let Some(message) = input.as_array().and_then(|items| {
            items.iter().rev().find(|item| {
                item.get("type").and_then(serde_json::Value::as_str) == Some("message")
                    && item.get("role").and_then(serde_json::Value::as_str) == Some("user")
            })
        }) else {
            return false;
        };
        let mut text = String::new();
        collect_user_text(message.get("content"), &mut text);
        text
    };
    let text = text.to_lowercase();
    [
        "spawn_agent",
        "subagent",
        "sub-agent",
        "parallel",
        "read ",
        "write ",
        "create ",
        "run ",
        "execute",
        "invoke",
        "call ",
        "создай",
        "субагент",
        "суб-агент",
        "параллел",
        "прочитай",
        "прочти",
        "запиши",
        "создать",
        "запусти",
        "выполни",
        "вызови",
        "проведи live-тест",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn collect_user_text(value: Option<&serde_json::Value>, output: &mut String) {
    match value {
        Some(serde_json::Value::String(text)) => {
            output.push_str(text);
            output.push('\n');
        }
        Some(serde_json::Value::Array(items)) => {
            for item in items {
                collect_user_text(Some(item), output);
            }
        }
        Some(serde_json::Value::Object(object)) => {
            if let Some(text) = object.get("text").and_then(serde_json::Value::as_str) {
                output.push_str(text);
                output.push('\n');
            }
        }
        _ => {}
    }
}

fn is_setup_smoke_test_metadata(client_metadata: Option<&serde_json::Value>) -> bool {
    let Some(metadata) = client_metadata.and_then(serde_json::Value::as_object) else {
        return false;
    };
    if metadata
        .get(SETUP_SMOKE_TEST_METADATA_KEY)
        .and_then(serde_json::Value::as_str)
        == Some("true")
    {
        return true;
    }
    let Some(turn_metadata) = metadata
        .get("x-codex-turn-metadata")
        .and_then(serde_json::Value::as_str)
        .filter(|value| value.len() <= 8192)
        .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
    else {
        return false;
    };
    turn_metadata
        .get("extra")
        .and_then(|extra| extra.get(SETUP_SMOKE_TEST_METADATA_KEY))
        .and_then(serde_json::Value::as_str)
        == Some("true")
}

fn parse_conversation_thread_id(client_metadata: Option<&serde_json::Value>) -> Option<String> {
    let metadata = client_metadata?.as_object()?;
    let turn_metadata = metadata
        .get("x-codex-turn-metadata")
        .and_then(serde_json::Value::as_str)
        .filter(|value| value.len() <= 8192)
        .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok());
    metadata
        .get("thread_id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            turn_metadata
                .as_ref()
                .and_then(|value| value.get("thread_id"))
                .and_then(serde_json::Value::as_str)
        })
        .map(str::trim)
        .filter(|thread_id| !thread_id.is_empty() && thread_id.len() <= 128)
        .map(str::to_owned)
}

fn parse_agent_request_identity(
    client_metadata: Option<&serde_json::Value>,
) -> Option<AgentRequestIdentity> {
    let metadata = client_metadata?.as_object()?;
    let turn_metadata = metadata
        .get("x-codex-turn-metadata")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok());
    let is_thread_spawn = metadata
        .get("x-openai-subagent")
        .and_then(serde_json::Value::as_str)
        == Some(COLLAB_SPAWN_SUBAGENT)
        || turn_metadata
            .as_ref()
            .and_then(|value| value.get("subagent_kind"))
            .and_then(serde_json::Value::as_str)
            == Some("thread_spawn")
        || (metadata
            .get("x-codex-parent-thread-id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|parent| !parent.trim().is_empty())
            && turn_metadata
                .as_ref()
                .and_then(|value| value.get("agent_name"))
                .and_then(serde_json::Value::as_str)
                .is_some_and(|path| path.trim().starts_with('/')));
    if !is_thread_spawn {
        return None;
    }
    let thread_id = parse_conversation_thread_id(client_metadata)?;
    let agent_path = turn_metadata
        .as_ref()
        .and_then(|value| value.get("agent_name"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            metadata
                .get("agent_name")
                .and_then(serde_json::Value::as_str)
                .filter(|path| path.trim().starts_with('/'))
        })
        .map(str::trim)
        .filter(|path| !path.is_empty() && path.len() <= 256)
        .map(str::to_owned);
    let name = metadata
        .get("agent_name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| name.chars().take(48).collect::<String>())
        .or_else(|| {
            agent_path.as_deref().and_then(|path| {
                path.rsplit('/')
                    .find(|segment| !segment.is_empty())
                    .map(|name| name.chars().take(48).collect::<String>())
            })
        })
        .unwrap_or_else(|| "Subagent".to_owned());
    Some(AgentRequestIdentity {
        thread_id,
        name,
        agent_path,
    })
}

fn parse_http_request(stream: &mut TcpStream) -> anyhow::Result<ParsedHttpRequest> {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    let mut limited = (&mut reader).take(MAX_REQUEST_HEADER_BYTES as u64);
    let read = limited.read_line(&mut request_line)?;
    drop(limited);
    anyhow::ensure!(
        read > 0 && read <= MAX_REQUEST_HEADER_BYTES && request_line.ends_with('\n'),
        "invalid request line"
    );
    let mut parts = request_line.split_whitespace();
    let method = parts.next().context("missing request method")?.to_string();
    // Codex's model client appends `client_version` to model catalog URLs.
    // Route on the path component so `/models?client_version=...` reaches the
    // same local catalog handler as `/models`.
    let request_target = parts.next().context("missing request path")?;
    let path = request_target
        .split_once('?')
        .map_or(request_target, |(path, _)| path)
        .to_string();
    anyhow::ensure!(
        parts
            .next()
            .is_some_and(|version| version.starts_with("HTTP/1.")),
        "invalid HTTP version"
    );
    anyhow::ensure!(parts.next().is_none(), "invalid request line");
    let mut authorization = String::new();
    let mut content_type = None;
    let mut content_length = None;
    let mut has_transfer_encoding = false;
    let mut bytes_read = read;
    loop {
        let mut header = String::new();
        let remaining = MAX_REQUEST_HEADER_BYTES.saturating_sub(bytes_read);
        anyhow::ensure!(remaining > 0, "request headers are too large");
        let mut limited = (&mut reader).take(remaining as u64);
        let count = limited.read_line(&mut header)?;
        drop(limited);
        bytes_read += count;
        anyhow::ensure!(count > 0, "incomplete request headers");
        anyhow::ensure!(header.ends_with('\n'), "request headers are too large");
        if header == "\r\n" || header == "\n" {
            break;
        }
        let (name, value) = header.split_once(':').context("malformed HTTP header")?;
        if name.eq_ignore_ascii_case("authorization") {
            anyhow::ensure!(authorization.is_empty(), "duplicate Authorization header");
            authorization = value.trim().to_string();
        } else if name.eq_ignore_ascii_case("content-type") {
            anyhow::ensure!(content_type.is_none(), "duplicate Content-Type header");
            content_type = Some(value.trim().to_string());
        } else if name.eq_ignore_ascii_case("content-length") {
            anyhow::ensure!(content_length.is_none(), "duplicate Content-Length header");
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .context("invalid Content-Length")?,
            );
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            has_transfer_encoding = true;
        }
    }
    anyhow::ensure!(!has_transfer_encoding, "Transfer-Encoding is not supported");
    let body_length = content_length.unwrap_or(0);
    anyhow::ensure!(
        body_length <= MAX_REQUEST_BODY_BYTES,
        "request body exceeds {} bytes",
        MAX_REQUEST_BODY_BYTES
    );
    let mut body = vec![0; body_length];
    reader.read_exact(&mut body)?;
    Ok(ParsedHttpRequest {
        method,
        path,
        authorization,
        content_type,
        body,
    })
}

fn write_response(stream: &mut TcpStream, status: &str, body: &str) -> std::io::Result<()> {
    let body = body.replace('\r', " ").replace('\n', " ");
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn json_error(error_type: &str, code: &str, message: &str) -> serde_json::Value {
    serde_json::json!({
        "error": {
            "type": error_type,
            "code": code,
            "message": message
        }
    })
}

fn completed_response_object(
    response_id: &str,
    model: &str,
    output_items: Vec<serde_json::Value>,
    end_turn: bool,
) -> serde_json::Value {
    serde_json::json!({
        "id": response_id,
        "object": "response",
        "status": "completed",
        "model": model,
        "output": output_items,
        // The ChatGPT UI does not expose token counts. Leave usage unknown rather than inventing it.
        "usage": null,
        "end_turn": end_turn
    })
}

fn response_item_for_added(output_item: &serde_json::Value) -> serde_json::Value {
    let mut item = output_item.clone();
    let Some(object) = item.as_object_mut() else {
        return item;
    };
    object.insert(
        "status".to_string(),
        serde_json::Value::String("in_progress".to_string()),
    );
    match object.get("type").and_then(serde_json::Value::as_str) {
        Some("message") => {
            object.insert("content".to_string(), serde_json::json!([]));
        }
        Some("function_call") => {
            object.insert("arguments".to_string(), serde_json::json!(""));
        }
        Some("custom_tool_call") => {
            object.insert("input".to_string(), serde_json::json!(""));
        }
        _ => {}
    }
    item
}

fn response_item_for_completion(
    message_item_id: &str,
    tool_item_id: &str,
    call_id: &str,
    completion: BridgeCompletion,
) -> (serde_json::Value, bool) {
    match completion {
        BridgeCompletion::Message(text) => (
            serde_json::json!({
                "id": message_item_id,
                "type": "message",
                "status": "completed",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": text, "annotations": [] }]
            }),
            true,
        ),
        BridgeCompletion::FunctionCall {
            namespace,
            name,
            arguments,
        } => (
            serde_json::json!({
                "id": tool_item_id,
                "type": "function_call",
                "status": "completed",
                "call_id": call_id,
                "namespace": namespace,
                "name": name,
                "arguments": serde_json::to_string(&arguments).unwrap_or_else(|_| "{}".to_string())
            }),
            false,
        ),
        BridgeCompletion::CustomToolCall {
            namespace,
            name,
            input,
        } => (
            serde_json::json!({
                "id": tool_item_id,
                "type": "custom_tool_call",
                "status": "completed",
                "call_id": call_id,
                "namespace": namespace,
                "name": name,
                "input": input
            }),
            false,
        ),
        BridgeCompletion::ToolSearchCall { arguments } => (
            serde_json::json!({
                "id": tool_item_id,
                "type": "tool_search_call",
                "status": "completed",
                "call_id": call_id,
                "execution": "client",
                "arguments": arguments
            }),
            false,
        ),
    }
}

fn write_json_response(
    stream: &mut TcpStream,
    status: &str,
    body: &serde_json::Value,
) -> std::io::Result<()> {
    let body = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(&body)?;
    stream.flush()
}

fn write_sse_header(stream: &mut TcpStream) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
    )?;
    stream.flush()
}

fn write_sse_event(stream: &mut TcpStream, event: &serde_json::Value) -> std::io::Result<()> {
    let data = serde_json::to_string(event).unwrap_or_else(|_| "{}".to_string());
    let frame = format!(
        "event: {}\ndata: {}\n\n",
        event["type"].as_str().unwrap_or("message"),
        data
    );
    write!(stream, "{:X}\r\n", frame.len())?;
    stream.write_all(frame.as_bytes())?;
    stream.write_all(b"\r\n")?;
    stream.flush()
}

fn write_sse_error(
    stream: &mut TcpStream,
    response_id: &str,
    code: &str,
    message: &str,
) -> std::io::Result<()> {
    let event = serde_json::json!({
        "type": "response.failed",
        "response": {
            "id": response_id,
            "object": "response",
            "status": "failed",
            "error": { "code": code, "message": message }
        }
    });
    write_sse_event(stream, &event)
}

fn send_control(state_path: &Path, path: &str) -> anyhow::Result<()> {
    let contents = fs::read_to_string(state_path)?;
    let state: BrowserState = serde_json::from_str(&contents)?;
    anyhow::ensure!(
        state.address.ip().is_loopback() && !state.token.is_empty(),
        "invalid local Web2Codex browser state"
    );
    let mut stream = TcpStream::connect_timeout(&state.address, Duration::from_millis(400))?;
    stream.set_read_timeout(Some(Duration::from_secs(16)))?;
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        state.token
    )?;
    let mut response = String::new();
    std::io::Read::read_to_string(&mut stream, &mut response)?;
    anyhow::ensure!(
        response.starts_with("HTTP/1.1 204"),
        "browser control endpoint returned an error"
    );
    Ok(())
}

fn remove_runtime_files(state_path: &Path, lock_path: &Path) {
    let _ = fs::remove_file(state_path);
    let _ = fs::remove_file(lock_path);
}

fn data_directory() -> anyhow::Result<PathBuf> {
    if let Some(configured) = env::var_os("WEB2CODEX_WEB_DATA_DIR") {
        return Ok(PathBuf::from(configured));
    }

    let home = env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .context("could not determine the current user's home directory")?;
    Ok(if cfg!(windows) {
        env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Local"))
            .join("Web2Codex/Browser")
    } else if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Web2Codex/Browser")
    } else {
        env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("web2codex/browser")
    })
}

fn restrict_permissions(_path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(_path, fs::Permissions::from_mode(0o700)).with_context(|| {
            format!(
                "failed to restrict browser data permissions at {}",
                _path.display()
            )
        })?;
    }
    Ok(())
}

fn restrict_file_permissions(_path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(_path, fs::Permissions::from_mode(0o600)).with_context(|| {
            format!(
                "failed to restrict browser state permissions at {}",
                _path.display()
            )
        })?;
    }
    Ok(())
}
