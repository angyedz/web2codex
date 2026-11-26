//! Native, keyboard- and mouse-driven OpenAI MCP tunnel setup wizard.

use std::cell::Cell;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use tokio_util::sync::CancellationToken;

use codex_secrets::SecretName;
use codex_secrets::SecretScope;
use codex_secrets::SecretsBackendKind;
use codex_secrets::SecretsManager;
use codex_utils_home_dir::find_codex_home;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use crossterm::event::MouseButton;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use zeroize::Zeroize;

use crate::app_event::AppEvent;
use crate::app_event::McpTunnelSetupAction;
use crate::app_event::WebBrowserAction;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::CancellationEvent;
use crate::bottom_pane::ViewPresentation;
use crate::bottom_pane::bottom_pane_view::BottomPaneView;
use crate::render::renderable::Renderable;

const TUNNELS_URL: &str = "https://platform.openai.com/settings/organization/tunnels";
const API_KEYS_URL: &str = "https://platform.openai.com/settings/organization/api-keys";
const CONNECTORS_URL: &str = "https://chatgpt.com/#settings/Connectors";
const TUNNEL_ID_SECRET: &str = "OPENAI_TUNNEL_ID";
const TUNNEL_API_KEY_SECRET: &str = "OPENAI_TUNNEL_API_KEY";
const FIRST_RUN_SETUP_MARKER: &str = "first-run-native-mcp-setup-v1";
const LEGACY_FIRST_RUN_SETUP_MARKER: &str = "first-run-setup-v1";

static FIRST_RUN_SETUP_PROMPTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SetupPhase {
    SignIn,
    ChooseName,
    EnterName,
    ReuseCredentials,
    EnterTunnelId,
    EnterApiKey,
    StartingTunnel,
    AddConnector,
    SmokeTest,
    Complete,
}

pub(crate) struct McpTunnelSetupView {
    app_event_tx: AppEventSender,
    phase: SetupPhase,
    browser_opened: bool,
    connector_name: String,
    tunnel_id: String,
    input: String,
    api_key: String,
    saved_tunnel_id: Option<String>,
    credentials_saved: bool,
    status: Option<String>,
    smoke_test_cancellation: Option<CancellationToken>,
    complete: bool,
    rendered_area: Cell<Rect>,
}

impl McpTunnelSetupView {
    pub(crate) fn should_offer_first_run_setup() -> bool {
        let forced = std::env::var_os("WEB2CODEX_SETUP_ON_START").is_some();
        if !forced && first_run_setup_is_suppressed() {
            return false;
        }
        !FIRST_RUN_SETUP_PROMPTED.swap(true, Ordering::Relaxed)
    }

    pub(crate) fn new(app_event_tx: AppEventSender) -> Self {
        let connector_name = read_saved_connector_name().unwrap_or_default();
        let saved_tunnel_id = saved_tunnel_id_if_key_exists();
        let credentials_saved = saved_tunnel_id.is_some();
        Self {
            app_event_tx,
            phase: SetupPhase::SignIn,
            browser_opened: false,
            connector_name,
            tunnel_id: String::new(),
            input: String::new(),
            api_key: String::new(),
            saved_tunnel_id,
            credentials_saved,
            status: None,
            smoke_test_cancellation: None,
            complete: false,
            rendered_area: Cell::new(Rect::default()),
        }
    }

    fn open_chatgpt(&mut self) {
        self.app_event_tx
            .send(AppEvent::ManageWebBrowser(WebBrowserAction::Login));
        self.browser_opened = true;
        self.status = Some("ChatGPT opened in the persistent Web2Codex browser.".to_owned());
    }

    fn continue_after_sign_in(&mut self) {
        if self.connector_name.is_empty() {
            self.phase = SetupPhase::ChooseName;
        } else if self.saved_tunnel_id.is_some() {
            self.phase = SetupPhase::ReuseCredentials;
            self.tunnel_id = self.saved_tunnel_id.clone().unwrap_or_default();
        } else {
            self.phase = SetupPhase::EnterTunnelId;
        }
        self.status = None;
    }

    fn choose_generated_name(&mut self) {
        self.connector_name = random_connector_name();
        if self.persist_connector_name() {
            self.after_name_chosen();
        }
    }

    fn accept_custom_name(&mut self) {
        let name = self.input.trim().to_owned();
        if !is_valid_connector_name(&name) {
            self.status = Some("Use 1–64 letters, numbers, spaces, or hyphens.".to_owned());
            return;
        }
        self.connector_name = name;
        if self.persist_connector_name() {
            self.input.clear();
            self.after_name_chosen();
        }
    }

    fn after_name_chosen(&mut self) {
        if let Some(saved_tunnel_id) = self.saved_tunnel_id.clone() {
            self.tunnel_id = saved_tunnel_id;
            self.phase = SetupPhase::ReuseCredentials;
        } else {
            self.phase = SetupPhase::EnterTunnelId;
        }
        self.status = None;
    }

    fn accept_tunnel_id(&mut self) {
        let value = self.input.trim().to_owned();
        if !is_valid_tunnel_id(&value) {
            self.status =
                Some("Tunnel ID must start with `tunnel_` and contain the complete ID.".to_owned());
            return;
        }
        self.tunnel_id = value;
        self.input.clear();
        self.phase = SetupPhase::EnterApiKey;
        self.status = None;
    }

    fn save_and_start_tunnel(&mut self) {
        if self.api_key.trim().is_empty() {
            self.status = Some("Paste the runtime API key to continue.".to_owned());
            return;
        }
        if let Err(error) =
            store_tunnel_credentials(&self.connector_name, &self.tunnel_id, &self.api_key)
        {
            // Do not include the key or backend error text in TUI history.
            self.status = Some(format!(
                "Could not save credentials securely: {}",
                error.user_message()
            ));
            return;
        }
        self.api_key.zeroize();
        self.credentials_saved = true;
        self.phase = SetupPhase::StartingTunnel;
        self.status = Some("Starting the OpenAI Tunnel runtime…".to_owned());
        self.app_event_tx.send(AppEvent::StartMcpTunnelSetup);
    }

    fn open_page(&mut self, url: &'static str) {
        self.app_event_tx
            .send(AppEvent::OpenMcpSetupPage(url.to_owned()));
        self.status = Some("Opening the page in a Web2Codex browser tab…".to_owned());
    }

    fn start_tunnel(&mut self) {
        self.phase = SetupPhase::StartingTunnel;
        self.status = Some("Starting the OpenAI Tunnel runtime…".to_owned());
        self.app_event_tx.send(AppEvent::StartMcpTunnelSetup);
    }

    fn run_smoke_test(&mut self) {
        self.phase = SetupPhase::SmokeTest;
        self.status = Some("Testing ChatGPT Web with an exact plain-text reply…".to_owned());
        let cancellation = CancellationToken::new();
        self.smoke_test_cancellation = Some(cancellation.clone());
        self.app_event_tx
            .send(AppEvent::RunMcpTunnelSmokeTest { cancellation });
    }

    fn handle_setup_action_completed(&mut self, action: McpTunnelSetupAction, succeeded: bool) {
        let marker_path = first_run_setup_marker_path();
        self.handle_setup_action_completed_with_marker(action, succeeded, marker_path.as_deref());
    }

    fn handle_setup_action_completed_with_marker(
        &mut self,
        action: McpTunnelSetupAction,
        succeeded: bool,
        marker_path: Option<&Path>,
    ) {
        match action {
            McpTunnelSetupAction::OpenPlatformPage => {
                self.status = Some(if succeeded {
                    "The page is open in a new Web2Codex browser tab.".to_owned()
                } else {
                    "Could not open the page. Use the address shown above in the Web2Codex browser."
                        .to_owned()
                });
            }
            McpTunnelSetupAction::StartTunnel => {
                if succeeded {
                    self.phase = SetupPhase::AddConnector;
                    self.status = Some("Tunnel runtime is ready.".to_owned());
                } else {
                    self.phase = SetupPhase::AddConnector;
                    self.status = Some(
                        "Tunnel did not start. Press R to retry, or verify the saved ID and runtime key."
                            .to_owned(),
                    );
                }
            }
            McpTunnelSetupAction::SmokeTest => {
                self.smoke_test_cancellation = None;
                if succeeded {
                    if let Some(path) = marker_path {
                        let _ = write_first_run_setup_marker_at(path, "completed");
                    }
                    self.phase = SetupPhase::Complete;
                    self.status = Some(
                        "Smoke test passed: ChatGPT replied exactly `READY TO WORK.`".to_owned(),
                    );
                } else {
                    self.phase = SetupPhase::AddConnector;
                    self.status = Some(
                        "Smoke test failed or was incomplete. Check sign-in and connector setup, then run web2codex web tunnel smoke-test for details."
                            .to_owned(),
                    );
                }
            }
        }
    }

    fn handle_input_key(&mut self, key_event: KeyEvent) {
        match key_event.code {
            KeyCode::Esc => {
                match self.phase {
                    SetupPhase::EnterName => {
                        self.input.zeroize();
                        self.phase = SetupPhase::ChooseName;
                    }
                    SetupPhase::EnterTunnelId => {
                        self.input.zeroize();
                        self.phase = if self.saved_tunnel_id.is_some() {
                            SetupPhase::ReuseCredentials
                        } else {
                            SetupPhase::ChooseName
                        };
                    }
                    SetupPhase::EnterApiKey => {
                        self.api_key.zeroize();
                        self.input = self.tunnel_id.clone();
                        self.phase = SetupPhase::EnterTunnelId;
                    }
                    _ => {}
                }
                self.status = None;
            }
            KeyCode::Backspace => {
                if self.phase == SetupPhase::EnterApiKey {
                    self.api_key.pop();
                } else {
                    self.input.pop();
                }
                self.status = None;
            }
            KeyCode::Enter => match self.phase {
                SetupPhase::EnterName => self.accept_custom_name(),
                SetupPhase::EnterTunnelId => self.accept_tunnel_id(),
                SetupPhase::EnterApiKey => self.save_and_start_tunnel(),
                _ => {}
            },
            KeyCode::F(2) => match self.phase {
                SetupPhase::EnterTunnelId => self.open_page(TUNNELS_URL),
                SetupPhase::EnterApiKey => self.open_page(API_KEYS_URL),
                _ => {}
            },
            KeyCode::Char(ch)
                if !key_event
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && ch.is_ascii()
                    && !ch.is_control() =>
            {
                match self.phase {
                    SetupPhase::EnterName
                        if (ch.is_ascii_alphanumeric() || ch == ' ' || ch == '-')
                            && self.input.len() < 64 =>
                    {
                        self.input.push(ch);
                        self.status = None;
                    }
                    SetupPhase::EnterTunnelId
                        if (ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
                            && self.input.len() < 48 =>
                    {
                        self.input.push(ch);
                        self.status = None;
                    }
                    SetupPhase::EnterApiKey if !ch.is_whitespace() && self.api_key.len() < 512 => {
                        self.api_key.push(ch);
                        self.status = None;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn render_lines(&self) -> Vec<Line<'static>> {
        let mut lines = match self.phase {
            SetupPhase::SignIn => {
                let mut lines = vec![
                    Line::from("Web2Codex setup".bold()),
                    Line::from(""),
                    Line::from("Step 1 · Sign in to ChatGPT".bold()),
                    Line::from("Sign in inside the Web2Codex browser window."),
                    Line::from("Your login session stays saved in this browser profile."),
                    Line::from(""),
                ];
                if self.browser_opened {
                    lines.push(Line::from("Browser opened. Return here after sign-in."));
                    lines.push(Line::from(""));
                    lines.push(Line::from("Enter / C  Continue setup".bold()));
                    lines.push(Line::from("O  Reopen ChatGPT   ·   Esc  Cancel".dim()));
                } else {
                    lines.push(Line::from("Enter / O  Open ChatGPT".bold()));
                    lines.push(Line::from(
                        "S  Skip if already signed in   ·   Esc  Cancel".dim(),
                    ));
                }
                lines
            }
            SetupPhase::ChooseName => vec![
                Line::from("Web2Codex setup".bold()),
                Line::from(""),
                Line::from("Step 2 · Name your MCP connector".bold()),
                Line::from("Use the same name when you add the connector in ChatGPT."),
                Line::from("1  Generate a permanent two-word name".bold()),
                Line::from("2  Enter a name yourself"),
                Line::from(""),
                Line::from("Esc  Cancel".dim()),
            ],
            SetupPhase::EnterName => vec![
                Line::from("Web2Codex setup".bold()),
                Line::from(""),
                Line::from("Step 2 · Choose a connector name".bold()),
                Line::from("Name: ".to_owned() + &self.input),
                Line::from("Use 1–64 letters, numbers, spaces, or hyphens."),
                Line::from("Enter  Save name and continue".bold()),
                Line::from("Esc  Back".dim()),
            ],
            SetupPhase::ReuseCredentials => vec![
                Line::from("Web2Codex setup".bold()),
                Line::from(""),
                Line::from("Saved tunnel credentials found".bold()),
                Line::from(format!("Connector name: {}", self.connector_name)),
                Line::from(format!("Tunnel ID: {}", self.tunnel_id)),
                Line::from("Enter  Reuse saved credentials and start the tunnel".bold()),
                Line::from("R  Replace the tunnel ID and runtime API key"),
                Line::from("Esc  Back".dim()),
            ],
            SetupPhase::EnterTunnelId => vec![
                Line::from("Web2Codex setup".bold()),
                Line::from(""),
                Line::from("Step 3 · Create an OpenAI Tunnel".bold()),
                Line::from("Open the Tunnels page, create a tunnel, then copy its ID."),
                Line::from("F2 / click  Open the Tunnels page".bold()),
                Line::from(TUNNELS_URL),
                Line::from(""),
                Line::from(format!("Tunnel ID: {}", self.input)),
                Line::from("Paste the whole ID exactly as shown; it starts with tunnel_."),
                Line::from("Enter  Continue   ·   F2  Open page".bold()),
                Line::from("Esc  Back".dim()),
            ],
            SetupPhase::EnterApiKey => vec![
                Line::from("Web2Codex setup".bold()),
                Line::from(""),
                Line::from("Step 4 · Create a runtime API key".bold()),
                Line::from("Set expiration to Never and enable Tunnels Read + Use."),
                Line::from("F2 / click  Open the organization API Keys page".bold()),
                Line::from(API_KEYS_URL),
                Line::from("Do not create or paste an admin key."),
                Line::from(""),
                Line::from(format!("Runtime API key: {}", masked_secret(&self.api_key))),
                Line::from("Enter  Save securely and start the tunnel".bold()),
                Line::from("Esc  Back".dim()),
            ],
            SetupPhase::StartingTunnel => vec![
                Line::from("Web2Codex setup".bold()),
                Line::from(""),
                Line::from("Starting the OpenAI Tunnel runtime…".bold()),
                Line::from("Web2Codex keeps the tunnel running in the background."),
                Line::from(""),
                Line::from("Esc  Cancel this wizard".dim()),
            ],
            SetupPhase::AddConnector => vec![
                Line::from("Web2Codex setup".bold()),
                Line::from(""),
                Line::from("Step 5 · Add the connector in ChatGPT".bold()),
                Line::from("In ChatGPT, open Settings > Apps & Connectors > Add custom connector."),
                Line::from("Choose Connection: Tunnel, then enter the exact name and ID below."),
                Line::from(format!("Connector name: {}", self.connector_name)),
                Line::from(format!("Tunnel ID: {}", self.tunnel_id)),
                Line::from("Open Settings > Plugins > Cosmic Birch > Permissions."),
                Line::from(
                    "Select Allow all tools (High risk) to let the plugin run actions without prompts.",
                ),
                Line::from("Temporary Chats need Personalized mode."),
                Line::from(""),
                Line::from("O  Open connector settings".bold()),
                Line::from(CONNECTORS_URL),
                Line::from("T  Run exact smoke test".bold()),
                Line::from("Esc  Close setup".dim()),
            ],
            SetupPhase::SmokeTest => vec![
                Line::from("Web2Codex setup".bold()),
                Line::from(""),
                Line::from("Step 6 · Testing ChatGPT Web".bold()),
                Line::from("A temporary chat opens in the Setup · smoke test browser tab."),
                Line::from("Available model choices are checked in turn."),
                Line::from("No MCP schemas or JSON are sent into the chat."),
                Line::from(""),
                Line::from("Watch the browser tab while the checks run."),
                Line::from("Esc  Close setup".dim()),
            ],
            SetupPhase::Complete => vec![
                Line::from("Web2Codex setup".bold()),
                Line::from(""),
                Line::from("Setup complete".bold()),
                Line::from("ChatGPT replied exactly: READY TO WORK."),
                Line::from("Your tunnel is ready for Web2Codex MCP requests."),
                Line::from("You can send your first request in this chat now."),
                Line::from("Type /setup to change this later, or /web to open the browser."),
                Line::from(""),
                Line::from("Esc  Close setup".dim()),
            ],
        };
        if let Some(status) = &self.status {
            lines.push(Line::from(""));
            lines.push(Line::from(status.clone().yellow()));
        }
        lines
    }

    fn persist_connector_name(&mut self) -> bool {
        match browser_data_directory().and_then(|directory| {
            fs::create_dir_all(&directory)?;
            fs::write(directory.join("connector-name.txt"), &self.connector_name)
        }) {
            Ok(()) => true,
            Err(_) => {
                self.status = Some(
                    "Could not save the connector name. Check Web2Codex's local data folder."
                        .to_owned(),
                );
                false
            }
        }
    }

    fn advance_to_tunnel_id(&mut self) {
        self.phase = SetupPhase::EnterTunnelId;
        self.status = None;
    }

    fn cancel(&mut self) {
        let marker_path = first_run_setup_marker_path();
        self.cancel_with_marker_path(marker_path.as_deref());
    }

    fn cancel_with_marker_path(&mut self, marker_path: Option<&Path>) {
        if let Some(cancellation) = self.smoke_test_cancellation.take() {
            cancellation.cancel();
        }
        self.api_key.zeroize();
        self.input.zeroize();
        let _ = persist_setup_cancel_marker(self.credentials_saved, marker_path);
        self.complete = true;
    }
}

impl Drop for McpTunnelSetupView {
    fn drop(&mut self) {
        if let Some(cancellation) = self.smoke_test_cancellation.take() {
            cancellation.cancel();
        }
        self.api_key.zeroize();
        self.input.zeroize();
    }
}

impl BottomPaneView for McpTunnelSetupView {
    fn presentation(&self) -> ViewPresentation {
        ViewPresentation::Centered
    }

    fn prefer_esc_to_handle_key_event(&self) -> bool {
        true
    }

    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if !matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return;
        }
        if matches!(
            self.phase,
            SetupPhase::EnterName | SetupPhase::EnterTunnelId | SetupPhase::EnterApiKey
        ) {
            self.handle_input_key(key_event);
            return;
        }
        match self.phase {
            SetupPhase::SignIn => match key_event.code {
                KeyCode::Esc => self.cancel(),
                KeyCode::Char('o' | 'O') => self.open_chatgpt(),
                KeyCode::Enter if !self.browser_opened => self.open_chatgpt(),
                KeyCode::Enter | KeyCode::Char('c' | 'C') | KeyCode::Char('s' | 'S') => {
                    self.continue_after_sign_in();
                }
                _ => {}
            },
            SetupPhase::ChooseName => match key_event.code {
                KeyCode::Esc => self.cancel(),
                KeyCode::Enter | KeyCode::Char('1') => self.choose_generated_name(),
                KeyCode::Char('2') => {
                    self.input.clear();
                    self.phase = SetupPhase::EnterName;
                    self.status = None;
                }
                _ => {}
            },
            SetupPhase::ReuseCredentials => match key_event.code {
                KeyCode::Esc => self.phase = SetupPhase::SignIn,
                KeyCode::Enter => self.start_tunnel(),
                KeyCode::Char('r' | 'R') => {
                    self.input = self.tunnel_id.clone();
                    self.phase = SetupPhase::EnterTunnelId;
                    self.status = None;
                }
                _ => {}
            },
            SetupPhase::StartingTunnel => {
                if key_event.code == KeyCode::Esc {
                    self.cancel();
                }
            }
            SetupPhase::AddConnector => match key_event.code {
                KeyCode::Esc => self.cancel(),
                KeyCode::Char('o' | 'O') => self.open_page(CONNECTORS_URL),
                KeyCode::Char('t' | 'T') | KeyCode::Enter => self.run_smoke_test(),
                KeyCode::Char('r' | 'R') => self.start_tunnel(),
                _ => {}
            },
            SetupPhase::SmokeTest => {
                if key_event.code == KeyCode::Esc {
                    self.cancel();
                }
            }
            SetupPhase::Complete => {
                if key_event.code == KeyCode::Esc || key_event.code == KeyCode::Enter {
                    self.cancel();
                }
            }
            SetupPhase::EnterName | SetupPhase::EnterTunnelId | SetupPhase::EnterApiKey => {}
        }
    }

    fn handle_mouse_event(&mut self, mouse_event: MouseEvent) -> bool {
        let area = self.rendered_area.get();
        if mouse_event.kind != MouseEventKind::Down(MouseButton::Left)
            || mouse_event.column < area.left()
            || mouse_event.column >= area.right()
            || mouse_event.row < area.top()
            || mouse_event.row >= area.bottom()
        {
            return false;
        }
        let row = mouse_event.row.saturating_sub(area.y);
        match self.phase {
            SetupPhase::SignIn => {
                if !self.browser_opened && row == 6 {
                    self.open_chatgpt();
                } else if self.browser_opened && row == 8 {
                    self.continue_after_sign_in();
                } else if self.browser_opened && row == 9 {
                    self.open_chatgpt();
                }
            }
            SetupPhase::ChooseName => match row {
                4 => self.choose_generated_name(),
                5 => {
                    self.input.clear();
                    self.phase = SetupPhase::EnterName;
                }
                _ => {}
            },
            SetupPhase::EnterName if row == 5 => self.accept_custom_name(),
            SetupPhase::ReuseCredentials => match row {
                5 => self.start_tunnel(),
                6 => {
                    self.input = self.tunnel_id.clone();
                    self.phase = SetupPhase::EnterTunnelId;
                }
                _ => {}
            },
            SetupPhase::EnterTunnelId => match row {
                4 => self.open_page(TUNNELS_URL),
                9 => self.accept_tunnel_id(),
                _ => {}
            },
            SetupPhase::EnterApiKey => match row {
                4 => self.open_page(API_KEYS_URL),
                9 => self.save_and_start_tunnel(),
                _ => {}
            },
            SetupPhase::AddConnector => match row {
                8 => self.open_page(CONNECTORS_URL),
                10 => self.run_smoke_test(),
                _ => {}
            },
            SetupPhase::StartingTunnel if row == 5 => self.cancel(),
            SetupPhase::SmokeTest if row == 7 => self.cancel(),
            SetupPhase::Complete if row == 6 => self.cancel(),
            SetupPhase::EnterName
            | SetupPhase::StartingTunnel
            | SetupPhase::SmokeTest
            | SetupPhase::Complete => {}
        }
        true
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.cancel();
        CancellationEvent::Handled
    }

    fn handle_paste(&mut self, mut pasted: String) -> bool {
        let trimmed = pasted.trim();
        match self.phase {
            SetupPhase::EnterName => {
                for character in trimmed.chars() {
                    if self.input.len() >= 64 {
                        break;
                    }
                    if character.is_ascii_alphanumeric() || character == ' ' || character == '-' {
                        self.input.push(character);
                    }
                }
            }
            SetupPhase::EnterTunnelId => {
                for character in trimmed.chars() {
                    if self.input.len() >= 48 {
                        break;
                    }
                    if character.is_ascii_lowercase()
                        || character.is_ascii_digit()
                        || character == '_'
                    {
                        self.input.push(character);
                    }
                }
            }
            SetupPhase::EnterApiKey => {
                for character in trimmed.chars() {
                    if self.api_key.len() >= 512 {
                        break;
                    }
                    if character.is_ascii_graphic() {
                        self.api_key.push(character);
                    }
                }
            }
            _ => {
                pasted.zeroize();
                return false;
            }
        }
        // The paste buffer can contain the runtime API key. Erase it after copying
        // the filtered characters into the wizard's protected input field.
        pasted.zeroize();
        self.status = None;
        true
    }

    fn is_complete(&self) -> bool {
        self.complete
    }

    fn handle_mcp_tunnel_setup_action_completed(
        &mut self,
        action: McpTunnelSetupAction,
        succeeded: bool,
    ) -> bool {
        self.handle_setup_action_completed(action, succeeded);
        true
    }
}

impl Renderable for McpTunnelSetupView {
    fn desired_height(&self, _width: u16) -> u16 {
        self.render_lines().len().min(u16::MAX as usize) as u16
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.rendered_area.set(area);
        Paragraph::new(self.render_lines()).render(area, buf);
    }
}

fn read_saved_connector_name() -> Option<String> {
    fs::read_to_string(browser_data_directory().ok()?.join("connector-name.txt"))
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
}

fn first_run_setup_marker_path() -> Option<PathBuf> {
    Some(
        find_codex_home()
            .ok()?
            .to_path_buf()
            .join(FIRST_RUN_SETUP_MARKER),
    )
}

fn first_run_setup_is_suppressed() -> bool {
    let Some(marker_path) = first_run_setup_marker_path() else {
        return false;
    };
    if let Ok(marker) = fs::read_to_string(&marker_path) {
        return marker_suppresses_first_run_setup(&marker);
    }
    migrate_legacy_completed_marker(&marker_path)
}

fn migrate_legacy_completed_marker(marker_path: &Path) -> bool {
    if marker_path.exists() {
        return false;
    }
    let Some(legacy_path) = marker_path
        .parent()
        .map(|parent| parent.join(LEGACY_FIRST_RUN_SETUP_MARKER))
    else {
        return false;
    };
    let Ok(marker) = fs::read_to_string(legacy_path) else {
        return false;
    };
    if !legacy_marker_should_migrate(&marker) {
        return false;
    }
    let _ = write_first_run_setup_marker_at(marker_path, "completed");
    true
}

fn marker_suppresses_first_run_setup(marker: &str) -> bool {
    matches!(marker.trim(), "completed" | "skipped")
}

fn legacy_marker_should_migrate(marker: &str) -> bool {
    marker.trim() == "completed"
}

fn write_first_run_setup_marker_at(path: &Path, value: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, format!("{value}\n"))
}

fn persist_setup_cancel_marker(
    credentials_saved: bool,
    marker_path: Option<&Path>,
) -> std::io::Result<()> {
    if credentials_saved {
        return Ok(());
    }
    if let Some(path) = marker_path {
        write_first_run_setup_marker_at(path, "skipped")?;
    }
    Ok(())
}

fn saved_tunnel_id_if_key_exists() -> Option<String> {
    let codex_home = find_codex_home().ok()?;
    let manager = SecretsManager::new(codex_home.to_path_buf(), SecretsBackendKind::Local);
    let api_key_name = SecretName::new(TUNNEL_API_KEY_SECRET).ok()?;
    let tunnel_id_name = SecretName::new(TUNNEL_ID_SECRET).ok()?;
    let tunnel_id = manager.get(&SecretScope::Global, &tunnel_id_name).ok()??;
    let mut api_key = manager.get(&SecretScope::Global, &api_key_name).ok()??;
    api_key.zeroize();
    Some(tunnel_id)
}

fn store_tunnel_credentials(
    connector_name: &str,
    tunnel_id: &str,
    api_key: &str,
) -> Result<(), SetupSaveError> {
    let directory = browser_data_directory().map_err(|_| SetupSaveError::DATA_DIRECTORY)?;
    fs::create_dir_all(&directory).map_err(|_| SetupSaveError::DATA_DIRECTORY)?;
    fs::write(directory.join("connector-name.txt"), connector_name)
        .map_err(|_| SetupSaveError::DATA_DIRECTORY)?;

    let codex_home = find_codex_home().map_err(|_| SetupSaveError::SECRETS_STORE)?;
    let manager = SecretsManager::new(codex_home.to_path_buf(), SecretsBackendKind::Local);
    let api_key_name =
        SecretName::new(TUNNEL_API_KEY_SECRET).map_err(|_| SetupSaveError::SECRETS_STORE)?;
    let tunnel_id_name =
        SecretName::new(TUNNEL_ID_SECRET).map_err(|_| SetupSaveError::SECRETS_STORE)?;
    let scope = SecretScope::Global;
    let previous_tunnel_id = manager
        .get(&scope, &tunnel_id_name)
        .map_err(|_| SetupSaveError::SECRETS_STORE)?;
    manager
        .set(&scope, &tunnel_id_name, tunnel_id)
        .map_err(|_| SetupSaveError::SECRETS_STORE)?;
    if manager.set(&scope, &api_key_name, api_key).is_err() {
        if let Some(previous) = previous_tunnel_id {
            let _ = manager.set(&scope, &tunnel_id_name, &previous);
        } else {
            let _ = manager.delete(&scope, &tunnel_id_name);
        }
        return Err(SetupSaveError::SECRETS_STORE);
    }
    Ok(())
}

fn browser_data_directory() -> std::io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("WEB2CODEX_WEB_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .ok_or_else(|| std::io::Error::other("home directory unavailable"))?;
    #[cfg(windows)]
    {
        Ok(std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Local"))
            .join("Web2Codex/Browser"))
    }
    #[cfg(target_os = "macos")]
    {
        Ok(home.join("Library/Application Support/Web2Codex/Browser"))
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        Ok(std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("web2codex/browser"))
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

fn is_valid_connector_name(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == ' ' || character == '-'
        })
}

fn masked_secret(secret: &str) -> String {
    if secret.is_empty() {
        String::new()
    } else {
        "••••••••".to_owned()
    }
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

struct SetupSaveError {
    cause: SetupSaveErrorCause,
}

#[derive(Clone, Copy)]
enum SetupSaveErrorCause {
    DataDirectory,
    SecretsStore,
}

impl SetupSaveError {
    const DATA_DIRECTORY: Self = Self {
        cause: SetupSaveErrorCause::DataDirectory,
    };
    const SECRETS_STORE: Self = Self {
        cause: SetupSaveErrorCause::SecretsStore,
    };

    fn user_message(self) -> &'static str {
        match self.cause {
            SetupSaveErrorCause::DataDirectory => "check Web2Codex's local data folder",
            SetupSaveErrorCause::SecretsStore => "check the operating system's credential store",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_view(phase: SetupPhase) -> McpTunnelSetupView {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        McpTunnelSetupView {
            app_event_tx: AppEventSender::new(tx),
            phase,
            browser_opened: false,
            connector_name: String::new(),
            tunnel_id: String::new(),
            input: String::new(),
            api_key: String::new(),
            saved_tunnel_id: None,
            credentials_saved: false,
            status: None,
            smoke_test_cancellation: None,
            complete: false,
            rendered_area: Cell::new(Rect::default()),
        }
    }

    #[test]
    fn generated_connector_name_has_two_accepted_words() {
        let name = random_connector_name();
        assert_eq!(name.split_whitespace().count(), 2);
        assert!(is_valid_connector_name(&name));
    }

    #[test]
    fn connector_name_validation_rejects_empty_long_or_unsafe_values() {
        assert!(is_valid_connector_name("Cosmic Birch"));
        assert!(is_valid_connector_name("Web2-Codex 2"));
        assert!(!is_valid_connector_name(""));
        assert!(!is_valid_connector_name(&"x".repeat(65)));
        assert!(!is_valid_connector_name("name\nother"));
        assert!(!is_valid_connector_name("name/other"));
    }

    #[test]
    fn tunnel_id_validation_accepts_supported_shapes_only() {
        assert!(is_valid_tunnel_id(
            "tunnel_6afe13ca57a4819180e53756686fc15f"
        ));
        assert!(is_valid_tunnel_id(
            "tunnel_ab12_6afe13ca57a4819180e53756686fc15f"
        ));
        assert!(!is_valid_tunnel_id("tunnel_short"));
        assert!(!is_valid_tunnel_id(
            "tunnel_AB12_6afe13ca57a4819180e53756686fc15f"
        ));
        assert!(!is_valid_tunnel_id(
            "tunnel_ab123_6afe13ca57a4819180e53756686fc15f"
        ));
    }

    #[test]
    fn api_key_display_is_masked_and_does_not_include_the_secret() {
        let secret = "sk-runtime-secret-value";
        let display = masked_secret(secret);
        assert_eq!(display, "••••••••");
        assert!(!display.contains(secret));
        assert!(display.chars().all(|character| character == '•'));
        assert_eq!(masked_secret("x"), display);
        assert_eq!(masked_secret(&"x".repeat(512)), display);
    }

    #[test]
    fn pasted_runtime_key_is_masked_in_the_setup_view() {
        let secret = "sk-runtime-secret-value";
        let mut view = test_view(SetupPhase::EnterApiKey);
        assert!(view.handle_paste(secret.to_owned()));
        assert_eq!(view.api_key, secret);
        let display = view
            .render_lines()
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect::<String>();
        assert!(!display.contains(secret));
        assert!(display.contains(&masked_secret(secret)));
    }

    #[test]
    fn connector_setup_explains_plugin_action_permissions() {
        let view = test_view(SetupPhase::AddConnector);
        let display = view
            .render_lines()
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect::<String>();

        assert!(display.contains("Settings > Plugins > Cosmic Birch > Permissions"));
        assert!(display.contains("Allow all tools (High risk)"));
        assert!(display.contains("run actions without prompts"));
    }

    #[test]
    fn cancelling_first_run_persists_skipped_marker_only_without_saved_credentials() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let marker_path = directory.path().join(FIRST_RUN_SETUP_MARKER);

        let mut first_run_view = test_view(SetupPhase::SignIn);
        first_run_view.cancel_with_marker_path(Some(&marker_path));
        assert!(first_run_view.is_complete());
        assert_eq!(
            fs::read_to_string(&marker_path).expect("cancel should persist marker"),
            "skipped\n"
        );

        let mut configured_view = test_view(SetupPhase::AddConnector);
        configured_view.credentials_saved = true;
        configured_view.cancel_with_marker_path(Some(&marker_path));
        assert_eq!(
            fs::read_to_string(&marker_path).expect("existing marker should remain"),
            "skipped\n"
        );
    }

    #[test]
    fn native_setup_marker_is_distinct_from_the_legacy_cli_marker() {
        assert_ne!(FIRST_RUN_SETUP_MARKER, "first-run-setup-v1");
    }

    #[test]
    fn legacy_completed_setup_migrates_but_legacy_skip_offers_the_native_wizard() {
        let completed_directory =
            tempfile::tempdir().expect("temporary directory should be created");
        let completed_marker = completed_directory.path().join(FIRST_RUN_SETUP_MARKER);
        fs::write(
            completed_directory
                .path()
                .join(LEGACY_FIRST_RUN_SETUP_MARKER),
            "completed\n",
        )
        .expect("legacy completion should be created");
        assert!(migrate_legacy_completed_marker(&completed_marker));
        assert_eq!(
            fs::read_to_string(&completed_marker).expect("completion should migrate"),
            "completed\n"
        );

        let skipped_directory = tempfile::tempdir().expect("temporary directory should be created");
        let skipped_marker = skipped_directory.path().join(FIRST_RUN_SETUP_MARKER);
        fs::write(
            skipped_directory.path().join(LEGACY_FIRST_RUN_SETUP_MARKER),
            "skipped\n",
        )
        .expect("legacy skip should be created");
        assert!(!migrate_legacy_completed_marker(&skipped_marker));
        assert!(!skipped_marker.exists());
    }

    #[test]
    fn only_explicitly_completed_or_skipped_setup_markers_suppress_first_run() {
        assert!(marker_suppresses_first_run_setup("completed\n"));
        assert!(marker_suppresses_first_run_setup("skipped\n"));
        assert!(!marker_suppresses_first_run_setup(""));
        assert!(!marker_suppresses_first_run_setup("in_progress\n"));
    }

    #[test]
    fn escape_returns_to_the_previous_setup_input_step() {
        let mut api_key_view = test_view(SetupPhase::EnterApiKey);
        api_key_view.tunnel_id = "tunnel_ab12_6afe13ca57a4819180e53756686fc15f".to_owned();
        api_key_view.api_key = "secret-key".to_owned();
        api_key_view.handle_input_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(api_key_view.phase, SetupPhase::EnterTunnelId);
        assert_eq!(api_key_view.input, api_key_view.tunnel_id);
        assert!(api_key_view.api_key.is_empty());

        let mut tunnel_id_view = test_view(SetupPhase::EnterTunnelId);
        tunnel_id_view.input = "tunnel_partial".to_owned();
        tunnel_id_view.handle_input_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(tunnel_id_view.phase, SetupPhase::ChooseName);
        assert!(tunnel_id_view.input.is_empty());

        let mut name_view = test_view(SetupPhase::EnterName);
        name_view.input = "Cosmic Birch".to_owned();
        name_view.handle_input_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(name_view.phase, SetupPhase::ChooseName);
        assert!(name_view.input.is_empty());

        let mut replacement_view = test_view(SetupPhase::EnterTunnelId);
        replacement_view.saved_tunnel_id = Some("tunnel_original".to_owned());
        replacement_view.handle_input_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(replacement_view.phase, SetupPhase::ReuseCredentials);
    }

    #[test]
    fn tunnel_and_smoke_results_advance_the_wizard_without_output_payloads() {
        let mut view = test_view(SetupPhase::StartingTunnel);
        view.handle_setup_action_completed_with_marker(
            McpTunnelSetupAction::StartTunnel,
            true,
            None,
        );
        assert_eq!(view.phase, SetupPhase::AddConnector);
        assert_eq!(view.status.as_deref(), Some("Tunnel runtime is ready."));

        view.handle_setup_action_completed_with_marker(McpTunnelSetupAction::SmokeTest, true, None);
        assert_eq!(view.phase, SetupPhase::Complete);
        assert!(view.status.as_deref().unwrap().contains("READY TO WORK."));
    }

    #[test]
    fn only_a_successful_smoke_test_marks_setup_complete() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let marker_path = directory.path().join(FIRST_RUN_SETUP_MARKER);

        let mut view = test_view(SetupPhase::StartingTunnel);
        view.handle_setup_action_completed_with_marker(
            McpTunnelSetupAction::StartTunnel,
            true,
            Some(&marker_path),
        );
        assert!(!marker_path.exists());

        view.handle_setup_action_completed_with_marker(
            McpTunnelSetupAction::SmokeTest,
            false,
            Some(&marker_path),
        );
        assert!(!marker_path.exists());

        view.handle_setup_action_completed_with_marker(
            McpTunnelSetupAction::SmokeTest,
            true,
            Some(&marker_path),
        );
        assert_eq!(
            fs::read_to_string(marker_path).expect("successful smoke test writes marker"),
            "completed\n"
        );
    }

    #[test]
    fn cancelling_the_setup_view_cancels_its_smoke_test() {
        let mut view = test_view(SetupPhase::AddConnector);
        view.run_smoke_test();
        let cancellation = view
            .smoke_test_cancellation
            .as_ref()
            .expect("smoke test cancellation token")
            .clone();

        view.cancel_with_marker_path(None);

        assert!(cancellation.is_cancelled());
    }
}
