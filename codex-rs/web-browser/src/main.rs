#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
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

const CHATGPT_SIGN_IN_URL: &str = "https://chatgpt.com/";
const CHATGPT_TEMPORARY_CHAT_URL: &str = "https://chatgpt.com/?temporary-chat=true";
const STALE_LOCK_AGE: Duration = Duration::from_secs(30);
const ACCOUNT_CAPABILITY_PROBE_SCRIPT: &str = r##"
(() => {
  if (window.top !== window || location.origin !== "https://chatgpt.com") return;
  let reported = false;
  const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
  const visible = element => {
    if (!element || !element.isConnected) return false;
    for (let parent = element; parent; parent = parent.parentElement) {
      if (parent.matches("[hidden], [inert], [aria-hidden='true']")) return false;
      const style = getComputedStyle(parent);
      if (style.display === "none" || style.visibility === "hidden" || style.opacity === "0") return false;
    }
    return true;
  };
  const uniqueVisible = (root, selector) => {
    try {
      const matches = [...root.querySelectorAll(selector)].filter(visible);
      return matches.length === 1 ? matches[0] : null;
    } catch (_) {
      return null;
    }
  };
  const emit = picker_options => {
    if (reported) return;
    reported = true;
    window.ipc.postMessage(JSON.stringify({ type: "account-capabilities", probe_version: 3, picker_options }));
  };
  const composerSelector = [
    '[data-testid="prompt-textarea"]',
    "#prompt-textarea",
    '[contenteditable="true"][data-lexical-editor="true"]',
    'form[data-chatgpt-composer] [data-composer-markdown][contenteditable="true"][role="textbox"]'
  ].join(",");
  (async () => {
    const deadline = Date.now() + 10000;
    let observations = 0;
    while (!reported && Date.now() < deadline) {
      if (location.origin !== "https://chatgpt.com") return;
      const composer = uniqueVisible(document, composerSelector);
      const form = composer?.closest("form");
      if (!composer || !form || document.readyState !== "complete") {
        observations = 0;
        await sleep(250);
        continue;
      }
      const control = uniqueVisible(form, 'button[data-testid="model-switcher-dropdown-button"][aria-haspopup="menu"]');
      if (control) {
        observations += 1;
        if (observations >= 2) {
          const expanded = control.getAttribute("aria-expanded");
          const state = control.getAttribute("data-state");
          const wasOpen = expanded === "true" || state === "open";
          if (!wasOpen) control.click();
          const menuDeadline = Date.now() + 5000;
          while (!reported && Date.now() < menuDeadline) {
            const menus = [...document.querySelectorAll('[role="menu"], [role="listbox"]')].filter(visible);
            if (menus.length === 1) {
              const options = [...menus[0].querySelectorAll('[role="menuitem"], [role="option"], button')]
                .filter(visible)
                .map(element => (element.getAttribute("aria-label") || element.innerText || element.textContent || "").replace(/\s+/g, " ").trim())
                .filter(text => text.length > 0 && text.length <= 120)
                .filter((text, index, all) => all.indexOf(text) === index)
                .slice(0, 40);
              if (options.length > 0) {
                emit(options);
                if (!wasOpen) control.click();
                return;
              }
            }
            await sleep(250);
          }
          if (!wasOpen) control.click();
          emit([]);
          return;
        }
      } else {
        observations = 0;
      }
      await sleep(250);
    }
  })().catch(() => {});
})();
"##;
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
const ACCOUNT_CHOOSER_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Sign in to Codex Web</title>
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
    <p class="note">Sign-in stays in this private Codex Web browser profile.</p>
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
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct AccountCapabilities {
    probe_version: u8,
    picker_options: Vec<String>,
    detected_at_unix_seconds: u64,
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
    Hide,
    Stop(SyncSender<()>),
}

fn main() -> anyhow::Result<()> {
    if !env::args().any(|argument| argument == "--background") {
        anyhow::bail!("Codex Web starts only when requested by the Codex CLI");
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
        .with_title("Codex Web")
        .with_inner_size(LogicalSize::new(1200.0, 800.0))
        .with_visible(false)
        .build(&event_loop)
        .context("failed to initialize the hidden Codex Web window")?;

    let lock_file = acquire_lock(&lock_path, &state_path)?;
    let listener = TcpListener::bind("127.0.0.1:0")
        .context("failed to bind the local Codex Web control socket")?;
    let state = BrowserState {
        address: listener.local_addr()?,
        token: Uuid::new_v4().to_string(),
    };
    write_state(&state_path, &state)?;

    let event_proxy = event_loop.create_proxy();
    thread::Builder::new()
        .name("codex-web-control".to_string())
        .spawn(move || serve_control_requests(listener, state.token, event_proxy))
        .context("failed to start the Codex Web control listener")?;

    run_event_loop(
        event_loop,
        window,
        data_dir.join("profile"),
        data_dir.join("selected-account-tier.json"),
        data_dir.join("account-capabilities.json"),
        state_path,
        lock_file,
    )
}

fn run_event_loop(
    event_loop: tao::event_loop::EventLoop<BrowserEvent>,
    window: Window,
    profile_dir: PathBuf,
    selected_account_tier_path: PathBuf,
    account_capabilities_path: PathBuf,
    state_path: PathBuf,
    lock_file: PathBuf,
) -> ! {
    let window_id = window.id();
    let mut web_context: Option<WebContext> = None;
    let mut webview: Option<WebView> = None;
    let mut browser_page: Option<BrowserPage> = None;
    let event_proxy = event_loop.create_proxy();

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(BrowserEvent::Open(page, reply)) => {
                // An explicit open always returns to the Temporary Chat entry URL, even if the
                // current WebView navigated to a regular conversation while it was visible.
                let rebuilding_webview = page == BrowserPage::TemporaryChat
                    || webview.is_none()
                    || browser_page != Some(page);
                if page == BrowserPage::AccountChooser
                    || (page == BrowserPage::TemporaryChat && rebuilding_webview)
                {
                    let _ = fs::remove_file(&account_capabilities_path);
                }
                if rebuilding_webview {
                    webview = None;
                    web_context = None;
                    if let Err(error) = prepare_profile_directory(&profile_dir) {
                        let _ = reply.send(Err(error.to_string()));
                        return;
                    }
                    let context = web_context
                        .get_or_insert_with(|| WebContext::new(Some(profile_dir.clone())));
                    match build_webview(&window, context, page, event_proxy.clone()) {
                        Ok(created) => {
                            webview = Some(created);
                            browser_page = Some(page);
                        }
                        Err(error) => {
                            let _ = reply.send(Err(error.to_string()));
                            return;
                        }
                    }
                }
                window.set_visible(true);
                window.set_focus();
                let _ = reply.send(Ok(()));
            }
            Event::UserEvent(BrowserEvent::ContinueLogin(tier)) => {
                if browser_page != Some(BrowserPage::AccountChooser) {
                    return;
                }
                let Some(view) = webview.as_ref() else {
                    return;
                };
                if let Err(_error) = store_account_tier(&selected_account_tier_path, tier) {
                    let _ = view.evaluate_script(
                        "document.querySelector('#status').textContent = 'Could not save your account choice. Please try again.'; for (const button of document.querySelectorAll('button[data-tier]')) button.disabled = false;",
                    );
                    return;
                }
                if let Err(_error) = view.load_url(CHATGPT_SIGN_IN_URL) {
                    let _ = view.evaluate_script(
                        "document.querySelector('#status').textContent = 'Could not open ChatGPT. Please try again.'; for (const button of document.querySelectorAll('button[data-tier]')) button.disabled = false;",
                    );
                    return;
                }
                browser_page = Some(BrowserPage::SignIn);
            }
            Event::UserEvent(BrowserEvent::AccountCapabilities(capabilities)) => {
                if let Err(error) = store_account_capabilities(&account_capabilities_path, capabilities) {
                    eprintln!("failed to save ChatGPT account capabilities: {error:#}");
                }
            }
            Event::UserEvent(BrowserEvent::Hide) => {
                window.set_visible(false);
                // Keep only the lightweight controller resident while hidden.
                // WebView cookies and local storage live in the persistent profile.
                webview = None;
                web_context = None;
                browser_page = None;
            }
            Event::UserEvent(BrowserEvent::Stop(reply)) => {
                window.set_visible(false);
                remove_runtime_files(&state_path, &lock_file);
                let _ = reply.send(());
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent {
                window_id: event_window_id,
                event: WindowEvent::CloseRequested,
                ..
            } if event_window_id == window_id => {
                window.set_visible(false);
                webview = None;
                web_context = None;
                browser_page = None;
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
) -> anyhow::Result<WebView> {
    let builder = WebViewBuilder::new_with_web_context(context);
    #[cfg(target_os = "macos")]
    let builder = builder.with_data_store_identifier(PROFILE_DATA_STORE_ID);

    let builder = builder
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
            if message.get("type").and_then(serde_json::Value::as_str)
                != Some("account-capabilities")
            {
                return;
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
        builder.with_initialization_script(TEMPORARY_CHAT_ENFORCEMENT_SCRIPT)
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
                    anyhow::bail!("another Codex Web process is already running");
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
                        anyhow::bail!("another Codex Web process is still starting");
                    }
                }
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to create Codex Web lock file {}",
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
        .with_context(|| format!("could not acquire Codex Web lock {}", lock_path.display()))?;
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
    event_proxy: EventLoopProxy<BrowserEvent>,
) {
    for connection in listener.incoming() {
        let Ok(stream) = connection else {
            continue;
        };
        if handle_control_request(stream, &token, &event_proxy) {
            break;
        }
    }
}

fn handle_control_request(
    mut stream: TcpStream,
    token: &str,
    event_proxy: &EventLoopProxy<BrowserEvent>,
) -> bool {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let (request_path, authorization) = match parse_request(&mut stream) {
        Ok(request) => request,
        Err(_) => {
            let _ = write_response(&mut stream, "400 Bad Request", "bad request");
            return false;
        }
    };
    if authorization != format!("Bearer {token}") {
        let _ = write_response(&mut stream, "403 Forbidden", "forbidden");
        return false;
    }

    match request_path.as_str() {
        "/status" => {
            let _ = write_response(&mut stream, "204 No Content", "");
            false
        }
        "/open" | "/login" => {
            let (reply, result) = mpsc::sync_channel(1);
            let page = if request_path == "/login" {
                BrowserPage::AccountChooser
            } else {
                BrowserPage::TemporaryChat
            };
            if event_proxy
                .send_event(BrowserEvent::Open(page, reply))
                .is_err()
            {
                let _ = write_response(&mut stream, "503 Service Unavailable", "browser stopped");
                return false;
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
            false
        }
        "/hide" => {
            let status = if event_proxy.send_event(BrowserEvent::Hide).is_ok() {
                "204 No Content"
            } else {
                "503 Service Unavailable"
            };
            let _ = write_response(&mut stream, status, "");
            false
        }
        "/stop" => {
            let (reply, stopped) = mpsc::sync_channel(1);
            if event_proxy.send_event(BrowserEvent::Stop(reply)).is_err() {
                let _ = write_response(&mut stream, "503 Service Unavailable", "browser stopped");
                return true;
            }
            let _ = stopped.recv_timeout(Duration::from_secs(2));
            let _ = write_response(&mut stream, "204 No Content", "");
            true
        }
        _ => {
            let _ = write_response(&mut stream, "404 Not Found", "not found");
            false
        }
    }
}

fn parse_request(stream: &mut TcpStream) -> anyhow::Result<(String, String)> {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    anyhow::ensure!(parts.next() == Some("POST"), "expected POST request");
    let path = parts.next().context("missing request path")?.to_string();
    let mut authorization = String::new();
    let mut bytes_read = request_line.len();
    loop {
        let mut header = String::new();
        let count = reader.read_line(&mut header)?;
        bytes_read += count;
        anyhow::ensure!(bytes_read <= 8192, "request headers are too large");
        if count == 0 || header == "\r\n" || header == "\n" {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("authorization")
        {
            authorization = value.trim().to_string();
        }
    }
    Ok((path, authorization))
}

fn write_response(stream: &mut TcpStream, status: &str, body: &str) -> std::io::Result<()> {
    let body = body.replace('\r', " ").replace('\n', " ");
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
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
    if let Some(configured) = env::var_os("CODEX_WEB_DATA_DIR") {
        return Ok(PathBuf::from(configured));
    }

    let home = env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .context("could not determine the current user's home directory")?;
    Ok(if cfg!(windows) {
        env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Local"))
            .join("Codex Web")
    } else if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Codex Web")
    } else {
        env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("codex-web")
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
