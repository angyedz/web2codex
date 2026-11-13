//! Commands for managing Web2Codex's private native WebView process.

use std::fs;
use std::io::{IsTerminal, Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

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
    /// Hide the browser window; active agent turns continue in the background.
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
        /// Workspace associated with the launcher; execution uses the active Codex turn.
        #[arg(long)]
        workspace: PathBuf,
    },
    /// Configure, inspect, or remove OpenAI Secure MCP Tunnel credentials.
    #[command(hide = true)]
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
    #[serde(default)]
    bridge_protocol: u32,
}

/// Returns transient provider overrides that force the local TUI through ChatGPT Web.
/// This deliberately wins over configured model providers: Web2Codex has no API-model fallback.
pub(crate) fn interactive_provider_overrides(cwd: Option<&Path>) -> anyhow::Result<Vec<String>> {
    let workspace = match cwd {
        Some(cwd) if cwd.is_absolute() => cwd.to_path_buf(),
        Some(cwd) => std::env::current_dir()?.join(cwd),
        None => std::env::current_dir()?,
    };
    ensure_browser_started()?;
    start_configured_tunnel_if_available(&workspace);
    let state = read_state()?;
    anyhow::ensure!(
        state.address.ip().is_loopback(),
        "Web2Codex browser daemon must listen on a loopback address"
    );
    anyhow::ensure!(
        !state.token.is_empty(),
        "Web2Codex browser daemon state has no bearer credential"
    );

    let origin = format!("http://{}", state.address);
    let base_url = format!("{origin}/v1");
    let model_catalog_url = format!("{origin}/models");
    let quote =
        |value: &str| serde_json::to_string(value).expect("string serialization cannot fail");

    Ok(vec![
        "model_provider=\"web2codex\"".to_string(),
        format!("model_providers.web2codex.name={}", quote("Web2Codex")),
        format!("model_providers.web2codex.base_url={}", quote(&base_url)),
        format!(
            "model_providers.web2codex.model_catalog_url={}",
            quote(&model_catalog_url)
        ),
        format!(
            "model_providers.web2codex.experimental_bearer_token={}",
            quote(&state.token)
        ),
        "model_providers.web2codex.wire_api=\"responses\"".to_string(),
        "model_providers.web2codex.requires_openai_auth=false".to_string(),
        "model_providers.web2codex.supports_websockets=false".to_string(),
        "model_providers.web2codex.supports_standalone_web_search=false".to_string(),
        // Retrying an inference request could submit the same prompt twice.
        "model_providers.web2codex.request_max_retries=0".to_string(),
        "model_providers.web2codex.stream_max_retries=0".to_string(),
        "web_search_mode=\"disabled\"".to_string(),
        "analytics.enabled=false".to_string(),
        "feedback.enabled=false".to_string(),
        "cloud.skills.enabled=false".to_string(),
        "include_apps_instructions=false".to_string(),
        "features.apps=false".to_string(),
        // Image generation is an OpenAI API endpoint; ChatGPT Web owns its image tools.
        "features.image_generation=false".to_string(),
        "features.realtime_conversation=false".to_string(),
        "features.use_agent_identity=false".to_string(),
        "features.runtime_agent_instances=false".to_string(),
        // ChatGPT account backed plugin catalogs and sharing are not part of
        // the Web2Codex runtime. Local plugin MCP servers remain available.
        "features.remote_plugin=false".to_string(),
        "features.plugin_sharing=false".to_string(),
        // Guardian V2 starts a separate reviewer model session for tool approvals. Keep it
        // disabled here: the only supported inference path is the ChatGPT WebView. The normal
        // harness approval prompts and sandbox checks remain active.
        "features.guardianv2=false".to_string(),
        "features.guardianv2_decisions_comparison=false".to_string(),
    ])
}

/// Stop the independent WebView process when its owning interactive harness exits.
/// The signed-in profile remains on disk, so the next launch can restore the
/// Temporary Chat without leaving a browser process consuming memory.
pub(crate) fn stop_after_interactive() -> anyhow::Result<()> {
    match send_control(ControlAction::Stop) {
        Ok(()) => Ok(()),
        Err(error) if error.to_string().contains("state is missing") => Ok(()),
        Err(error) => Err(error).context("failed to stop the Web2Codex browser"),
    }
}

/// Open the private ChatGPT window before the interactive TUI loads its model catalog.
/// On a first run this shows the account chooser; on later runs it opens a Temporary Chat.
/// Waiting here lets a user finish sign-in before the TUI asks the local provider for models.
pub(crate) fn prepare_interactive_launch() -> anyhow::Result<()> {
    configure_tunnel_on_first_launch()?;
    let capabilities_path = data_directory()?.join("account-capabilities.json");
    let has_cached_models = read_account_capabilities(&capabilities_path)
        .is_some_and(|capabilities| !capabilities.picker_options.is_empty());
    ensure_browser_started()?;
    send_control(if has_saved_account_choice() {
        ControlAction::Open
    } else {
        ControlAction::Login
    })?;

    // A previously authenticated profile already has a usable model catalog.
    // Reopening its Temporary Chat must not block startup while the background
    // probe refreshes the same list.
    if has_cached_models {
        return Ok(());
    }

    let deadline = Instant::now() + Duration::from_secs(5 * 60);
    eprintln!(
        "Waiting for ChatGPT Web models. Sign in in the Web2Codex window; press Ctrl+C to cancel."
    );
    while Instant::now() < deadline {
        if read_account_capabilities(&capabilities_path)
            .is_some_and(|capabilities| !capabilities.picker_options.is_empty())
        {
            return Ok(());
        }
        if send_control(ControlAction::Status).is_err() {
            anyhow::bail!(
                "Web2Codex browser stopped while waiting for ChatGPT models; run `web2codex web` and try again"
            );
        }
        thread::sleep(Duration::from_millis(500));
    }

    anyhow::bail!(
        "No ChatGPT Web models were detected after five minutes. Finish signing in and check `web2codex web models`, then run `web2codex` again."
    );
}

fn configure_tunnel_on_first_launch() -> anyhow::Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Ok(());
    }
    let home = find_codex_home()?;
    let setup_marker = home.join("first-run-setup-v1");
    if setup_marker.is_file() {
        return Ok(());
    }

    let (manager, scope, api_key_name, tunnel_id_name) = tunnel_secrets()?;
    let has_api_key = manager.get(&scope, &api_key_name)?.is_some();
    let has_tunnel_id = manager.get(&scope, &tunnel_id_name)?.is_some();
    if has_api_key && has_tunnel_id {
        fs::write(setup_marker, b"completed\n")?;
        return Ok(());
    }

    println!("First run setup: connect the ChatGPT MCP tools through OpenAI Tunnel.");
    println!("ChatGPT connector name: Web2Codex");
    let choice = prompt_line("Configure the tunnel now? [Y/n]: ")?;
    if matches!(choice.trim().to_ascii_lowercase().as_str(), "n" | "no") {
        fs::write(setup_marker, b"skipped\n")?;
        println!("Tunnel setup skipped. Configure it later with `/mcp tunnel` in Web2Codex.");
        return Ok(());
    }

    configure_tunnel_credentials()?;
    fs::write(setup_marker, b"completed\n")?;
    Ok(())
}

fn read_account_capabilities(path: &Path) -> Option<AccountCapabilities> {
    let contents = fs::read(path).ok()?;
    let capabilities = serde_json::from_slice::<AccountCapabilities>(&contents).ok()?;
    (capabilities.probe_version == 3).then_some(capabilities)
}

fn start_configured_tunnel_if_available(workspace: &Path) {
    let Ok((manager, scope, api_key_name, tunnel_id_name)) = tunnel_secrets() else {
        return;
    };
    let Ok(Some(api_key)) = manager.get(&scope, &api_key_name) else {
        return;
    };
    let Ok(Some(tunnel_id)) = manager.get(&scope, &tunnel_id_name) else {
        return;
    };
    if let Err(error) = crate::web_tunnel::start(&tunnel_id, &api_key, workspace) {
        eprintln!("Web2Codex MCP tunnel could not start automatically: {error}");
        eprintln!("In the chat, enter `/mcp tunnel` to configure it again.");
    }
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
            println!("Web2Codex browser is running in the background. Its window is hidden.");
        }
        WebSubcommand::Open | WebSubcommand::Setup => {
            ensure_browser_started()?;
            send_control(ControlAction::Open)?;
            println!("Opened the private Web2Codex window.");
        }
        WebSubcommand::Login => {
            ensure_browser_started()?;
            send_control(ControlAction::Login)?;
            println!("Opened the ChatGPT account chooser.");
        }
        WebSubcommand::Hide => {
            send_control(ControlAction::Hide)?;
            println!("Hid the Web2Codex window; the lightweight controller is still running.");
        }
        WebSubcommand::Status => match send_control(ControlAction::Status) {
            Ok(()) => println!("Web2Codex browser is running."),
            Err(_) => println!("Web2Codex browser is stopped."),
        },
        WebSubcommand::Models => print_account_capabilities()?,
        WebSubcommand::Stop => match send_control(ControlAction::Stop) {
            Ok(()) => println!("Stopped the Web2Codex browser."),
            Err(_) => println!("Web2Codex browser is already stopped."),
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
    let path = data_directory()?.join("account-capabilities.json");
    let contents = match fs::read(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("ChatGPT Web picker snapshot: unknown (not detected yet).");
            println!("Open ChatGPT with `web2codex web open`, then check again.");
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
            println!("Open ChatGPT with `web2codex web open` to refresh the scan.");
            return Ok(());
        }
    };
    if capabilities.probe_version != 3 {
        println!("ChatGPT Web picker snapshot: unknown (scan format is unsupported).");
        println!("Open ChatGPT with `web2codex web open` to refresh the scan.");
        return Ok(());
    }

    println!(
        "ChatGPT Web picker snapshot (last observed at Unix time {}):",
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
    let api_key =
        prompt_hidden("OpenAI tunnel runtime API key (Tunnels Read + Use, not an admin key): ")?;
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
    println!("Tunnel credentials saved in Web2Codex's encrypted secret store.");
    start_tunnel_runtime().context("credentials were saved, but the MCP tunnel did not start")
}

fn start_tunnel_runtime() -> anyhow::Result<()> {
    ensure_browser_started()?;
    let (manager, scope, api_key_name, tunnel_id_name) = tunnel_secrets()?;
    let api_key = manager.get(&scope, &api_key_name)?.context(
        "OpenAI tunnel API key is not configured; open the chat and enter `/mcp tunnel`",
    )?;
    let tunnel_id = manager
        .get(&scope, &tunnel_id_name)?
        .context("OpenAI tunnel ID is not configured; open the chat and enter `/mcp tunnel`")?;
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
        let state = read_state()?;
        if state.bridge_protocol == 2 {
            return Ok(());
        }
        // Old browser builds wrote state without a protocol version. Ask the
        // owner process to stop, but tolerate a legacy server that cannot
        // complete the current authenticated control handshake.
        let _ = send_control(ControlAction::Stop);
        if state.address.ip().is_loopback() {
            let _ = std::net::TcpStream::connect_timeout(
                &state.address,
                Duration::from_millis(300),
            )
            .and_then(|mut stream| {
                stream.set_write_timeout(Some(Duration::from_millis(300)))?;
                stream.write_all(b"POST /stop HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            });
        }
        // The same-user legacy process is our own background WebView. Stop it
        // by PID if it ignored the protocol-aware request, then wait for its
        // lock to be released before starting the updated helper.
        #[cfg(windows)]
        if state.address.ip().is_loopback() {
            let lock_path = data_directory()?.join("browser.lock");
            if let Ok(pid_text) = fs::read_to_string(&lock_path)
                && let Ok(pid) = pid_text.trim().parse::<u32>()
            {
                let _ = Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
        for _ in 0..50 {
            if send_control(ControlAction::Status).is_err()
                && !data_directory()?.join("browser.lock").exists()
            {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        anyhow::ensure!(
            !data_directory()?.join("browser.lock").exists(),
            "the previous Web2Codex browser did not stop; close its Web2Codex window and retry"
        );
    }

    let executable = find_browser_executable().context(
        "the bundled Web2Codex browser was not found; reinstall Web2Codex or set WEB2CODEX_WEBVIEW_BIN",
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
            "failed to start the Web2Codex browser at {}",
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
                "Web2Codex browser exited during startup with {status}; check that a graphical session and the required native WebView runtime are available"
            );
        }
    }
    let _ = child.kill();
    let status = child.wait().ok();
    anyhow::bail!(
        "Web2Codex browser did not start{}; check that a graphical session and the required native WebView runtime are available",
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
    stream.set_read_timeout(Some(
        if matches!(action, ControlAction::Open | ControlAction::Login) {
            Duration::from_secs(16)
        } else {
            Duration::from_millis(700)
        },
    ))?;
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        state.token
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    if !response.starts_with("HTTP/1.1 204") {
        let body = response.split_once("\r\n\r\n").map_or("", |(_, body)| body);
        anyhow::bail!("Web2Codex browser rejected the control request: {body}");
    }
    Ok(())
}

fn read_state() -> anyhow::Result<BrowserState> {
    let state_path = data_directory()?.join("browser-state.json");
    let contents = fs::read_to_string(&state_path).with_context(|| {
        format!(
            "Web2Codex browser state is missing: {}",
            state_path.display()
        )
    })?;
    serde_json::from_str(&contents).context("Web2Codex browser state is invalid")
}

pub(crate) fn bridge_credentials() -> anyhow::Result<(SocketAddr, String)> {
    let state = read_state()?;
    anyhow::ensure!(
        state.address.ip().is_loopback() && !state.token.is_empty() && state.bridge_protocol == 2,
        "Invalid local Web2Codex bridge state"
    );
    Ok((state.address, state.token))
}

fn find_browser_executable() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("WEB2CODEX_WEBVIEW_BIN").map(PathBuf::from) {
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
    if let Some(configured) = std::env::var_os("WEB2CODEX_WEB_DATA_DIR") {
        let path = PathBuf::from(configured);
        fs::create_dir_all(&path).with_context(|| {
            format!(
                "failed to create Web2Codex browser data directory {}",
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
            .join("Web2Codex/Browser")
    } else if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Web2Codex/Browser")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("web2codex/browser")
    };
    fs::create_dir_all(&path).with_context(|| {
        format!(
            "failed to create Web2Codex browser data directory {}",
            path.display()
        )
    })?;
    Ok(path)
}
