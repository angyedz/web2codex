#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, SyncSender};
use std::thread;
use std::time::Duration;

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

const CHATGPT_URL: &str = "https://chatgpt.com/";
const STALE_LOCK_AGE: Duration = Duration::from_secs(30);
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

enum BrowserEvent {
    Open(SyncSender<Result<(), String>>),
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
        state_path,
        lock_file,
    )
}

fn run_event_loop(
    event_loop: tao::event_loop::EventLoop<BrowserEvent>,
    window: Window,
    profile_dir: PathBuf,
    state_path: PathBuf,
    lock_file: PathBuf,
) -> ! {
    let window_id = window.id();
    let mut web_context: Option<WebContext> = None;
    let mut webview: Option<WebView> = None;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(BrowserEvent::Open(reply)) => {
                if webview.is_none() {
                    if let Err(error) = prepare_profile_directory(&profile_dir) {
                        let _ = reply.send(Err(error.to_string()));
                        return;
                    }
                    let context = web_context
                        .get_or_insert_with(|| WebContext::new(Some(profile_dir.clone())));
                    match build_webview(&window, context) {
                        Ok(created) => webview = Some(created),
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
            Event::UserEvent(BrowserEvent::Hide) => {
                window.set_visible(false);
                // Keep only the lightweight controller resident while hidden.
                // WebView cookies and local storage live in the persistent profile.
                webview = None;
                web_context = None;
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
            }
            _ => {}
        }
    })
}

fn build_webview(window: &Window, context: &mut WebContext) -> anyhow::Result<WebView> {
    let builder = WebViewBuilder::new_with_web_context(context);
    #[cfg(target_os = "macos")]
    let builder = builder.with_data_store_identifier(PROFILE_DATA_STORE_ID);

    builder
        .with_url(CHATGPT_URL)
        .build(window)
        .context(WEBVIEW_CREATION_CONTEXT)
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
        "/open" => {
            let (reply, result) = mpsc::sync_channel(1);
            if event_proxy.send_event(BrowserEvent::Open(reply)).is_err() {
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
