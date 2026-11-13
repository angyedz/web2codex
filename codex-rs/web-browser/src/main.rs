#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, SyncSender};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::{Window, WindowBuilder};
use uuid::Uuid;
#[cfg(target_os = "macos")]
use wry::WebViewBuilderExtDarwin;
use wry::{WebContext, WebView, WebViewBuilder};

#[cfg(test)]
mod bridge_tests;
mod responses_http;
mod turn_broker;

use turn_broker::TurnBroker;

const CHATGPT_SIGN_IN_URL: &str = "https://chatgpt.com/";
const CHATGPT_TEMPORARY_CHAT_URL: &str = "https://chatgpt.com/?temporary-chat=true";
const STALE_LOCK_AGE: Duration = Duration::from_secs(30);
const MAX_REQUEST_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_REQUEST_HEADER_BYTES: usize = 16 * 1024;
const MAX_RESPONSE_TEXT_BYTES: usize = 4 * 1024 * 1024;
const MAX_PROMPT_TEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_TOOL_SCHEMAS_BYTES: usize = 4 * 1024 * 1024;
const MAX_TOOL_COUNT: usize = 1024;
const MAX_TOOL_CALL_INPUT_BYTES: usize = 512 * 1024;
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
          location.replace(temporaryChatUrl);
          return;
        }
      }
      if (next === temporaryChatUrl) {
        location.replace(temporaryChatUrl);
        return;
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
        location.replace(temporaryChatUrl);
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
    detected_at_unix_seconds: u64,
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
    fs::write(path, serde_json::to_vec(&stats)?).with_context(|| {
        format!(
            "failed to save local usage statistics at {}",
            path.display()
        )
    })?;
    restrict_file_permissions(path)
}

fn model_catalog_from_picker_snapshot(path: &Path) -> serde_json::Value {
    let Ok(contents) = fs::read(path) else {
        return serde_json::json!({ "models": [] });
    };
    let Ok(capabilities) = serde_json::from_slice::<AccountCapabilities>(&contents) else {
        return serde_json::json!({ "models": [] });
    };
    if capabilities.probe_version != 3 {
        return serde_json::json!({ "models": [] });
    }

    let mut seen = Vec::new();
    let models = capabilities
        .picker_options
        .into_iter()
        .enumerate()
        .filter_map(|(priority, label)| {
            let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
            if label.is_empty() || label.len() > 120 || seen.contains(&label) {
                return None;
            }
            seen.push(label.clone());
            Some(serde_json::json!({
                // The visible picker label is an opaque, account-specific key. The request bridge
                // checks it against the selected Web model before submitting any prompt.
                "slug": label.clone(),
                "display_name": label.clone(),
                "description": "Detected in the signed-in ChatGPT Web model picker.",
                "supported_reasoning_levels": [],
                "shell_type": "unified_exec",
                "tool_mode": "direct",
                "visibility": "list",
                "supported_in_api": true,
                "priority": i32::try_from(priority).unwrap_or(i32::MAX),
                "availability_nux": null,
                "upgrade": null,
                "include_skills_usage_instructions": false,
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
                "input_modalities": ["text"],
                "supports_search_tool": false,
                "supports_experimental_context": false,
                "use_responses_lite": false,
                "supports_reasoning_effort_updates": false
            }))
        })
        .collect::<Vec<_>>();
    serde_json::json!({ "models": models })
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
    Open(BrowserPage, SyncSender<Result<(), String>>),
    ContinueLogin(AccountTierChoice),
    AccountCapabilities(AccountCapabilities),
    ResponsesBridgeReady(String),
    ResponsesRequest {
        request_id: String,
        model: String,
        prompt: String,
    },
    ResponsesToolState(String),
    ResponsesResult {
        request_id: String,
        revision: Option<u64>,
        result: Result<String, String>,
    },
    ReleaseResponses(String),
    Hide,
    Stop(SyncSender<()>),
}

struct PendingResponsesRequest {
    model: String,
    prompt: String,
    started_at: std::time::Instant,
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
        .with_visible(false)
        .build(&event_loop)
        .context("failed to initialize the hidden Web2Codex window")?;

    let lock_file = acquire_lock(&lock_path, &state_path)?;
    let listener = TcpListener::bind("127.0.0.1:0")
        .context("failed to bind the local Web2Codex control socket")?;
    let state = BrowserState {
        address: listener.local_addr()?,
        token: Uuid::new_v4().to_string(),
        bridge_protocol: 2,
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
    )
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
) -> ! {
    let window_id = window.id();
    let mut web_context: Option<WebContext> = None;
    let mut webview: Option<WebView> = None;
    let mut browser_page: Option<BrowserPage> = None;
    // Turns share sign-in storage, never a composer or a capability token.
    let mut response_views: HashMap<String, WebView> = HashMap::new();
    let mut pending_responses: HashMap<String, PendingResponsesRequest> = HashMap::new();
    let event_proxy = event_loop.create_proxy();

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(BrowserEvent::Open(page, reply)) => {
                if let Err(error) = prepare_profile_directory(&profile_dir) {
                    let _ = reply.send(Err(error.to_string())); return;
                }
                if page == BrowserPage::AccountChooser || page == BrowserPage::TemporaryChat {
                    let _ = fs::remove_file(&account_capabilities_path);
                }
                webview = None;
                let context = web_context.get_or_insert_with(|| WebContext::new(Some(profile_dir.clone())));
                match build_webview(&window, context, page, event_proxy.clone(), None) {
                    Ok(created) => { webview = Some(created); browser_page = Some(page); }
                    Err(error) => { let _ = reply.send(Err(error.to_string())); return; }
                }
                window.set_visible(true);
                window.set_focus();
                let _ = reply.send(Ok(()));
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
            Event::UserEvent(BrowserEvent::AccountCapabilities(capabilities)) => {
                if let Err(error) = store_account_capabilities(&account_capabilities_path, capabilities) {
                    eprintln!("failed to save ChatGPT account capabilities: {error:#}");
                }
            }
            Event::UserEvent(BrowserEvent::ResponsesBridgeReady(request_id)) => {
                if let (Some(view), Some(pending)) = (response_views.get(&request_id), pending_responses.get(&request_id)) {
                    let payload = serde_json::json!({"request_id": request_id, "model": pending.model, "prompt": pending.prompt});
                    if let Err(error) = view.evaluate_script(&format!("window.__web2codexSend({payload});")) {
                        broker.complete(&request_id, None, Err(format!("Could not send prompt to ChatGPT Web: {error}")));
                    }
                }
            }
            Event::UserEvent(BrowserEvent::ResponsesRequest { request_id, model, prompt }) => {
                if let Err(error) = prepare_profile_directory(&profile_dir) {
                    broker.complete(&request_id, None, Err(error.to_string())); return;
                }
                let context = web_context.get_or_insert_with(|| WebContext::new(Some(profile_dir.clone())));
                match build_webview(&window, context, BrowserPage::TemporaryChat, event_proxy.clone(), Some(request_id.clone())) {
                    Ok(view) => { response_views.insert(request_id.clone(), view); }
                    Err(error) => { broker.complete(&request_id, None, Err(error.to_string())); return; }
                }
                pending_responses.insert(request_id, PendingResponsesRequest { model, prompt, started_at: std::time::Instant::now() });
            }
            Event::UserEvent(BrowserEvent::ResponsesToolState(request_id)) => {
                let state = broker.snapshot(&request_id);
                if state.get("active").and_then(serde_json::Value::as_bool) == Some(false) {
                    response_views.remove(&request_id);
                    if let Some(pending) = pending_responses.remove(&request_id) {
                        let milliseconds = u64::try_from(pending.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
                        let _ = record_local_usage(&usage_stats_path, &pending.model, pending.prompt.chars().count(), None, milliseconds);
                    }
                    if response_views.is_empty() && webview.is_none() { web_context = None; }
                    return;
                }
                if let Some(view) = response_views.get(&request_id) {
                    let _ = view.evaluate_script(&format!("window.__web2codexToolState={state};"));
                }
            }
            Event::UserEvent(BrowserEvent::ResponsesResult { request_id, revision, result }) => {
                let output_characters = result.as_ref().ok().map(|text| text.chars().count());
                if broker.complete(&request_id, revision, result) {
                    if let Some(view) = response_views.get(&request_id) { let _ = view.evaluate_script("window.__web2codexCompleted=true;"); }
                    if let Some(pending) = pending_responses.remove(&request_id) {
                        let milliseconds = u64::try_from(pending.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
                        if let Err(error) = record_local_usage(&usage_stats_path, &pending.model, pending.prompt.chars().count(), output_characters, milliseconds) {
                            eprintln!("failed to record local Web2Codex usage: {error:#}");
                        }
                    }
                }
            }
            Event::UserEvent(BrowserEvent::ReleaseResponses(request_id)) => {
                response_views.remove(&request_id);
                if let Some(pending) = pending_responses.remove(&request_id) {
                    let milliseconds = u64::try_from(pending.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
                    let _ = record_local_usage(&usage_stats_path, &pending.model, pending.prompt.chars().count(), None, milliseconds);
                }
                if response_views.is_empty() && webview.is_none() { web_context = None; }
            }
            Event::UserEvent(BrowserEvent::Hide) => {
                window.set_visible(false);
                webview = None;
                browser_page = None;
                if response_views.is_empty() { web_context = None; }
            }
            Event::UserEvent(BrowserEvent::Stop(reply)) => {
                window.set_visible(false);
                broker.stop("ChatGPT WebView stopped before the response completed");
                response_views.clear();
                pending_responses.clear();
                remove_runtime_files(&state_path, &lock_file);
                let _ = reply.send(());
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent { window_id: event_window_id, event: WindowEvent::CloseRequested, .. } if event_window_id == window_id => {
                window.set_visible(false);
                webview = None;
                browser_page = None;
                if response_views.is_empty() { web_context = None; }
            }
            _ => {}
        }
    })
}

fn build_webview(
    window: &Window,
    context: &mut WebContext,
    page: BrowserPage,
    event_proxy: EventLoopProxy<BrowserEvent>,
    response_id: Option<String>,
) -> anyhow::Result<WebView> {
    let builder = WebViewBuilder::new_with_web_context(context);
    let builder = if response_id.is_some() {
        builder.with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
    } else {
        builder
    };
    #[cfg(target_os = "macos")]
    let builder = builder.with_data_store_identifier(PROFILE_DATA_STORE_ID);

    let ipc_response_id = response_id.clone();
    let builder = builder
        .with_visible(response_id.is_none())
        .with_initialization_script(ACCOUNT_CAPABILITY_PROBE_SCRIPT)
        .with_navigation_handler(move |target| {
            if page != BrowserPage::TemporaryChat {
                return true;
            }
            let Ok(target) = url::Url::parse(&target) else {
                return false;
            };
            if target.host_str() == Some("chatgpt.com") {
                return target.scheme() == "https"
                    && target.port_or_known_default() == Some(443)
                    && target.path() == "/";
            }
            if target.host_str() == Some("chat.openai.com") {
                // The legacy hostname is accepted only at its root so the
                // initialization script can immediately canonicalize it.
                return target.scheme() == "https"
                    && target.port_or_known_default() == Some(443)
                    && target.path() == "/";
            }
            target.scheme() == "https"
                || (target.scheme() == "http"
                    && target.host_str().is_some_and(|host| {
                        host == "localhost" || host == "127.0.0.1" || host == "[::1]"
                    }))
        })
        .with_ipc_handler(move |request| {
            let body = request.body().as_str();
            let tier = match body {
                "free-go" => Some(AccountTierChoice::FreeGo),
                "plus" => Some(AccountTierChoice::Plus),
                "pro" => Some(AccountTierChoice::Pro),
                _ => None,
            };
            if let Some(tier) = tier {
                let _ = event_proxy.send_event(BrowserEvent::ContinueLogin(tier));
                return;
            }
            let Ok(message) = serde_json::from_str::<serde_json::Value>(body) else {
                return;
            };
            match message.get("type").and_then(serde_json::Value::as_str) {
                Some("responses-bridge-ready")
                | Some("responses-tool-state")
                | Some("responses-result") => {
                    let Some(request_id) = ipc_response_id.as_ref() else {
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
                            let _ = event_proxy
                                .send_event(BrowserEvent::ResponsesBridgeReady(request_id.clone()));
                        }
                        Some("responses-tool-state") => {
                            let _ = event_proxy
                                .send_event(BrowserEvent::ResponsesToolState(request_id.clone()));
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
                != Some(3)
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
            let detected_at_unix_seconds = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let _ =
                event_proxy.send_event(BrowserEvent::AccountCapabilities(AccountCapabilities {
                    probe_version: 3,
                    picker_options,
                    detected_at_unix_seconds,
                }));
        });

    let builder = if page == BrowserPage::TemporaryChat {
        builder
            .with_initialization_script(TEMPORARY_CHAT_ENFORCEMENT_SCRIPT)
            .with_initialization_script(&format!(
                "window.__web2codexRequestId={};",
                serde_json::json!(response_id)
            ))
            .with_initialization_script(RESPONSES_BRIDGE_SCRIPT)
    } else {
        builder
    };

    let builder = match page {
        BrowserPage::SignIn => builder.with_url(CHATGPT_SIGN_IN_URL),
        BrowserPage::TemporaryChat => builder.with_url(CHATGPT_TEMPORARY_CHAT_URL),
        BrowserPage::AccountChooser => builder.with_html(ACCOUNT_CHOOSER_HTML),
    };

    builder.build(window).context(WEBVIEW_CREATION_CONTEXT)
}

fn store_account_tier(path: &Path, tier: AccountTierChoice) -> anyhow::Result<()> {
    fs::write(path, serde_json::to_vec(&tier)?).with_context(|| {
        format!(
            "failed to save the selected ChatGPT account type at {}",
            path.display()
        )
    })?;
    restrict_file_permissions(path)
}

fn store_account_capabilities(
    path: &Path,
    capabilities: AccountCapabilities,
) -> anyhow::Result<()> {
    let contents = serde_json::to_vec(&capabilities)?;
    fs::write(path, contents).with_context(|| {
        format!(
            "failed to save ChatGPT account capabilities at {}",
            path.display()
        )
    })?;
    restrict_file_permissions(path)
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
    let serialized = serde_json::to_vec(state)?;
    fs::write(state_path, serialized)
        .with_context(|| format!("failed to write browser state at {}", state_path.display()))?;
    restrict_file_permissions(state_path)
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
            let status = if error.to_string().contains("exceeds 8 MiB") {
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
        ("POST", "/status") => {
            let _ = write_response(&mut stream, "204 No Content", "");
        }
        ("POST", "/open") | ("POST", "/login") => {
            let (reply, result) = mpsc::sync_channel(1);
            let page = if request.path == "/login" {
                BrowserPage::AccountChooser
            } else {
                BrowserPage::TemporaryChat
            };
            if event_proxy
                .send_event(BrowserEvent::Open(page, reply))
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
            let (reply, stopped) = mpsc::sync_channel(1);
            if event_proxy.send_event(BrowserEvent::Stop(reply)).is_err() {
                let _ = write_response(&mut stream, "503 Service Unavailable", "browser stopped");
                return;
            }
            let _ = stopped.recv_timeout(Duration::from_secs(2));
            let _ = write_response(&mut stream, "204 No Content", "");
        }
        ("POST", "/v1/responses") => {
            responses_http::handle(&mut stream, &request, event_proxy, &broker);
        }
        ("POST", "/bridge/tools") | ("POST", "/bridge/invoke") => {
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
        Ok(0) => true,
        Err(error) if error.kind() != std::io::ErrorKind::WouldBlock => true,
        _ => false,
    };
    let _ = stream.set_nonblocking(false);
    closed
}

struct TextOnlyResponsesRequest {
    model: String,
    prompt: String,
    stream: bool,
    tools: Vec<CallableTool>,
    outputs: Vec<(String, serde_json::Value)>,
    expects_continuation: bool,
}

type RequestParseError = (&'static str, &'static str, &'static str);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CallableToolKind {
    Function,
    Custom,
    ToolSearch,
}

#[derive(Clone)]
struct CallableTool {
    kind: CallableToolKind,
    namespace: String,
    name: String,
    declaration: serde_json::Value,
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
    let unsupported = |code, message| ("422 Unprocessable Content", code, message);
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
    let unsupported = |code, message| ("422 Unprocessable Content", code, message);
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
            "tool declarations exceed the WebView bridge size limit",
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
                "the request contains more callable tools than this bridge can safely accept",
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
            "callable tool schemas could not be serialized",
        )
    })?;
    if serialized.len() > MAX_TOOL_SCHEMAS_BYTES {
        return Err((
            "413 Payload Too Large",
            "tool_schemas_too_large",
            "callable tool declarations exceed the WebView bridge size limit",
        ));
    }
    Ok(())
}

fn input_content_to_text(content: &serde_json::Value) -> Result<String, RequestParseError> {
    let unsupported = |code, message| ("422 Unprocessable Content", code, message);
    match content {
        serde_json::Value::String(text) => Ok(text.clone()),
        serde_json::Value::Array(parts) => {
            let mut text = String::new();
            for part in parts {
                let kind = part
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
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
                        return Err(unsupported(
                            "input_modality_unsupported",
                            "this ChatGPT Web bridge currently accepts text context only",
                        ));
                    }
                    _ => {
                        return Err(unsupported(
                            "input_format_unsupported",
                            "the conversation contains an unsupported content part",
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

fn responses_input_to_prompt(input: &serde_json::Value) -> Result<String, RequestParseError> {
    let unsupported = |code, message| ("422 Unprocessable Content", code, message);
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
    for item in items {
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
                let text = input_content_to_text(content)?;
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
                let text = input_content_to_text(content)?;
                if !text.trim().is_empty() {
                    sections.push(format!("Agent ({author}):\n{text}"));
                }
            }
            "function_call"
            | "custom_tool_call"
            | "local_shell_call"
            | "web_search_call"
            | "mcp_call"
            | "function_call_output"
            | "custom_tool_call_output" => {}
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
    Ok(sections.join("\n\n"))
}

fn extract_tool_outputs(input: &serde_json::Value) -> Vec<(String, serde_json::Value)> {
    input
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            if !matches!(
                item.get("type").and_then(serde_json::Value::as_str),
                Some("function_call_output" | "custom_tool_call_output" | "tool_search_output")
            ) {
                return None;
            }
            let call_id = item.get("call_id")?.as_str()?.to_owned();
            Some((
                call_id,
                item.get("output").cloned().unwrap_or_else(|| item.clone()),
            ))
        })
        .collect()
}

fn parse_text_only_responses_request(
    body: &[u8],
) -> Result<TextOnlyResponsesRequest, RequestParseError> {
    let request: serde_json::Value = serde_json::from_slice(body).map_err(|_| {
        (
            "400 Bad Request",
            "invalid_json",
            "request body must be valid JSON",
        )
    })?;
    let object = request.as_object().ok_or((
        "400 Bad Request",
        "invalid_request",
        "request body must be a JSON object",
    ))?;
    let unsupported = |code, message| ("422 Unprocessable Content", code, message);
    let model = object
        .get("model")
        .and_then(serde_json::Value::as_str)
        .filter(|model| !model.trim().is_empty() && model.len() <= 128)
        .ok_or((
            "400 Bad Request",
            "invalid_model",
            "request must include a model identifier",
        ))?
        .to_string();
    let stream = object
        .get("stream")
        .map(|value| {
            value.as_bool().ok_or((
                "400 Bad Request",
                "invalid_stream_option",
                "stream must be a boolean",
            ))
        })
        .transpose()?
        .unwrap_or(false);
    let input = object.get("input").ok_or(unsupported(
        "input_format_unsupported",
        "request is missing input",
    ))?;
    let mut tools = parse_callable_tools(object.get("tools"))?;
    extend_callable_tools_from_input(&mut tools, input)?;
    if object
        .get("tool_choice")
        .is_some_and(|choice| choice != "auto")
    {
        return Err(unsupported(
            "tool_choice_unsupported",
            "only automatic tool choice is supported by the ChatGPT connector",
        ));
    }
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

    let mut prompt = responses_input_to_prompt(input)?;
    if let Some(instructions) = object
        .get("instructions")
        .and_then(serde_json::Value::as_str)
        .filter(|instructions| !instructions.trim().is_empty())
    {
        prompt = format!("Developer instructions:\n{instructions}\n\n{prompt}");
    }
    let outputs = extract_tool_outputs(input);
    if prompt.trim().is_empty() && outputs.is_empty() {
        return Err((
            "400 Bad Request",
            "empty_input",
            "input must contain non-empty text",
        ));
    }
    if prompt.len() > MAX_PROMPT_TEXT_BYTES {
        return Err((
            "413 Payload Too Large",
            "input_too_large",
            "the conversation exceeds the WebView bridge limit",
        ));
    }
    Ok(TextOnlyResponsesRequest {
        model,
        prompt,
        stream,
        tools,
        outputs,
        expects_continuation: input
            .as_array()
            .and_then(|items| {
                items.iter().rev().find(|item| {
                    !matches!(
                        item.get("type").and_then(serde_json::Value::as_str),
                        Some("reasoning" | "additional_tools" | "configuration_update")
                    )
                })
            })
            .and_then(|item| item.get("type"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|kind| {
                matches!(
                    kind,
                    "function_call_output" | "custom_tool_call_output" | "tool_search_output"
                )
            }),
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
    let path = parts.next().context("missing request path")?.to_string();
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
        "request body exceeds 8 MiB"
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
    output_item: serde_json::Value,
    end_turn: bool,
) -> serde_json::Value {
    serde_json::json!({
        "id": response_id,
        "object": "response",
        "status": "completed",
        "model": model,
        "output": [output_item],
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

fn write_sse_error(stream: &mut TcpStream, code: &str, message: &str) -> std::io::Result<()> {
    let event = serde_json::json!({
        "type": "error",
        "error": {
            "type": "server_error",
            "code": code,
            "message": message
        }
    });
    write_sse_event(stream, &event)
}

fn send_control(state_path: &Path, path: &str) -> anyhow::Result<()> {
    let contents = fs::read_to_string(state_path)?;
    let state: BrowserState = serde_json::from_str(&contents)?;
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
