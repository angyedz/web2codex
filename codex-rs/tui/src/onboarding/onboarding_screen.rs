//! Onboarding screen orchestration and top-level keyboard routing.
//!
//! The onboarding flow presents project trust prompts and folder-entry consent.

use codex_app_server_client::AppServerEvent;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ConfigBatchWriteParams;
use codex_app_server_protocol::RequestId;
use codex_exec_server::LOCAL_FS;
use codex_git_utils::resolve_root_git_project_for_trust;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::prelude::Widget;
use ratatui::style::Color;
use ratatui::widgets::Clear;
use ratatui::widgets::WidgetRef;

use crate::app_server_session::AppServerSession;
use crate::config_update::RemoteProjectTrust;
use crate::config_update::format_config_error;
use crate::config_update::replace_config_value;
use crate::config_update::write_trusted_project;
use crate::empty_state_animation::Presentation;
use crate::key_hint::KeyBindingListExt;
use crate::legacy_core::config::Config;
use crate::onboarding::keys;
use crate::onboarding::trust_directory::TrustDirectorySelection;
use crate::onboarding::trust_directory::TrustDirectoryWidget;
use crate::onboarding::welcome::WelcomeWidget;
use crate::startup_draft::StartupCancelled;
use crate::tui::FrameRequester;
use crate::tui::MouseCapture;
use crate::tui::Tui;
use crate::tui::TuiEvent;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;
use uuid::Uuid;

#[path = "directory_trust.rs"]
mod directory_trust;
pub(crate) use directory_trust::DirectoryTrustOptions;
pub(crate) use directory_trust::check_directory_trust;

#[allow(clippy::large_enum_variant)]
enum Step {
    Welcome(WelcomeWidget),
    TrustDirectory(TrustDirectoryWidget),
}

pub(crate) trait KeyboardHandler {
    fn handle_key_event(&mut self, key_event: KeyEvent);
    fn handle_paste(&mut self, _pasted: String) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepState {
    Hidden,
    InProgress,
    Complete,
}

pub(crate) trait StepStateProvider {
    fn get_step_state(&self) -> StepState;
}

pub(crate) struct OnboardingScreen {
    request_frame: FrameRequester,
    steps: Vec<Step>,
    remote_trust_key: Option<String>,
    is_done: bool,
    should_exit: bool,
}

pub(crate) struct OnboardingScreenArgs {
    pub show_trust_screen: bool,
    pub remote_project_trust: Option<RemoteProjectTrust>,
    pub app_server_request_handle: Option<AppServerRequestHandle>,
    pub config: Config,
}

#[derive(Default)]
pub(crate) struct OnboardingResult {
    pub directory_trust_persisted: bool,
    pub should_exit: bool,
}

impl OnboardingScreen {
    pub(crate) async fn new(tui: &mut Tui, args: OnboardingScreenArgs) -> Self {
        let OnboardingScreenArgs {
            show_trust_screen,
            remote_project_trust,
            app_server_request_handle,
            config,
        } = args;
        let local_settings = crate::local_settings::LocalSettings::from(&config);
        let cwd = config.cwd.to_path_buf();
        let remote_trust_key = remote_project_trust
            .as_ref()
            .map(|project| project.trust_target.to_string_lossy().into_owned());
        let mut steps: Vec<Step> = Vec::new();
        steps.push(Step::Welcome(WelcomeWidget::new(
            false,
            tui.frame_requester(),
            local_settings.tui.animations && local_settings.tui.effects.welcome,
        )));
        #[cfg(target_os = "windows")]
        let show_windows_create_sandbox_hint = if show_trust_screen
            && remote_project_trust.is_none()
            && let Some(handle) = &app_server_request_handle
        {
            crate::windows_sandbox::WindowsSandboxConfig::read(
                handle.clone(),
                config.cwd.display().to_string(),
            )
            .await
            .is_ok_and(|state| !state.is_enabled())
        } else {
            false
        };
        #[cfg(not(target_os = "windows"))]
        let show_windows_create_sandbox_hint = false;
        let highlighted = TrustDirectorySelection::Trust;
        if show_trust_screen {
            let (cwd, trust_target) = match remote_project_trust {
                Some(RemoteProjectTrust {
                    cwd, trust_target, ..
                }) => (cwd, trust_target),
                None => {
                    let trust_target =
                        resolve_root_git_project_for_trust(LOCAL_FS.as_ref(), &config.cwd)
                            .await
                            .map(Into::into)
                            .unwrap_or_else(|| cwd.clone());
                    (cwd, trust_target)
                }
            };
            steps.push(Step::TrustDirectory(TrustDirectoryWidget {
                restricted: false,
                existing_task: false,
                cancel: super::trust_directory::TrustCancelAction::Quit,
                cwd,
                trust_target,
                show_windows_create_sandbox_hint,
                should_quit: false,
                selection: None,
                highlighted,
                error: None,
            }))
        }
        Self {
            request_frame: tui.frame_requester(),
            steps,
            remote_trust_key,
            is_done: false,
            should_exit: false,
        }
    }

    fn current_steps_mut(&mut self) -> Vec<&mut Step> {
        let mut out: Vec<&mut Step> = Vec::new();
        for step in self.steps.iter_mut() {
            match step.get_step_state() {
                StepState::Hidden => continue,
                StepState::Complete => out.push(step),
                StepState::InProgress => {
                    out.push(step);
                    break;
                }
            }
        }
        out
    }

    fn current_steps(&self) -> Vec<&Step> {
        let mut out: Vec<&Step> = Vec::new();
        for step in self.steps.iter() {
            match step.get_step_state() {
                StepState::Hidden => continue,
                StepState::Complete => out.push(step),
                StepState::InProgress => {
                    out.push(step);
                    break;
                }
            }
        }
        out
    }

    pub(crate) fn is_done(&self) -> bool {
        self.is_done
            || !self
                .steps
                .iter()
                .any(|step| matches!(step.get_step_state(), StepState::InProgress))
    }

    pub fn should_exit(&self) -> bool {
        self.should_exit
    }

    /// Check whether the first unfinished onboarding step can grant directory trust.
    fn is_trust_step_active(&self) -> bool {
        self.steps
            .iter()
            .find(|step| matches!(step.get_step_state(), StepState::InProgress))
            .is_some_and(|step| matches!(step, Step::TrustDirectory(_)))
    }
}
impl KeyboardHandler for OnboardingScreen {
    /// Route key events to onboarding steps.
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        let should_quit = key_event.kind == KeyEventKind::Press && keys::QUIT.is_pressed(key_event);
        if should_quit {
            if self.is_trust_step_active() {
                self.should_exit = true;
            }
            self.is_done = true;
        } else {
            if let Some(Step::Welcome(widget)) = self
                .steps
                .iter_mut()
                .find(|step| matches!(step, Step::Welcome(_)))
            {
                widget.handle_key_event(key_event);
            }
            if let Some(active_step) = self.current_steps_mut().into_iter().last() {
                active_step.handle_key_event(key_event);
            }
            if self.steps.iter().any(|step| {
                if let Step::TrustDirectory(widget) = step {
                    widget.should_quit()
                } else {
                    false
                }
            }) {
                self.should_exit = true;
                self.is_done = true;
            }
        }
        self.request_frame.schedule_frame();
    }

    fn handle_paste(&mut self, pasted: String) {
        if pasted.is_empty() {
            return;
        }

        if let Some(active_step) = self.current_steps_mut().into_iter().last() {
            active_step.handle_paste(pasted);
        }
        self.request_frame.schedule_frame();
    }
}

impl WidgetRef for &OnboardingScreen {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        let logo_presentation = Presentation::Animated;
        for step in self.current_steps() {
            match step {
                Step::Welcome(widget) => widget.set_presentation(logo_presentation),
                Step::TrustDirectory(_) => {}
            }
        }

        Clear.render(area, buf);
        // Render steps top-to-bottom, measuring each step's height dynamically.
        let mut y = area.y;
        let bottom = area.y.saturating_add(area.height);
        let width = area.width;

        // Helper to scan a temporary buffer and return number of used rows.
        fn used_rows(tmp: &Buffer, width: u16, height: u16) -> u16 {
            if width == 0 || height == 0 {
                return 0;
            }
            let mut last_non_empty: Option<u16> = None;
            for yy in 0..height {
                let mut any = false;
                for xx in 0..width {
                    let cell = &tmp[(xx, yy)];
                    let has_symbol = !cell.symbol().trim().is_empty();
                    let has_style = cell.fg != Color::Reset
                        || cell.bg != Color::Reset
                        || !cell.modifier.is_empty();
                    if has_symbol || has_style {
                        any = true;
                        break;
                    }
                }
                if any {
                    last_non_empty = Some(yy);
                }
            }
            last_non_empty.map(|v| v + 2).unwrap_or(0)
        }

        let mut i = 0usize;
        let current_steps = self.current_steps();

        while i < current_steps.len() && y < bottom {
            let step = &current_steps[i];
            let max_h = bottom.saturating_sub(y);
            if max_h == 0 || width == 0 {
                break;
            }
            let scratch_area = Rect::new(0, 0, width, max_h);
            let mut scratch = Buffer::empty(scratch_area);
            if let Step::Welcome(widget) = step {
                widget.update_layout_area(scratch_area);
            }
            step.render_ref(scratch_area, &mut scratch);
            let h = used_rows(&scratch, width, max_h).min(max_h);
            if h > 0 {
                let target = Rect {
                    x: area.x,
                    y,
                    width,
                    height: h,
                };
                Clear.render(target, buf);
                step.render_ref(target, buf);
                y = y.saturating_add(h);
            }
            i += 1;
        }
    }
}

impl KeyboardHandler for Step {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match self {
            Step::Welcome(widget) => widget.handle_key_event(key_event),
            Step::TrustDirectory(widget) => widget.handle_key_event(key_event),
        }
    }

    fn handle_paste(&mut self, pasted: String) {
        match self {
            Step::Welcome(_) => {}
            Step::TrustDirectory(widget) => widget.handle_paste(pasted),
        }
    }
}

impl StepStateProvider for Step {
    fn get_step_state(&self) -> StepState {
        match self {
            Step::Welcome(w) => w.get_step_state(),
            Step::TrustDirectory(w) => w.get_step_state(),
        }
    }
}

impl WidgetRef for Step {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        match self {
            Step::Welcome(widget) => {
                widget.render_ref(area, buf);
            }
            Step::TrustDirectory(widget) => {
                widget.render_ref(area, buf);
            }
        }
    }
}

pub(crate) async fn run_onboarding_app(
    args: OnboardingScreenArgs,
    app_server: Option<&mut AppServerSession>,
    tui: &mut Tui,
) -> Result<OnboardingResult> {
    let request_handle = args.app_server_request_handle.clone();
    let screen = OnboardingScreen::new(tui, args).await;
    run_onboarding_screen(screen, request_handle, app_server, tui).await
}

pub(super) async fn run_onboarding_screen(
    onboarding_screen: OnboardingScreen,
    app_server_request_handle: Option<AppServerRequestHandle>,
    app_server: Option<&mut AppServerSession>,
    tui: &mut Tui,
) -> Result<OnboardingResult> {
    // Keep project trust prompts readable and selectable in fullscreen mode.
    let previous_mouse_capture = tui.mouse_capture;
    let result = match tui.set_mouse_capture(MouseCapture::Disabled) {
        Ok(()) => {
            run_onboarding_screen_inner(
                onboarding_screen,
                app_server_request_handle,
                app_server,
                tui,
            )
            .await
        }
        Err(error) => Err(error.into()),
    };
    let restore = tui.set_mouse_capture(previous_mouse_capture);
    result.and_then(|result| restore.map(|()| result).map_err(Into::into))
}

async fn run_onboarding_screen_inner(
    mut onboarding_screen: OnboardingScreen,
    app_server_request_handle: Option<AppServerRequestHandle>,
    mut app_server: Option<&mut AppServerSession>,
    tui: &mut Tui,
) -> Result<OnboardingResult> {
    use tokio_stream::StreamExt;
    let mut directory_trust_persisted = false;

    tui.draw(u16::MAX, |frame| {
        frame.render_widget_ref(&onboarding_screen, frame.area());
    })?;

    tui.discard_pending_input_before_interactive_screen()?;
    let tui_events = tui.event_stream();
    tokio::pin!(tui_events);
    let mut trust_step_was_active = onboarding_screen.is_trust_step_active();

    while !onboarding_screen.is_done() {
        tokio::select! {
            event = tui_events.next() => {
                if let Some(event) = event {
                    tui.screen_size_for_event(&event)?;
                    match event {
                        TuiEvent::Key(key_event) => {
                            onboarding_screen.handle_key_event(key_event);
                            if !directory_trust_persisted {
                                directory_trust_persisted = persist_selected_trust(
                                    &mut onboarding_screen,
                                    app_server_request_handle.clone(),
                                )
                                .await;
                            }
                        }
                        TuiEvent::Paste(text) => {
                            onboarding_screen.handle_paste(text);
                        }
                        TuiEvent::Draw
                        | TuiEvent::Resume
                        | TuiEvent::Resize(_)
                        | TuiEvent::FocusGained
                        | TuiEvent::FocusLost => {
                            for step in &onboarding_screen.steps {
                                if let Step::Welcome(widget) = step {
                                    if matches!(&event, TuiEvent::Resume) {
                                        widget.set_presentation(crate::empty_state_animation::Presentation::Hidden);
                                    }
                                    widget.set_focused(tui.is_terminal_focused());
                                }
                            }
                            tui.draw(u16::MAX, |frame| {
                                frame.render_widget_ref(&onboarding_screen, frame.area());
                            })?;
                        }
                        TuiEvent::Mouse(_) => {}
                    }
                } else {
                    return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, StartupCancelled).into());
                }
            }
            event = async {
                match app_server.as_mut() {
                    Some(app_server) => app_server.next_event().await,
                    None => None,
                }
            }, if app_server.is_some() => {
                let Some(event) = event else {
                    return Err(color_eyre::eyre::eyre!(super::SETUP_CONNECTION_LOST));
                };
                match event.event {
                    AppServerEvent::ServerNotification(_) => {}
                    AppServerEvent::Disconnected { message } => {
                        return Err(color_eyre::eyre::eyre!(message));
                    }
                    AppServerEvent::Lagged { .. }
                    | AppServerEvent::ServerRequest(_) => {}
                }
            }
        }

        discard_pending_input_on_trust_step_transition(
            &onboarding_screen,
            &mut trust_step_was_active,
            |onboarding_screen| {
                tui.draw(u16::MAX, |frame| {
                    frame.render_widget_ref(onboarding_screen, frame.area());
                })?;
                tui.discard_pending_input_before_interactive_screen()?;
                Ok(())
            },
        )?;
    }
    Ok(OnboardingResult {
        directory_trust_persisted,
        should_exit: onboarding_screen.should_exit(),
    })
}

/// Render a newly activated trust step before discarding keys from the preceding screen.
///
/// Once the step is active, preserve fresh navigation and confirmation input.
fn discard_pending_input_on_trust_step_transition(
    onboarding_screen: &OnboardingScreen,
    trust_step_was_active: &mut bool,
    render_and_discard: impl FnOnce(&OnboardingScreen) -> Result<()>,
) -> Result<()> {
    let trust_step_is_active = onboarding_screen.is_trust_step_active();
    if trust_step_is_active && !*trust_step_was_active {
        render_and_discard(onboarding_screen)?;
    }
    *trust_step_was_active = trust_step_is_active;
    Ok(())
}

async fn persist_selected_trust(
    onboarding_screen: &mut OnboardingScreen,
    request_handle: Option<AppServerRequestHandle>,
) -> bool {
    let Some((trust_step_index, trust_target)) = onboarding_screen
        .steps
        .iter()
        .enumerate()
        .find_map(|(index, step)| {
            if let Step::TrustDirectory(widget) = step
                && widget.selection == Some(TrustDirectorySelection::Trust)
                && !widget.restricted
            {
                return Some((index, widget.trust_target.clone()));
            }
            None
        })
    else {
        return false;
    };

    let result = match (
        request_handle,
        onboarding_screen.remote_trust_key.as_deref(),
    ) {
        (Some(request_handle), Some(project_key)) => {
            let project_key = project_key.replace('\\', "\\\\").replace('"', "\\\"");
            request_handle
                .request_typed::<serde_json::Value>(ClientRequest::ConfigBatchWrite {
                    request_id: RequestId::String(format!(
                        "tui-project-trust-write-{}",
                        Uuid::new_v4()
                    )),
                    params: ConfigBatchWriteParams {
                        edits: vec![replace_config_value(
                            format!("projects.\"{project_key}\".trust_level"),
                            serde_json::json!("trusted"),
                        )],
                        file_path: None,
                        expected_version: None,
                        reload_user_config: true,
                    },
                })
                .await
                .map(|_| ())
                .wrap_err("config/batchWrite failed while persisting remote project trust")
        }
        (Some(request_handle), None) => write_trusted_project(request_handle, &trust_target)
            .await
            .map(|_| ()),
        (None, _) => Err(color_eyre::eyre::eyre!("app server unavailable")),
    };

    match result {
        Ok(()) => true,
        Err(error) => {
            let error = format_config_error(&error);
            tracing::error!(
                "failed to persist trusted project state for {}: {error}",
                trust_target.display()
            );
            if let Step::TrustDirectory(widget) = &mut onboarding_screen.steps[trust_step_index] {
                widget.selection = None;
                widget.error = Some(format!(
                    "Failed to set trust for {}: {error}",
                    trust_target.display()
                ));
            }
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::OnboardingScreen;
    use super::Step;
    use super::StepStateProvider;
    use super::discard_pending_input_on_trust_step_transition;
    use super::persist_selected_trust;
    use crate::onboarding::onboarding_screen::KeyboardHandler;
    use crate::onboarding::trust_directory::TrustDirectorySelection;
    use crate::onboarding::trust_directory::TrustDirectoryWidget;
    use crate::tui::FrameRequester;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;

    #[test]
    fn buffered_enter_is_discarded_when_trust_step_becomes_active() {
        let mut onboarding_screen = OnboardingScreen {
            request_frame: FrameRequester::test_dummy(),
            steps: vec![Step::TrustDirectory(TrustDirectoryWidget {
                restricted: false,
                existing_task: false,
                cancel: super::super::trust_directory::TrustCancelAction::Quit,
                cwd: PathBuf::from("/workspace/project"),
                trust_target: PathBuf::from("/workspace/project"),
                show_windows_create_sandbox_hint: false,
                should_quit: false,
                selection: None,
                highlighted: TrustDirectorySelection::Trust,
                error: None,
            })],
            remote_trust_key: None,
            is_done: false,
            should_exit: false,
        };

        let mut buffered_keys = vec![KeyEvent::from(KeyCode::Enter)];
        let mut trust_step_was_active = false;
        let mut drain_count = 0;
        discard_pending_input_on_trust_step_transition(
            &onboarding_screen,
            &mut trust_step_was_active,
            |_| {
                drain_count += 1;
                buffered_keys.clear();
                Ok(())
            },
        )
        .expect("discard buffered trust input");
        for key in buffered_keys {
            onboarding_screen.handle_key_event(key);
        }

        assert!(onboarding_screen.is_trust_step_active());
        let Step::TrustDirectory(widget) = &onboarding_screen.steps[0] else {
            panic!("trust step should remain present");
        };
        assert_eq!(widget.selection, None);
        assert_eq!(widget.highlighted, TrustDirectorySelection::Trust);

        let mut navigation_keys = vec![KeyEvent::from(KeyCode::Down)];
        discard_pending_input_on_trust_step_transition(
            &onboarding_screen,
            &mut trust_step_was_active,
            |_| {
                drain_count += 1;
                navigation_keys.clear();
                Ok(())
            },
        )
        .expect("retain fresh trust navigation");
        assert_eq!(drain_count, 1);
        onboarding_screen.handle_key_event(navigation_keys.remove(0));
        let Step::TrustDirectory(widget) = &onboarding_screen.steps[0] else {
            panic!("trust step should remain present");
        };
        assert_eq!(widget.highlighted, TrustDirectorySelection::Quit);
        onboarding_screen.handle_key_event(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(onboarding_screen.should_exit());
    }

    #[tokio::test]
    async fn restricted_acceptance_and_cancellation_do_not_persist_trust() {
        for key in [KeyCode::Enter, KeyCode::Esc] {
            let mut screen = OnboardingScreen {
                request_frame: FrameRequester::test_dummy(),
                steps: vec![Step::TrustDirectory(TrustDirectoryWidget {
                    restricted: true,
                    existing_task: false,
                    cancel: super::super::trust_directory::TrustCancelAction::AgentsOverview,
                    cwd: PathBuf::from("/workspace/project"),
                    trust_target: PathBuf::from("/workspace/project"),
                    show_windows_create_sandbox_hint: false,
                    should_quit: false,
                    selection: None,
                    highlighted: TrustDirectorySelection::Trust,
                    error: None,
                })],
                remote_trust_key: Some("/workspace/project".to_string()),
                is_done: false,
                should_exit: false,
            };
            screen.handle_key_event(key.into());
            assert!(!persist_selected_trust(&mut screen, /*request_handle*/ None).await);
            assert!(screen.is_done());
            assert_eq!(screen.should_exit(), key == KeyCode::Esc);
            let Step::TrustDirectory(widget) = &screen.steps[0] else {
                panic!("trust step")
            };
            assert_eq!(widget.error, None);
        }
    }

    #[tokio::test]
    async fn trust_persistence_failure_keeps_trust_step_in_progress() {
        let mut onboarding_screen = OnboardingScreen {
            request_frame: FrameRequester::test_dummy(),
            steps: vec![Step::TrustDirectory(TrustDirectoryWidget {
                restricted: false,
                existing_task: false,
                cancel: super::super::trust_directory::TrustCancelAction::Quit,
                cwd: PathBuf::from("/workspace/project"),
                trust_target: PathBuf::from("/workspace/project"),
                show_windows_create_sandbox_hint: false,
                should_quit: false,
                selection: Some(TrustDirectorySelection::Trust),
                highlighted: TrustDirectorySelection::Trust,
                error: None,
            })],
            remote_trust_key: None,
            is_done: false,
            should_exit: false,
        };

        let persisted =
            persist_selected_trust(&mut onboarding_screen, /*request_handle*/ None).await;

        assert!(!persisted);
        let Step::TrustDirectory(widget) = &onboarding_screen.steps[0] else {
            panic!("trust step should remain present");
        };
        assert_eq!(widget.selection, None);
        assert_eq!(widget.get_step_state(), super::StepState::InProgress);
        assert!(
            widget
                .error
                .as_deref()
                .is_some_and(|error| error.contains("app server unavailable"))
        );
    }
}
