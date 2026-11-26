//! Commands for managing Web2Codex's private native WebView process.

use std::fs;
use std::io::{IsTerminal, Read, Seek, SeekFrom, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::Args;
use clap::Subcommand;
use codex_core::config::find_codex_home;
use codex_secrets::{SecretName, SecretScope, SecretsBackendKind, SecretsManager};
use serde::Deserialize;

const TUNNEL_API_KEY_NAME: &str = "OPENAI_TUNNEL_API_KEY";
const TUNNEL_ID_NAME: &str = "OPENAI_TUNNEL_ID";
const TUNNEL_SETTINGS_URL: &str = "https://platform.openai.com/settings/organization/tunnels";
const API_KEYS_SETTINGS_URL: &str = "https://platform.openai.com/settings/organization/api-keys";
const CONNECTOR_SETTINGS_URL: &str = "https://chatgpt.com/#settings/Connectors";
const SMOKE_TEST_PROMPT: &str = "Reply with exactly: READY TO WORK.";
const SMOKE_TEST_REPLY: &str = "READY TO WORK.";
const SUBAGENT_COMPATIBILITY_OVERRIDES: &[&str] = &[
    "features.multi_agent=true",
    "features.multi_agent_v2=false",
    "agents.max_depth=2",
];
const SMOKE_TEST_TOTAL_TIMEOUT: Duration = Duration::from_secs(150);
const SETUP_SMOKE_TEST_METADATA_OVERRIDE: &str =
    "responses_api_metadata={web2codex_setup_smoke_test=\"true\"}";
const MAX_SMOKE_TEST_MODELS: usize = 4;
const ACCOUNT_CAPABILITIES_PROBE_VERSION: u8 = 5;

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
    /// Open and list browser tabs, including from a Web2Codex tool call.
    Tab {
        #[command(subcommand)]
        command: TabSubcommand,
    },
    /// Stop the background browser process.
    Stop,
    /// Internal MCP server process used by OpenAI Secure MCP Tunnel.
    #[command(name = "mcp-server", hide = true)]
    McpServer {
        /// Workspace associated with the launcher; execution uses the active Web2Codex turn.
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
enum TabSubcommand {
    /// Open a URL in a background tab.
    Open { url: String },
    /// List browser tabs.
    List,
    /// Close a finished subagent's browser tab without opening or focusing the browser.
    #[command(name = "close-agent", hide = true)]
    CloseAgent {
        thread_id: String,
        #[arg(long)]
        agent_path: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum TunnelSubcommand {
    /// Save tunnel credentials and start the local MCP tunnel runtime.
    Configure {
        /// The native Web2Codex setup screen already opened its persistent browser profile.
        #[arg(long, hide = true)]
        browser_opened: bool,
    },
    /// Start the local MCP tunnel runtime in the background.
    Start,
    /// Check whether the local MCP tunnel runtime is ready.
    Status,
    /// Stop the local MCP tunnel runtime.
    Stop,
    /// Remove the saved tunnel ID and API key.
    Remove,
    /// Send a temporary ChatGPT Web smoke-test request and verify its exact reply.
    SmokeTest,
}

#[derive(Debug, Deserialize)]
struct BrowserState {
    address: SocketAddr,
    token: String,
    #[serde(default)]
    bridge_protocol: u32,
}

/// Holding the socket ties the helper lifetime to the command, even if its
/// process exits before Rust can run destructors.
pub(crate) struct BrowserSession {
    state: BrowserState,
    _lease: TcpStream,
}

impl BrowserSession {
    pub(crate) fn connect() -> anyhow::Result<Self> {
        for attempt in 0..2 {
            ensure_browser_started(true)?;
            let state = read_state()?;
            match Self::acquire_socket(&state) {
                Ok(socket) => {
                    return Ok(Self {
                        state,
                        _lease: socket,
                    });
                }
                // The previous owner might have closed its final socket just
                // as we connected. Retry only this handshake, before inference.
                Err(_) if attempt == 0 => thread::sleep(Duration::from_millis(150)),
                Err(error) => {
                    return Err(error.context("could not attach to the Web2Codex browser"));
                }
            }
        }
        unreachable!()
    }

    fn acquire_socket(state: &BrowserState) -> anyhow::Result<TcpStream> {
        let mut stream = TcpStream::connect_timeout(&state.address, Duration::from_secs(1))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        stream.set_read_timeout(Some(Duration::from_secs(6)))?;
        write!(
            stream,
            "POST /lease HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nConnection: Upgrade\r\nUpgrade: web2codex-lease\r\nContent-Length: 0\r\n\r\n",
            state.token
        )?;
        const ACCEPTED: &[u8] = b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: web2codex-lease\r\n\r\n";
        let mut response = [0; ACCEPTED.len()];
        stream.read_exact(&mut response)?;
        anyhow::ensure!(
            response.as_slice() == ACCEPTED,
            "browser rejected the session lease"
        );
        Ok(stream)
    }

    pub(crate) fn provider_overrides(&self, cwd: Option<&Path>) -> anyhow::Result<Vec<String>> {
        let workspace = match cwd {
            Some(cwd) if cwd.is_absolute() => cwd.to_path_buf(),
            Some(cwd) => std::env::current_dir()?.join(cwd),
            None => std::env::current_dir()?,
        };
        start_configured_tunnel_if_available(&workspace)?;
        let state = &self.state;

        let origin = format!("http://{}", state.address);
        let base_url = format!("{origin}/v1");
        let model_catalog_url = format!("{origin}/models");
        let quote =
            |value: &str| serde_json::to_string(value).expect("string serialization cannot fail");

        let mut overrides = vec![
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
            "analytics.enabled=false".to_string(),
            "feedback.enabled=false".to_string(),
            "cloud.skills.enabled=false".to_string(),
            // Keep skill catalogs out of every request; relevant skills stay available on demand.
            "skills.include_instructions=false".to_string(),
            "include_apps_instructions=false".to_string(),
            "features.apps=false".to_string(),
            // Image generation is an OpenAI API endpoint; ChatGPT Web owns its image tools.
            "features.image_generation=false".to_string(),
            "features.realtime_conversation=false".to_string(),
            "features.use_agent_identity=false".to_string(),
            "features.runtime_agent_instances=false".to_string(),
            // A Web2Codex context profile applies to every model, including child agents.
            "features.subagent_default_context_limits=false".to_string(),
            // ChatGPT account backed plugin catalogs and sharing are not part of
            // the Web2Codex runtime. Local plugin MCP servers remain available.
            "features.remote_plugin=false".to_string(),
            "features.plugin_sharing=false".to_string(),
            // Guardian V2 starts a separate reviewer model session for tool approvals. Keep it
            // disabled here: the only supported inference path is the ChatGPT WebView. The normal
            // harness approval prompts and sandbox checks remain active.
            "features.guardianv2=false".to_string(),
            "features.guardianv2_decisions_comparison=false".to_string(),
        ];
        // Keep routed child requests on the plaintext, cross-backend-compatible V1 surface.
        // This is also the protocol used by codex-chatgpt-web for new installations.
        overrides.extend(
            SUBAGENT_COMPATIBILITY_OVERRIDES
                .iter()
                .map(|value| (*value).to_string()),
        );
        Ok(overrides)
    }

    pub(crate) fn prepare_interactive_launch(&self) -> anyhow::Result<()> {
        let workspace =
            std::env::current_dir().context("failed to resolve the workspace directory")?;
        start_configured_tunnel_if_available(&workspace)?;
        let capabilities_path = data_directory()?.join("account-capabilities.json");
        let has_cached_models = read_account_capabilities(&capabilities_path)
            .is_some_and(|capabilities| !capabilities.picker_options.is_empty());
        if has_cached_models {
            return Ok(());
        }
        send_control_to(
            &self.state,
            if has_saved_account_choice() {
                ControlAction::SessionOpen
            } else {
                ControlAction::SessionLogin
            },
        )?;

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
            if send_control_to(&self.state, ControlAction::Status).is_err() {
                anyhow::bail!(
                    "Web2Codex browser stopped while waiting for ChatGPT models; run `web2codex web` and try again"
                );
            }
            thread::sleep(Duration::from_millis(500));
        }

        anyhow::bail!(
            "No ChatGPT Web models were detected after five minutes. Finish signing in and check `web2codex web models`, then run `web2codex` again."
        )
    }
}

impl Drop for BrowserSession {
    fn drop(&mut self) {
        crate::web_tunnel::stop_auto_started();
    }
}

fn read_account_capabilities(path: &Path) -> Option<AccountCapabilities> {
    let contents = fs::read(path).ok()?;
    let capabilities = serde_json::from_slice::<AccountCapabilities>(&contents).ok()?;
    (capabilities.probe_version == ACCOUNT_CAPABILITIES_PROBE_VERSION).then_some(capabilities)
}

fn start_configured_tunnel_if_available(workspace: &Path) -> anyhow::Result<()> {
    let (manager, scope, api_key_name, tunnel_id_name) = tunnel_secrets()?;
    let Some(api_key) = manager.get(&scope, &api_key_name)? else {
        return Ok(());
    };
    let Some(tunnel_id) = manager.get(&scope, &tunnel_id_name)? else {
        return Ok(());
    };
    crate::web_tunnel::start(&tunnel_id, &api_key, workspace)
        .context("saved MCP tunnel credentials exist, but the tunnel failed to start")
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
    SessionOpen,
    SessionLogin,
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
            ensure_browser_started(true)?;
            send_control(ControlAction::Hide)?;
            println!(
                "Web2Codex browser is hidden and ready to attach. It closes automatically after the attach window expires or the Web2Codex session ends."
            );
        }
        WebSubcommand::Open | WebSubcommand::Setup => {
            ensure_browser_started(true)?;
            send_control(ControlAction::Open)?;
            println!("Opened the private Web2Codex window.");
        }
        WebSubcommand::Login => {
            ensure_browser_started(true)?;
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
        WebSubcommand::Tab { command } => match command {
            TabSubcommand::Open { url } => {
                ensure_browser_started(true)?;
                browser_tabs_request("POST", Some(&url))?;
                println!("Opened a Web2Codex browser tab: {url}");
            }
            TabSubcommand::List => {
                ensure_browser_started(true)?;
                println!("{}", browser_tabs_request("GET", None)?);
            }
            TabSubcommand::CloseAgent {
                thread_id,
                agent_path,
            } => {
                close_agent_browser_tab(&thread_id, agent_path.as_deref())?;
            }
        },
        WebSubcommand::Stop => match send_control(ControlAction::Stop) {
            Ok(()) => println!("Stopped the Web2Codex browser."),
            Err(error)
                if error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                    matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    )
                }) =>
            {
                println!("Web2Codex browser is already stopped.");
            }
            Err(error) => return Err(error.context("Could not confirm Web2Codex browser shutdown")),
        },
        WebSubcommand::McpServer { workspace } => crate::web_mcp_server::run(workspace)?,
        WebSubcommand::Tunnel { command } => match command {
            TunnelSubcommand::Configure { browser_opened } => {
                configure_tunnel_credentials(browser_opened)?
            }
            TunnelSubcommand::Start => start_tunnel_runtime()?,
            TunnelSubcommand::Status => crate::web_tunnel::status()?,
            TunnelSubcommand::Stop => crate::web_tunnel::stop()?,
            TunnelSubcommand::Remove => remove_tunnel_credentials()?,
            TunnelSubcommand::SmokeTest => run_standalone_smoke_test()?,
        },
    }
    Ok(())
}

fn browser_tabs_request(method: &str, url: Option<&str>) -> anyhow::Result<String> {
    let state = read_state()?;
    let body = url
        .map(|url| serde_json::json!({"url":url}).to_string())
        .unwrap_or_default();
    let mut stream = TcpStream::connect_timeout(&state.address, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    write!(
        stream,
        "{method} /browser/tabs HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        state.token,
        body.len(),
        body
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let (header, body) = response
        .split_once("\r\n\r\n")
        .context("invalid browser response")?;
    anyhow::ensure!(
        header.starts_with("HTTP/1.1 200"),
        "browser tab request failed: {body}"
    );
    Ok(body.to_string())
}

fn close_agent_browser_tab(thread_id: &str, agent_path: Option<&str>) -> anyhow::Result<()> {
    if thread_id.trim().is_empty() || thread_id.len() > 256 {
        return Ok(());
    }
    let Ok(state) = read_state() else {
        return Ok(());
    };
    let body = serde_json::json!({
        "action": "close-agent",
        "thread_id": thread_id,
        "agent_path": agent_path,
    })
    .to_string();
    let mut stream = TcpStream::connect_timeout(&state.address, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(6)))?;
    write!(
        stream,
        "POST /browser/tabs HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        state.token,
        body.len(),
        body
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    anyhow::ensure!(
        response.starts_with("HTTP/1.1 204") || response.starts_with("HTTP/1.1 202"),
        "the Web2Codex browser could not close the finished agent tab"
    );
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
    if capabilities.probe_version != ACCOUNT_CAPABILITIES_PROBE_VERSION {
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

fn configure_tunnel_credentials(browser_opened: bool) -> anyhow::Result<()> {
    println!("\n=== Web2Codex: MCP setup ===");
    if !browser_opened {
        println!("Step 1 · Sign in to ChatGPT in your Web2Codex browser.");
        println!(
            "The browser uses a persistent profile, so your session remains available after restart."
        );
        ensure_browser_started(true)?;
        send_control(ControlAction::Login)?;
        prompt_line("Sign in if needed, then return here and press Enter to continue: ")?;
    }

    let name_path = data_directory()?.join("connector-name.txt");
    let existing_name = fs::read_to_string(&name_path)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    let connector_name = if let Some(name) = existing_name {
        println!("Saved connector name: {name}");
        name
    } else {
        println!("Choose the name you will use for the app in ChatGPT:");
        println!("  1. Generate a name from two random words (default)");
        println!("  2. Enter my own name");
        'choice: loop {
            match prompt_line("Choice [1/2]: ")?.as_str() {
                "" | "1" => break 'choice random_connector_name(),
                "2" => loop {
                    let name =
                        prompt_line("Connector name (1-64 letters, numbers, spaces or hyphens): ")?;
                    if !name.is_empty()
                        && name.len() <= 64
                        && name
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '-')
                    {
                        break 'choice name;
                    }
                    println!("Please use 1-64 letters, numbers, spaces or hyphens.");
                },
                _ => println!("Please enter 1 or 2."),
            }
        }
    };
    println!("Connector name: {connector_name}");
    let (manager, scope, api_key_name, tunnel_id_name) = tunnel_secrets()?;
    if manager.get(&scope, &api_key_name)?.is_some()
        && manager.get(&scope, &tunnel_id_name)?.is_some()
    {
        let reuse = prompt_line("Use the saved tunnel ID and key? [Y/n]: ")?;
        if !matches!(reuse.to_ascii_lowercase().as_str(), "n" | "no") {
            fs::write(&name_path, &connector_name).context("failed to save the connector name")?;
            println!("Name saved. Starting the tunnel; this may take a minute...");
            start_tunnel_runtime()
                .context("the connector name was saved, but the MCP tunnel did not start")?;
            return finish_tunnel_setup(&connector_name);
        }
    }
    println!("\nStep 2 · Create your OpenAI Tunnel");
    println!("The Web2Codex browser will open the organization Tunnels page in a new tab.");
    if let Err(error) = open_setup_page(TUNNEL_SETTINGS_URL) {
        println!("Open this page in the Web2Codex browser: {TUNNEL_SETTINGS_URL} ({error:#})");
    }
    println!("Create a tunnel for this organization, then copy its tunnel ID.");
    let tunnel_id = loop {
        let value = prompt_line("Tunnel ID: ")?;
        if is_valid_tunnel_id(&value) {
            break value;
        }
        println!(
            "That ID does not look right. Copy the full tunnel ID from the Tunnels page and try again."
        );
    };
    println!("\nStep 3 · Create a runtime API key");
    println!("The Web2Codex browser will open the organization API Keys page in a new tab.");
    if let Err(error) = open_setup_page(API_KEYS_SETTINGS_URL) {
        println!("Open this page in the Web2Codex browser: {API_KEYS_SETTINGS_URL} ({error:#})");
    }
    println!(
        "Create a runtime key with expiration set to Never and Tunnels Read + Use permissions."
    );
    println!(
        "Do not use an admin key. Copy the runtime key; it stays hidden while you paste it below."
    );
    let api_key =
        prompt_hidden("OpenAI tunnel runtime API key (Tunnels Read + Use, not an admin key): ")?;
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
    fs::write(&name_path, &connector_name).context("failed to save the connector name")?;
    println!("Credentials saved securely. Starting the tunnel; this may take a minute...");
    start_tunnel_runtime().context("credentials were saved, but the MCP tunnel did not start")?;
    finish_tunnel_setup(&connector_name)
}

fn open_setup_page(url: &str) -> anyhow::Result<()> {
    ensure_browser_started(true)?;
    browser_tabs_request("POST", Some(url))?;
    println!("Opened in a new Web2Codex browser tab: {url}");
    Ok(())
}

fn finish_tunnel_setup(connector_name: &str) -> anyhow::Result<()> {
    let (manager, scope, _, tunnel_id_name) = tunnel_secrets()?;
    let tunnel_id = manager
        .get(&scope, &tunnel_id_name)?
        .context("the saved tunnel ID is unavailable")?;

    println!("\nStep 4 · Add the connector in ChatGPT");
    if let Err(error) = open_setup_page(CONNECTOR_SETTINGS_URL) {
        println!("Open this page in the Web2Codex browser: {CONNECTOR_SETTINGS_URL} ({error:#})");
    }
    println!("1. Add a custom MCP connector and choose Connection: Tunnel.");
    println!("2. Use this exact connector name: {connector_name}");
    println!("3. Use this tunnel ID: {tunnel_id}");
    println!("4. Save and enable the connector. For Temporary Chats, select Personalized mode.");
    println!(
        "\nWhen the connector is saved, press Enter to test ChatGPT Web. Type S to skip for now."
    );
    let choice = prompt_line("Run smoke test? [Y/n]: ")?;
    if matches!(
        choice.to_ascii_lowercase().as_str(),
        "n" | "no" | "s" | "skip"
    ) {
        println!("Smoke test skipped. Run `web2codex web tunnel smoke-test` after signing in.");
        return Ok(());
    }

    let smoke_result = run_browser_smoke_test();
    if let Err(error) = smoke_result {
        let keep_tunnel_result = start_tunnel_runtime();
        return match keep_tunnel_result {
            Ok(()) => Err(error),
            Err(restart_error) => Err(error.context(format!(
                "smoke test failed and the tunnel could not be left running: {restart_error:#}"
            ))),
        };
    }

    start_tunnel_runtime()
        .context("smoke test passed, but the MCP tunnel could not be left running")?;
    println!("Smoke test passed. ChatGPT Web replied exactly: {SMOKE_TEST_REPLY}");
    Ok(())
}

fn run_standalone_smoke_test() -> anyhow::Result<()> {
    let result = run_browser_smoke_test();
    let tunnel_result = start_tunnel_runtime();
    result?;
    tunnel_result.context("smoke test passed, but the MCP tunnel could not be left running")?;
    println!("Smoke test passed. ChatGPT Web replied exactly: {SMOKE_TEST_REPLY}");
    Ok(())
}

fn run_browser_smoke_test() -> anyhow::Result<()> {
    let capabilities_path = data_directory()?.join("account-capabilities.json");
    let capabilities = read_account_capabilities(&capabilities_path).context(
        "no ChatGPT model was detected yet; sign in using `web2codex web login`, then run `web2codex web models`",
    )?;
    let (models, omitted_models) = smoke_test_models(&capabilities.picker_options);
    anyhow::ensure!(
        !models.is_empty(),
        "the signed-in ChatGPT model picker did not expose any models"
    );
    // Keep smoke-test prompts away from project instructions and MCP settings.
    // Authentication still comes from the user's Web2Codex home.
    let workspace = tempfile::Builder::new()
        .prefix("web2codex-smoke-workspace-")
        .tempdir()
        .context("could not create an isolated smoke-test workspace")?;
    let deadline = Instant::now() + SMOKE_TEST_TOTAL_TIMEOUT;
    let mut passed_count = 0;
    let mut failed_count = 0;

    for (index, model) in models.iter().enumerate() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let models_left = models.len() - index;
        let per_model_timeout =
            Duration::from_millis((remaining.as_millis() / models_left as u128).max(1) as u64);
        let output_path =
            std::env::temp_dir().join(format!("web2codex-smoke-{}.txt", uuid::Uuid::new_v4()));

        println!(
            "Smoke test {}/{} · {model} (up to {:.1} seconds)",
            index + 1,
            models.len(),
            per_model_timeout.as_secs_f64()
        );
        let result =
            run_smoke_test_process(workspace.path(), model, &output_path, per_model_timeout);
        let _ = fs::remove_file(&output_path);
        match result {
            Ok(()) => {
                passed_count += 1;
                println!("Smoke test passed: {model}");
            }
            Err(error) => {
                failed_count += 1;
                eprintln!("Smoke test failed: {model}: {error:#}");
            }
        }
    }

    let timed_out_models = models.len().saturating_sub(passed_count + failed_count);
    let not_checked = omitted_models + timed_out_models;
    let visible_model_count = models.len() + omitted_models;
    if failed_count == 0 && not_checked == 0 {
        println!("Smoke test passed for all {visible_model_count} visible models.");
        return Ok(());
    }

    let failure_details = (failed_count > 0)
        .then(|| format!(" {failed_count} model checks failed."))
        .unwrap_or_default();
    let omitted_details = (omitted_models > 0)
        .then(|| {
            format!(" {omitted_models} additional models were skipped by the 4-model safety cap.")
        })
        .unwrap_or_default();
    let timeout_details = (timed_out_models > 0)
        .then(|| {
            format!(
                " {timed_out_models} selected models were not checked before the 150-second limit."
            )
        })
        .unwrap_or_default();
    anyhow::bail!(
        "Setup smoke test checked {}/{} visible models: {} passed, {failed_count} failed, {not_checked} not checked.{failure_details}{omitted_details}{timeout_details}",
        passed_count + failed_count,
        visible_model_count,
        passed_count
    )
}

fn smoke_test_models(picker_options: &[String]) -> (Vec<String>, usize) {
    let mut models: Vec<String> = Vec::new();
    for option in picker_options {
        let model = option.trim();
        if model.is_empty() || models.iter().any(|existing| existing.as_str() == model) {
            continue;
        }
        models.push(model.to_owned());
    }
    let omitted = models.len().saturating_sub(MAX_SMOKE_TEST_MODELS);
    models.truncate(MAX_SMOKE_TEST_MODELS);
    (models, omitted)
}

fn run_smoke_test_process(
    workspace: &Path,
    model: &str,
    output_path: &Path,
    timeout: Duration,
) -> anyhow::Result<()> {
    let executable = std::env::current_exe().context("failed to locate Web2Codex")?;
    let mut diagnostics =
        tempfile::tempfile().context("could not create smoke-test diagnostic file")?;
    let mut command = Command::new(executable);
    command
        .args(smoke_test_exec_args(workspace, model, output_path))
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(diagnostics.try_clone()?));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }

    let mut child = command
        .spawn()
        .context("could not start the Web2Codex browser smoke test")?;
    let status = wait_for_smoke_test_process(&mut child, timeout)
        .with_context(|| smoke_test_diagnostics(&mut diagnostics))?;
    if !status.success() {
        anyhow::bail!(
            "ChatGPT smoke test could not complete ({status}). {}",
            smoke_test_diagnostics(&mut diagnostics)
        );
    }

    let reply = fs::read_to_string(output_path)
        .context("ChatGPT finished, but Web2Codex could not read the smoke-test reply")?;
    anyhow::ensure!(
        is_exact_smoke_test_reply(&reply),
        "ChatGPT returned a response other than exactly `{SMOKE_TEST_REPLY}`."
    );
    Ok(())
}

fn smoke_test_diagnostics(file: &mut fs::File) -> String {
    const MAX_DIAGNOSTIC_BYTES: u64 = 4096;
    let read_tail = (|| -> std::io::Result<String> {
        let length = file.metadata()?.len();
        file.seek(SeekFrom::Start(length.saturating_sub(MAX_DIAGNOSTIC_BYTES)))?;
        let mut bytes = Vec::new();
        (&mut *file)
            .take(MAX_DIAGNOSTIC_BYTES)
            .read_to_end(&mut bytes)?;
        Ok(String::from_utf8_lossy(&bytes).trim().to_owned())
    })();
    match read_tail {
        Ok(message) if !message.is_empty() => format!("Smoke-test diagnostics: {message}"),
        _ => "No smoke-test diagnostics were captured. Check sign-in and model availability."
            .to_owned(),
    }
}

fn wait_for_smoke_test_process(child: &mut Child, timeout: Duration) -> anyhow::Result<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(error) => {
                terminate_smoke_test_process_tree(child);
                return Err(error).context("could not monitor the smoke test");
            }
        }
        if Instant::now() >= deadline {
            terminate_smoke_test_process_tree(child);
            anyhow::bail!(
                "ChatGPT did not finish this model's smoke test within {} seconds; check the Web2Codex browser and try again",
                timeout.as_secs()
            );
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        thread::sleep(Duration::from_millis(100).min(remaining));
    }
}

fn terminate_smoke_test_process_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        let pid = child.id().to_string();
        let _ = Command::new("taskkill")
            .args(["/PID", pid.as_str(), "/T", "/F"])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    let _ = child.kill();
    let _ = child.wait();
}

fn smoke_test_exec_args(
    workspace: &Path,
    model: &str,
    output_path: &Path,
) -> Vec<std::ffi::OsString> {
    vec![
        "exec".into(),
        "--ephemeral".into(),
        "--ignore-user-config".into(),
        "--ignore-rules".into(),
        "--skip-git-repo-check".into(),
        "-c".into(),
        SETUP_SMOKE_TEST_METADATA_OVERRIDE.into(),
        "-C".into(),
        workspace.as_os_str().to_owned(),
        "-m".into(),
        model.into(),
        "--output-last-message".into(),
        output_path.as_os_str().to_owned(),
        SMOKE_TEST_PROMPT.into(),
    ]
}

fn is_exact_smoke_test_reply(reply: &str) -> bool {
    reply.trim() == SMOKE_TEST_REPLY
}

#[cfg(test)]
mod tests {
    use super::{
        SUBAGENT_COMPATIBILITY_OVERRIDES, is_exact_smoke_test_reply, random_connector_name,
        smoke_test_diagnostics, smoke_test_exec_args, smoke_test_models,
    };
    use std::io::Write;
    use std::path::Path;

    #[cfg(windows)]
    use super::wait_for_smoke_test_process;
    #[cfg(windows)]
    use std::process::{Command, Stdio};
    #[cfg(windows)]
    use std::time::Duration;

    #[test]
    fn smoke_test_requires_the_exact_reply() {
        assert!(is_exact_smoke_test_reply("READY TO WORK."));
        assert!(is_exact_smoke_test_reply("\nREADY TO WORK.\n"));
        assert!(!is_exact_smoke_test_reply("READY TO WORK"));
        assert!(!is_exact_smoke_test_reply("READY TO WORK. I can help."));
        assert!(!is_exact_smoke_test_reply("ready to work."));
    }

    #[test]
    fn stop_control_waits_for_delayed_shutdown_acknowledgement() {
        use super::{BrowserState, ControlAction, send_control_to};
        use std::io::{BufRead, BufReader};
        use std::net::TcpListener;
        use std::thread;
        use std::time::Duration;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let state = BrowserState {
            address: listener.local_addr().unwrap(),
            token: "test-token".to_owned(),
            bridge_protocol: super::BRIDGE_PROTOCOL,
        };
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "POST /stop HTTP/1.1\r\n");
            loop {
                line.clear();
                assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
            }
            thread::sleep(Duration::from_millis(900));
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
        });
        let result = send_control_to(&state, ControlAction::Stop);
        server.join().unwrap();
        result.unwrap();
    }

    #[test]
    fn smoke_test_diagnostics_reports_the_bounded_tail() {
        let mut file = tempfile::tempfile().expect("create diagnostic file");
        assert!(smoke_test_diagnostics(&mut file).contains("No smoke-test diagnostics"));
        file.write_all(b"old error\n").expect("write old error");
        file.write_all(&vec![b'x'; 4096])
            .expect("write long diagnostic");
        file.write_all(b"\nstream disconnected\n")
            .expect("write final error");
        let diagnostic = smoke_test_diagnostics(&mut file);
        assert!(diagnostic.ends_with("stream disconnected"));
        assert!(!diagnostic.contains("old error"));
        assert!(diagnostic.len() <= 4096 + "Smoke-test diagnostics: ".len());
    }

    #[test]
    fn web_provider_uses_compatibility_v1_for_subagents() {
        assert_eq!(
            SUBAGENT_COMPATIBILITY_OVERRIDES,
            [
                "features.multi_agent=true",
                "features.multi_agent_v2=false",
                "agents.max_depth=2",
            ]
        );
    }

    #[test]
    fn smoke_test_checks_each_distinct_visible_picker_option_in_order() {
        let options = vec![
            " GPT-6 ".to_owned(),
            "GPT-5.6".to_owned(),
            "GPT-6".to_owned(),
            "".to_owned(),
        ];
        assert_eq!(
            smoke_test_models(&options).0,
            ["GPT-6".to_owned(), "GPT-5.6".to_owned()]
        );
    }

    #[test]
    fn smoke_test_model_selection_has_a_bounded_model_count() {
        let options = (0..50)
            .map(|index| format!("Model {index}"))
            .collect::<Vec<_>>();
        let (models, omitted) = smoke_test_models(&options);
        assert_eq!(models.len(), 4);
        assert_eq!(omitted, 46);
        assert_eq!(models.first().map(String::as_str), Some("Model 0"));
        assert_eq!(models.last().map(String::as_str), Some("Model 3"));
    }

    #[test]
    fn smoke_test_runner_ignores_user_mcp_config_and_workspace_rules() {
        let args = smoke_test_exec_args(
            Path::new("isolated-workspace"),
            "GPT-6",
            Path::new("reply.txt"),
        );
        let args: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert!(args.iter().any(|arg| arg == "--ignore-user-config"));
        assert!(args.iter().any(|arg| arg == "--ignore-rules"));
        assert!(args.iter().any(|arg| arg == "isolated-workspace"));
        assert_eq!(
            args.last().map(String::as_str),
            Some("Reply with exactly: READY TO WORK.")
        );
        assert!(!args.iter().any(|arg| arg.contains("mcp_servers")));
    }

    #[test]
    fn generated_connector_name_contains_two_words() {
        assert_eq!(random_connector_name().split_whitespace().count(), 2);
    }

    #[cfg(windows)]
    #[test]
    fn smoke_test_timeout_terminates_and_reaps_the_child_process() {
        use std::os::windows::process::CommandExt;

        let mut command = Command::new("cmd.exe");
        command
            .args(["/C", "ping -n 30 127.0.0.1 >NUL"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x08000000); // CREATE_NO_WINDOW
        let mut child = command.spawn().expect("start timeout test child");

        let error = wait_for_smoke_test_process(&mut child, Duration::from_secs(1))
            .expect_err("long-running child should time out");
        assert!(error.to_string().contains("within 1 seconds"));
        assert!(child.try_wait().expect("check child status").is_some());
    }
}

fn random_connector_name() -> String {
    const FIRST: &[&str] = &[
        "Amber", "Bright", "Calm", "Clear", "Cloud", "Coral", "Cosmic", "Dawn", "Golden", "Gentle",
        "Green", "Lunar", "Misty", "Silver", "Quiet", "Solar",
    ];
    const SECOND: &[&str] = &[
        "Badger", "Birch", "Cedar", "Falcon", "Finch", "Forest", "Fox", "Harbor", "Heron", "Maple",
        "Meadow", "Otter", "River", "Sparrow", "Willow", "Wolf",
    ];
    let id = uuid::Uuid::new_v4();
    let bytes = id.as_bytes();
    format!(
        "{} {}",
        FIRST[bytes[0] as usize % FIRST.len()],
        SECOND[bytes[1] as usize % SECOND.len()]
    )
}

pub(crate) fn connector_name() -> String {
    data_directory()
        .ok()
        .and_then(|path| fs::read_to_string(path.join("connector-name.txt")).ok())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Web2Codex".to_owned())
}

fn start_tunnel_runtime() -> anyhow::Result<()> {
    let (manager, scope, api_key_name, tunnel_id_name) = tunnel_secrets()?;
    let api_key = manager.get(&scope, &api_key_name)?.context(
        "OpenAI tunnel API key is not configured; open the chat and enter `/mcp tunnel`",
    )?;
    let tunnel_id = manager
        .get(&scope, &tunnel_id_name)?
        .context("OpenAI tunnel ID is not configured; open the chat and enter `/mcp tunnel`")?;
    let workspace = std::env::current_dir().context("failed to resolve the workspace directory")?;
    ensure_browser_started(true)?;
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
    anyhow::ensure!(
        std::io::stdin().read_line(&mut value)? != 0,
        "input closed; setup cancelled"
    );
    Ok(value.trim().to_string())
}

fn prompt_visible(prompt: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        std::io::stdin().is_terminal(),
        "tunnel setup requires an interactive terminal"
    );
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut value = String::new();
    std::io::stdin().read_line(&mut value)?;
    anyhow::ensure!(
        !value.trim().is_empty() && !value.chars().any(char::is_whitespace),
        "API key must be a non-empty value without whitespace"
    );
    Ok(value.trim().to_string())
}

fn has_saved_account_choice() -> bool {
    data_directory()
        .ok()
        .is_some_and(|directory| directory.join("selected-account-tier.json").is_file())
}

const BRIDGE_PROTOCOL: u32 = 7;

fn ensure_browser_started(session_owned: bool) -> anyhow::Result<()> {
    if send_control(ControlAction::Status).is_ok() {
        let state = read_state()?;
        if state.bridge_protocol == BRIDGE_PROTOCOL {
            return Ok(());
        }
        anyhow::bail!(
            "A different Web2Codex bridge version is running. Finish active sessions, run `web2codex web stop`, then launch again with matching CLI and WebView binaries."
        );
    }

    let executable = find_browser_executable().context(
        "the bundled Web2Codex browser was not found; reinstall Web2Codex or set WEB2CODEX_WEBVIEW_BIN",
    )?;
    let mut process = Command::new(&executable);
    if session_owned {
        process.arg("--session-owned");
    }
    process
        .arg("--background")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(target_os = "linux")]
    process.env(
        "WEBKIT_DISABLE_DMABUF_RENDERER",
        std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").unwrap_or_else(|| "1".into()),
    );
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
            anyhow::ensure!(
                read_state()?.bridge_protocol == BRIDGE_PROTOCOL,
                "The Web2Codex CLI and bundled WebView have different bridge versions; install them from the same build"
            );
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            anyhow::bail!(
                "Web2Codex browser exited during startup with {status}; check that a graphical session, native WebView runtime, and system tray host are available"
            );
        }
    }
    let _ = child.kill();
    let status = child.wait().ok();
    anyhow::bail!(
        "Web2Codex browser did not start{}; check that a graphical session, native WebView runtime, and system tray host are available",
        status.map_or_else(String::new, |status| format!(" ({status})")),
    )
}

fn send_control(action: ControlAction) -> anyhow::Result<()> {
    let state = read_state()?;
    send_control_to(&state, action)
}

fn send_control_to(state: &BrowserState, action: ControlAction) -> anyhow::Result<()> {
    let path = match action {
        ControlAction::Open => "/open",
        ControlAction::Login => "/login",
        ControlAction::SessionOpen => "/session/open",
        ControlAction::SessionLogin => "/session/login",
        ControlAction::Hide => "/hide",
        ControlAction::Stop => "/stop",
        ControlAction::Status => "/status",
    };
    let mut stream =
        std::net::TcpStream::connect_timeout(&state.address, Duration::from_millis(400))
            .with_context(|| format!("browser process is not reachable at {}", state.address))?;
    stream.set_read_timeout(Some(
        if matches!(
            action,
            ControlAction::Open
                | ControlAction::Login
                | ControlAction::SessionOpen
                | ControlAction::SessionLogin
                | ControlAction::Stop
        ) {
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
    let state: BrowserState =
        serde_json::from_str(&contents).context("Web2Codex browser state is invalid")?;
    anyhow::ensure!(
        state.address.ip().is_loopback()
            && !state.token.is_empty()
            && state.token.bytes().all(|b| b.is_ascii_graphic()),
        "Invalid local Web2Codex browser address or token"
    );
    Ok(state)
}

pub(crate) fn bridge_credentials() -> anyhow::Result<(SocketAddr, String)> {
    let state = read_state()?;
    anyhow::ensure!(
        state.address.ip().is_loopback()
            && !state.token.is_empty()
            && state.bridge_protocol == BRIDGE_PROTOCOL,
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
