//! Commands for managing Codex Web's first-party native WebView process.

use std::fs;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::Context;
use clap::Args;
use clap::Subcommand;
use serde::Deserialize;

#[derive(Debug, Args)]
pub(crate) struct WebCommand {
    #[command(subcommand)]
    command: Option<WebSubcommand>,
}

#[derive(Debug, Subcommand)]
enum WebSubcommand {
    /// Start the browser process hidden, without loading ChatGPT.
    Start,
    /// Open the private ChatGPT window.
    Open,
    /// Open ChatGPT to sign in (the window opens only for this command).
    Login,
    /// Open ChatGPT in the private window.
    Setup,
    /// Hide the browser window and keep only its lightweight controller running.
    Hide,
    /// Show whether the background browser process is running.
    Status,
    /// Stop the background browser process.
    Stop,
}

#[derive(Debug, Deserialize)]
struct BrowserState {
    address: SocketAddr,
    token: String,
}

#[derive(Clone, Copy)]
enum ControlAction {
    Open,
    Hide,
    Stop,
    Status,
}

pub(crate) fn run(command: WebCommand) -> anyhow::Result<()> {
    match command.command.unwrap_or(WebSubcommand::Open) {
        WebSubcommand::Start => {
            ensure_browser_started()?;
            println!("Codex Web is running in the background. Its window is hidden.");
        }
        WebSubcommand::Open | WebSubcommand::Login | WebSubcommand::Setup => {
            ensure_browser_started()?;
            send_control(ControlAction::Open)?;
            println!("Opened the private Codex Web window.");
        }
        WebSubcommand::Hide => {
            send_control(ControlAction::Hide)?;
            println!("Hid the Codex Web window; the lightweight controller is still running.");
        }
        WebSubcommand::Status => match send_control(ControlAction::Status) {
            Ok(()) => println!("Codex Web browser is running."),
            Err(_) => println!("Codex Web browser is stopped."),
        },
        WebSubcommand::Stop => match send_control(ControlAction::Stop) {
            Ok(()) => println!("Stopped the Codex Web browser."),
            Err(_) => println!("Codex Web browser is already stopped."),
        },
    }
    Ok(())
}

fn ensure_browser_started() -> anyhow::Result<()> {
    if send_control(ControlAction::Status).is_ok() {
        return Ok(());
    }

    let executable = find_browser_executable().context(
        "the bundled Codex Web browser was not found; reinstall Codex Web or set CODEX_WEB_BROWSER_BIN",
    )?;
    let mut process = Command::new(&executable);
    process
        .arg("--background")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        process.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = process.spawn().with_context(|| {
        format!(
            "failed to start the Codex Web browser at {}",
            executable.display()
        )
    })?;

    for _ in 0..100 {
        thread::sleep(Duration::from_millis(100));
        if send_control(ControlAction::Status).is_ok() {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            anyhow::bail!(
                "Codex Web browser exited during startup with {status}; check that a graphical session and the required native WebView runtime are available"
            );
        }
    }
    let _ = child.kill();
    let status = child.wait().ok();
    anyhow::bail!(
        "Codex Web browser did not start{}; check that a graphical session and the required native WebView runtime are available",
        status.map_or_else(String::new, |status| format!(" ({status})")),
    )
}

fn send_control(action: ControlAction) -> anyhow::Result<()> {
    let state = read_state()?;
    let path = match action {
        ControlAction::Open => "/open",
        ControlAction::Hide => "/hide",
        ControlAction::Stop => "/stop",
        ControlAction::Status => "/status",
    };
    let mut stream =
        std::net::TcpStream::connect_timeout(&state.address, Duration::from_millis(400))
            .with_context(|| format!("browser process is not reachable at {}", state.address))?;
    stream.set_read_timeout(Some(if matches!(action, ControlAction::Open) {
        Duration::from_secs(16)
    } else {
        Duration::from_millis(700)
    }))?;
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        state.token
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    if !response.starts_with("HTTP/1.1 204") {
        let body = response.split_once("\r\n\r\n").map_or("", |(_, body)| body);
        anyhow::bail!("Codex Web browser rejected the control request: {body}");
    }
    Ok(())
}

fn read_state() -> anyhow::Result<BrowserState> {
    let state_path = data_directory()?.join("browser-state.json");
    let contents = fs::read_to_string(&state_path).with_context(|| {
        format!(
            "Codex Web browser state is missing: {}",
            state_path.display()
        )
    })?;
    serde_json::from_str(&contents).context("Codex Web browser state is invalid")
}

fn find_browser_executable() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CODEX_WEB_BROWSER_BIN").map(PathBuf::from) {
        if path.is_file() {
            return Some(path);
        }
    }

    let executable_name = if cfg!(windows) {
        "codex-web-browser.exe"
    } else {
        "codex-web-browser"
    };
    let current = std::env::current_exe().ok()?;
    let current = current.canonicalize().unwrap_or(current);
    let parent = current.parent()?;
    let candidates = [
        parent.join(executable_name),
        parent.join("codex-resources").join(executable_name),
        parent
            .parent()
            .unwrap_or(parent)
            .join("codex-resources")
            .join(executable_name),
    ];
    candidates.into_iter().find(|path| path.is_file())
}

fn data_directory() -> anyhow::Result<PathBuf> {
    if let Some(configured) = std::env::var_os("CODEX_WEB_DATA_DIR") {
        let path = PathBuf::from(configured);
        fs::create_dir_all(&path).with_context(|| {
            format!(
                "failed to create Codex Web data directory {}",
                path.display()
            )
        })?;
        return Ok(path);
    }

    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .context("could not determine the current user's home directory")?;
    let path = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Local"))
            .join("Codex Web")
    } else if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Codex Web")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("codex-web")
    };
    fs::create_dir_all(&path).with_context(|| {
        format!(
            "failed to create Codex Web data directory {}",
            path.display()
        )
    })?;
    Ok(path)
}
