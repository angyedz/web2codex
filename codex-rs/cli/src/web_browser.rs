//! Commands for managing Codex Web's first-party native WebView process.

use std::fs;
use std::io::{IsTerminal, Read, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::Context;
use clap::Args;
use clap::Subcommand;
use codex_core::config::find_codex_home;
use codex_secrets::{SecretName, SecretScope, SecretsBackendKind, SecretsManager};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal;
use serde::Deserialize;

const TUNNEL_API_KEY_NAME: &str = "OPENAI_TUNNEL_API_KEY";
const TUNNEL_ID_NAME: &str = "OPENAI_TUNNEL_ID";

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
    /// Choose an account tier and open the ChatGPT sign-in page.
    Login,
    /// Open ChatGPT in the private window.
    Setup,
    /// Hide the browser window and keep only its lightweight controller running.
    Hide,
    /// Show whether the background browser process is running.
    Status,
    /// Show the last model and effort selector capabilities detected in ChatGPT Web.
    Models,
    /// Stop the background browser process.
    Stop,
    /// Internal MCP server process used by OpenAI Secure MCP Tunnel.
    #[command(name = "mcp-server", hide = true)]
    McpServer {
        /// Restrict file tools and command working directory to this workspace.
        #[arg(long)]
        workspace: PathBuf,
    },
    /// Configure, inspect, or remove OpenAI Secure MCP Tunnel credentials.
    Tunnel {
        #[command(subcommand)]
        command: TunnelSubcommand,
    },
}

#[derive(Debug, Subcommand)]
enum TunnelSubcommand {
    /// Save tunnel credentials and start the local MCP tunnel runtime.
    Configure,
    /// Start the local MCP tunnel runtime in the background.
    Start,
    /// Check whether the local MCP tunnel runtime is ready.
    Status,
    /// Stop the local MCP tunnel runtime.
    Stop,
    /// Remove the saved tunnel ID and API key.
    Remove,
}

#[derive(Debug, Deserialize)]
struct BrowserState {
    address: SocketAddr,
    token: String,
}

#[derive(Debug, Deserialize)]
struct AccountCapabilities {
    probe_version: u8,
    picker_options: Vec<String>,
    detected_at_unix_seconds: u64,
}

#[derive(Clone, Copy)]
enum ControlAction {
    Open,
    Login,
    Hide,
    Stop,
    Status,
}

pub(crate) fn run(command: WebCommand) -> anyhow::Result<()> {
    let command = command.command.unwrap_or_else(|| {
        if has_saved_account_choice() {
            WebSubcommand::Open
        } else {
            WebSubcommand::Login
        }
    });
    match command {
        WebSubcommand::Start => {
            ensure_browser_started()?;
            println!("Codex Web is running in the background. Its window is hidden.");
        }
        WebSubcommand::Open | WebSubcommand::Setup => {
            ensure_browser_started()?;
            send_control(ControlAction::Open)?;
            println!("Opened the private Codex Web window.");
        }
        WebSubcommand::Login => {
            ensure_browser_started()?;
            send_control(ControlAction::Login)?;
            println!("Opened the ChatGPT account chooser.");
        }
        WebSubcommand::Hide => {
            send_control(ControlAction::Hide)?;
            println!("Hid the Codex Web window; the lightweight controller is still running.");
        }
        WebSubcommand::Status => match send_control(ControlAction::Status) {
            Ok(()) => println!("Codex Web browser is running."),
            Err(_) => println!("Codex Web browser is stopped."),
        },
        WebSubcommand::Models => print_account_capabilities()?,
        WebSubcommand::Stop => match send_control(ControlAction::Stop) {
            Ok(()) => println!("Stopped the Codex Web browser."),
            Err(_) => println!("Codex Web browser is already stopped."),
        },
        WebSubcommand::McpServer { workspace } => crate::web_mcp_server::run(workspace)?,
        WebSubcommand::Tunnel { command } => match command {
            TunnelSubcommand::Configure => configure_tunnel_credentials()?,
            TunnelSubcommand::Start => start_tunnel_runtime()?,
            TunnelSubcommand::Status => crate::web_tunnel::status()?,
            TunnelSubcommand::Stop => crate::web_tunnel::stop()?,
            TunnelSubcommand::Remove => remove_tunnel_credentials()?,
        },
    }
    Ok(())
}

fn print_account_capabilities() -> anyhow::Result<()> {
    let models = codex_models_manager::bundled_models_response()
        .context("failed to read the bundled Codex model catalog")?;
    println!("Codex model catalog in this harness:");
    for model in models.models.iter().filter(|model| {
        model.visibility == codex_protocol::openai_models::ModelVisibility::List
            && model.supported_in_api
    }) {
        println!("  {} ({})", model.display_name, model.slug);
    }
    println!("The signed-in account controls which models are actually available.");

    let path = data_directory()?.join("account-capabilities.json");
    let contents = match fs::read(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("ChatGPT Web picker snapshot: unknown (not detected yet).");
            println!("Open ChatGPT with `codex web open`, then check again.");
            return Ok(());
        }
        Err(error) => {
            println!("ChatGPT Web picker snapshot: unknown (could not read saved scan: {error}).");
            return Ok(());
        }
    };
    let capabilities = match serde_json::from_slice::<AccountCapabilities>(&contents) {
        Ok(capabilities) => capabilities,
        Err(_) => {
            println!("ChatGPT Web picker snapshot: unknown (saved scan is invalid).");
            println!("Open ChatGPT with `codex web open` to refresh the scan.");
            return Ok(());
        }
    };
    if capabilities.probe_version != 3 {
        println!("ChatGPT Web picker snapshot: unknown (scan format is unsupported).");
        println!("Open ChatGPT with `codex web open` to refresh the scan.");
        return Ok(());
    }

    println!(
        "Last observed ChatGPT Web picker options (Unix time {}):",
        capabilities.detected_at_unix_seconds
    );
    if capabilities.picker_options.is_empty() {
        println!("  No readable options were found; ChatGPT controls actual access.");
    } else {
        for option in capabilities.picker_options {
            println!("  {option}");
        }
    }
    println!("This is visible UI text, not a verified account entitlement.");
    Ok(())
}

fn tunnel_secrets() -> anyhow::Result<(SecretsManager, SecretScope, SecretName, SecretName)> {
    let codex_home = find_codex_home().context("failed to resolve CODEX_HOME")?;
    let manager = SecretsManager::new(codex_home.to_path_buf(), SecretsBackendKind::Local);
    Ok((
        manager,
        SecretScope::Global,
        SecretName::new(TUNNEL_API_KEY_NAME)?,
        SecretName::new(TUNNEL_ID_NAME)?,
    ))
}

fn configure_tunnel_credentials() -> anyhow::Result<()> {
    let tunnel_id = prompt_line("OpenAI tunnel ID: ")?;
    anyhow::ensure!(
        is_valid_tunnel_id(&tunnel_id),
        "invalid tunnel ID; expected tunnel_<32 lowercase letters or digits> or tunnel_<4-character namespace>_<32 lowercase letters or digits>"
    );
    let api_key = prompt_hidden("OpenAI tunnel API key: ")?;
    let (manager, scope, api_key_name, tunnel_id_name) = tunnel_secrets()?;
    let previous_tunnel_id = manager.get(&scope, &tunnel_id_name)?;
    manager
        .set(&scope, &tunnel_id_name, &tunnel_id)
        .context("failed to save the OpenAI tunnel ID")?;
    if let Err(error) = manager.set(&scope, &api_key_name, &api_key) {
        if let Some(previous_tunnel_id) = previous_tunnel_id {
            let _ = manager.set(&scope, &tunnel_id_name, &previous_tunnel_id);
        } else {
            let _ = manager.delete(&scope, &tunnel_id_name);
        }
        return Err(error).context("failed to save the OpenAI tunnel API key");
    }
    println!("Tunnel credentials saved in Codex's encrypted secret store.");
    start_tunnel_runtime().context("credentials were saved, but the MCP tunnel did not start")
}

fn start_tunnel_runtime() -> anyhow::Result<()> {
    let (manager, scope, api_key_name, tunnel_id_name) = tunnel_secrets()?;
    let api_key = manager
        .get(&scope, &api_key_name)?
        .context("OpenAI tunnel API key is not configured; run `codex web tunnel configure`")?;
    let tunnel_id = manager
        .get(&scope, &tunnel_id_name)?
        .context("OpenAI tunnel ID is not configured; run `codex web tunnel configure`")?;
    let workspace = std::env::current_dir().context("failed to resolve the workspace directory")?;
    crate::web_tunnel::start(&tunnel_id, &api_key, &workspace)
}

fn is_valid_tunnel_id(value: &str) -> bool {
    let Some(value) = value.strip_prefix("tunnel_") else {
        return false;
    };
    let is_id = |id: &str| {
        id.len() == 32
            && id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    };

    if is_id(value) {
        return true;
    }

    let Some((namespace, id)) = value.split_once('_') else {
        return false;
    };
    namespace.len() == 4
        && namespace
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && is_id(id)
}

fn remove_tunnel_credentials() -> anyhow::Result<()> {
    crate::web_tunnel::stop()?;
    let (manager, scope, api_key_name, tunnel_id_name) = tunnel_secrets()?;
    manager.delete(&scope, &api_key_name)?;
    manager.delete(&scope, &tunnel_id_name)?;
    println!("Removed the saved OpenAI tunnel credentials.");
    Ok(())
}

fn prompt_line(prompt: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        std::io::stdin().is_terminal(),
        "tunnel setup requires an interactive terminal"
    );
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut value = String::new();
    std::io::stdin().read_line(&mut value)?;
    Ok(value.trim().to_string())
}

fn prompt_hidden(prompt: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        std::io::stdin().is_terminal(),
        "tunnel setup requires an interactive terminal"
    );
    print!("{prompt}");
    std::io::stdout().flush()?;
    terminal::enable_raw_mode().context("failed to disable terminal echo for the API key")?;
    let _raw_mode = RawTerminalMode;
    let mut value = String::new();
    loop {
        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Enter => break,
                KeyCode::Esc => anyhow::bail!("tunnel setup cancelled"),
                KeyCode::Backspace => {
                    value.pop();
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    anyhow::bail!("tunnel setup cancelled")
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    value.push(character);
                }
                _ => {}
            },
            Event::Paste(pasted) => value.push_str(&pasted),
            _ => {}
        }
    }
    drop(_raw_mode);
    println!();
    anyhow::ensure!(
        !value.trim().is_empty() && !value.chars().any(char::is_whitespace),
        "API key must be a non-empty value without whitespace"
    );
    Ok(value.trim().to_string())
}

struct RawTerminalMode;

impl Drop for RawTerminalMode {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

fn has_saved_account_choice() -> bool {
    data_directory()
        .ok()
        .is_some_and(|directory| directory.join("selected-account-tier.json").is_file())
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
        ControlAction::Login => "/login",
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
