use std::cell::Cell;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::MouseButton;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Borders;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::ViewCompletion;
use crate::bottom_pane::ViewPresentation;
use crate::keymap::KeymapContext;
use crate::keymap::KeymapContextSet;
use crate::render::renderable::Renderable;

use super::ChatWidget;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ContextWindowProfile {
    pub(super) label: &'static str,
    pub(super) context_window: i64,
}

const CONTEXT_WINDOW_PROFILES: [ContextWindowProfile; 4] = [
    ContextWindowProfile {
        label: "Small",
        context_window: 80_000,
    },
    ContextWindowProfile {
        label: "Normal",
        context_window: 200_000,
    },
    ContextWindowProfile {
        label: "Above normal",
        context_window: 270_000,
    },
    ContextWindowProfile {
        label: "Unstable",
        context_window: 400_000,
    },
];

pub(super) fn parse_context_window_profile(input: &str) -> Option<ContextWindowProfile> {
    let normalized = input.trim().to_ascii_lowercase().replace('_', "-");
    match normalized.as_str() {
        "small" => Some(CONTEXT_WINDOW_PROFILES[0]),
        "normal" | "stable" => Some(CONTEXT_WINDOW_PROFILES[1]),
        "above normal" | "above-normal" | "expanded" => Some(CONTEXT_WINDOW_PROFILES[2]),
        "unstable" => Some(CONTEXT_WINDOW_PROFILES[3]),
        _ => None,
    }
}

fn profile_index_for_window(context_window: i64) -> usize {
    CONTEXT_WINDOW_PROFILES
        .iter()
        .enumerate()
        .min_by_key(|(_, profile)| profile.context_window.abs_diff(context_window))
        .map(|(index, _)| index)
        .unwrap_or(/*default*/ 1)
}

/// Four-stop context selector shown by `/context` without arguments.
///
/// Arrow keys move the selection and Enter applies it. Clicking or dragging over a stop applies
/// that value directly, so the control works with both terminal input styles.
pub(super) struct ContextWindowSlider {
    app_event_tx: AppEventSender,
    current_context_window: i64,
    selected: usize,
    option_areas: Cell<[Rect; 4]>,
    rendered_area: Cell<Rect>,
    mouse_dragging: bool,
    completion: Option<ViewCompletion>,
}

impl ContextWindowSlider {
    pub(super) fn new(current_context_window: i64, app_event_tx: AppEventSender) -> Self {
        Self {
            app_event_tx,
            current_context_window,
            selected: profile_index_for_window(current_context_window),
            option_areas: Cell::new([Rect::default(); 4]),
            rendered_area: Cell::new(Rect::default()),
            mouse_dragging: false,
            completion: None,
        }
    }

    fn accept(&mut self) {
        let profile = CONTEXT_WINDOW_PROFILES[self.selected];
        self.app_event_tx.send(AppEvent::PersistModelContextWindow {
            tier: profile.label.to_string(),
            context_window: profile.context_window,
        });
        self.completion = Some(ViewCompletion::Accepted);
    }

    fn set_selected_from_mouse(&mut self, column: u16, row: u16) -> bool {
        let Some(index) = self.option_areas.get().iter().position(|area| {
            !area.is_empty()
                && column >= area.left()
                && column < area.right()
                && row >= area.top()
                && row < area.bottom()
        }) else {
            return false;
        };
        self.selected = index;
        true
    }

    fn is_inside_rendered_area(&self, column: u16, row: u16) -> bool {
        let area = self.rendered_area.get();
        column >= area.left() && column < area.right() && row >= area.top() && row < area.bottom()
    }
}

impl Renderable for ContextWindowSlider {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.rendered_area.set(area);
        self.option_areas.set([Rect::default(); 4]);
        if area.is_empty() {
            return;
        }

        let frame = Block::default()
            .borders(Borders::ALL)
            .title(Line::from(" Context window ").style(Style::default().fg(Color::Cyan)));
        let inner = frame.inner(area);
        frame.render(area, buf);
        if inner.is_empty() {
            return;
        }

        let compact = inner.width < 68;
        let options_height = 4;
        let note_height = if inner.width < 40 { 3 } else { 2 };
        let [intro_area, options_area, note_area, footer_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(options_height),
            Constraint::Length(note_height),
            Constraint::Length(1),
        ])
        .areas(inner);

        let intro = if inner.width < 40 {
            "Choose a local context budget"
        } else {
            "Choose Web2Codex's local context budget"
        };
        Paragraph::new(intro)
            .style(Style::default().fg(Color::Gray))
            .render(intro_area, buf);

        let mut option_areas = [Rect::default(); 4];
        if compact {
            for (index, profile) in CONTEXT_WINDOW_PROFILES.iter().enumerate() {
                let option_area = Rect {
                    x: options_area.x,
                    y: options_area.y.saturating_add(index as u16),
                    width: options_area.width,
                    height: 1,
                };
                option_areas[index] = option_area;
                let selected = index == self.selected;
                let label_style = if selected {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                };
                let marker = if selected { "▶ " } else { "  " };
                let line = Line::from(vec![
                    Span::styled(marker, label_style),
                    Span::styled(profile.label, label_style),
                    Span::styled(
                        format!(" · {}k", profile.context_window / 1_000),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]);
                Paragraph::new(line).render(option_area, buf);
            }
        } else {
            let option_columns = Layout::horizontal([
                Constraint::Ratio(1, 4),
                Constraint::Ratio(1, 4),
                Constraint::Ratio(1, 4),
                Constraint::Ratio(1, 4),
            ])
            .split(options_area);
            for (index, (profile, option_area)) in CONTEXT_WINDOW_PROFILES
                .iter()
                .zip(option_columns.iter().copied())
                .enumerate()
            {
                option_areas[index] = option_area;
                let selected = index == self.selected;
                let accent = if selected {
                    Color::Cyan
                } else {
                    Color::DarkGray
                };
                let text_style = if selected {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                };
                let marker = if selected { "● " } else { "○ " };
                let card = Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(accent));
                let card_inner = card.inner(option_area);
                card.render(option_area, buf);
                Paragraph::new(vec![
                    Line::from(Span::styled(
                        format!("{marker}{}", profile.label),
                        text_style,
                    )),
                    Line::from(Span::styled(
                        format!("{}k tokens", profile.context_window / 1_000),
                        text_style,
                    )),
                ])
                .alignment(ratatui::layout::Alignment::Center)
                .render(card_inner, buf);
            }
        }
        self.option_areas.set(option_areas);

        let selected_profile = CONTEXT_WINDOW_PROFILES[self.selected];
        let compact_at = selected_profile.context_window.saturating_mul(9) / 10;
        let has_custom_context_window = !CONTEXT_WINDOW_PROFILES
            .iter()
            .any(|profile| profile.context_window == self.current_context_window);
        let note = if has_custom_context_window {
            format!(
                "Current: {}k (custom). Model and account limits may be lower; applying a level replaces it.",
                self.current_context_window / 1_000
            )
        } else if selected_profile.context_window == 400_000 {
            format!(
                "Experimental local 400k target; ChatGPT may reject it. Compaction starts near {}k.",
                compact_at / 1_000
            )
        } else {
            format!(
                "Local target; model and account limits may be lower. Compaction starts near {}k.",
                compact_at / 1_000
            )
        };
        Paragraph::new(note)
            .style(Style::default().fg(Color::DarkGray))
            .wrap(Wrap { trim: true })
            .render(note_area, buf);

        let footer = if inner.width < 40 {
            "←/→ · Enter · click · Esc"
        } else {
            "←/→ move · Enter apply · click a level · Esc cancel"
        };
        Paragraph::new(footer)
            .style(Style::default().fg(Color::DarkGray))
            .render(footer_area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        if width < 68 { 13 } else { 12 }
    }
}

impl BottomPaneView for ContextWindowSlider {
    fn presentation(&self) -> ViewPresentation {
        ViewPresentation::Centered
    }

    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if !matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return;
        }
        match key_event.code {
            KeyCode::Left | KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Right | KeyCode::Down => {
                self.selected = (self.selected + 1).min(CONTEXT_WINDOW_PROFILES.len() - 1)
            }
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = CONTEXT_WINDOW_PROFILES.len() - 1,
            KeyCode::Enter => self.accept(),
            KeyCode::Esc => self.completion = Some(ViewCompletion::Cancelled),
            _ => {}
        }
    }

    fn handle_mouse_event(&mut self, mouse_event: MouseEvent) -> bool {
        match mouse_event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if !self.is_inside_rendered_area(mouse_event.column, mouse_event.row) {
                    return false;
                }
                self.mouse_dragging =
                    self.set_selected_from_mouse(mouse_event.column, mouse_event.row);
                true
            }
            MouseEventKind::Drag(MouseButton::Left) if self.mouse_dragging => {
                self.set_selected_from_mouse(mouse_event.column, mouse_event.row);
                true
            }
            MouseEventKind::Up(MouseButton::Left) if self.mouse_dragging => {
                self.set_selected_from_mouse(mouse_event.column, mouse_event.row);
                self.mouse_dragging = false;
                self.accept();
                true
            }
            _ => false,
        }
    }

    fn keymap_contexts(&self) -> KeymapContextSet {
        KeymapContextSet::new(KeymapContext::List)
    }

    fn is_complete(&self) -> bool {
        self.completion.is_some()
    }

    fn completion(&self) -> Option<ViewCompletion> {
        self.completion
    }

    fn view_id(&self) -> Option<&'static str> {
        Some("context-window-slider")
    }
}

impl ChatWidget {
    pub(super) fn open_context_window_slider(&mut self) {
        let current_context_window = self.config.model_context_window.unwrap_or(200_000);
        let slider = ContextWindowSlider::new(current_context_window, self.app_event_tx.clone());
        self.bottom_pane.show_view(Box::new(slider));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_event::AppEvent;
    use crossterm::event::KeyModifiers;
    use ratatui::layout::Rect;
    use tokio::sync::mpsc::unbounded_channel;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Press)
    }

    #[test]
    fn context_profiles_have_requested_sizes_and_legacy_aliases() {
        assert_eq!(
            CONTEXT_WINDOW_PROFILES.map(|profile| profile.context_window),
            [80_000, 200_000, 270_000, 400_000]
        );
        assert_eq!(
            parse_context_window_profile("stable"),
            Some(CONTEXT_WINDOW_PROFILES[1])
        );
        assert_eq!(
            parse_context_window_profile("expanded"),
            Some(CONTEXT_WINDOW_PROFILES[2])
        );
        assert_eq!(
            parse_context_window_profile("above normal"),
            Some(CONTEXT_WINDOW_PROFILES[2])
        );
        assert_eq!(profile_index_for_window(128_000), 0);
    }

    #[test]
    fn arrow_keys_move_the_slider_and_enter_persists_the_selected_profile() {
        let (raw_tx, mut rx) = unbounded_channel();
        let mut slider = ContextWindowSlider::new(200_000, AppEventSender::new(raw_tx));

        slider.handle_key_event(key(KeyCode::Right));
        assert!(rx.try_recv().is_err(), "moving the slider must not persist");
        slider.handle_key_event(key(KeyCode::Enter));

        assert_eq!(slider.completion(), Some(ViewCompletion::Accepted));
        match rx.try_recv().expect("context selection event") {
            AppEvent::PersistModelContextWindow {
                tier,
                context_window,
            } => {
                assert_eq!(tier, "Above normal");
                assert_eq!(context_window, 270_000);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn keyboard_navigation_clamps_at_both_ends() {
        let (raw_tx, mut rx) = unbounded_channel();
        let mut slider = ContextWindowSlider::new(80_000, AppEventSender::new(raw_tx));

        slider.handle_key_event(key(KeyCode::Left));
        assert_eq!(slider.selected, 0);
        for _ in 0..8 {
            slider.handle_key_event(key(KeyCode::Right));
        }
        assert_eq!(slider.selected, 3);
        slider.handle_key_event(key(KeyCode::Right));
        assert_eq!(slider.selected, 3);
        slider.handle_key_event(key(KeyCode::Home));
        assert_eq!(slider.selected, 0);
        slider.handle_key_event(key(KeyCode::End));
        assert_eq!(slider.selected, 3);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn dragging_the_mouse_moves_and_applies_the_slider() {
        let (raw_tx, mut rx) = unbounded_channel();
        let mut slider = ContextWindowSlider::new(200_000, AppEventSender::new(raw_tx));
        let area = Rect::new(0, 0, 100, 11);
        let mut buffer = Buffer::empty(area);
        slider.render(area, &mut buffer);
        let options = slider.option_areas.get();
        let first = options[0];
        let last = options[3];
        let first_column = first.x + first.width / 2;
        let last_column = last.x + last.width / 2;
        let row = last.y + last.height / 2;

        assert!(slider.handle_mouse_event(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: first_column,
            row: first.y + first.height / 2,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(slider.handle_mouse_event(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: last_column,
            row,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(slider.handle_mouse_event(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: last_column,
            row,
            modifiers: KeyModifiers::NONE,
        }));

        match rx.try_recv().expect("context selection event") {
            AppEvent::PersistModelContextWindow {
                tier,
                context_window,
            } => {
                assert_eq!(tier, "Unstable");
                assert_eq!(context_window, 400_000);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn escape_cancels_without_persisting() {
        let (raw_tx, mut rx) = unbounded_channel();
        let mut slider = ContextWindowSlider::new(200_000, AppEventSender::new(raw_tx));

        slider.handle_key_event(key(KeyCode::Right));
        slider.handle_key_event(key(KeyCode::Esc));

        assert_eq!(slider.completion(), Some(ViewCompletion::Cancelled));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn compact_layout_keeps_mouse_targets_and_unstable_notice_visible() {
        let (raw_tx, _rx) = unbounded_channel();
        let mut slider = ContextWindowSlider::new(400_000, AppEventSender::new(raw_tx));
        let area = Rect::new(0, 0, 36, slider.desired_height(36));
        let mut buffer = Buffer::empty(area);

        slider.render(area, &mut buffer);

        assert!(
            slider
                .option_areas
                .get()
                .iter()
                .all(|target| !target.is_empty())
        );
        let rendered = (0..area.height)
            .map(|row| {
                (0..area.width)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Unstable"));
        assert!(rendered.contains("400k"));
        assert!(rendered.contains("360k"));
    }

    #[test]
    fn custom_current_window_is_not_silently_replaced_on_open() {
        let (raw_tx, mut rx) = unbounded_channel();
        let mut slider = ContextWindowSlider::new(128_000, AppEventSender::new(raw_tx));
        let area = Rect::new(0, 0, 80, slider.desired_height(80));
        let mut buffer = Buffer::empty(area);
        slider.render(area, &mut buffer);
        let rendered = (0..area.height)
            .map(|row| {
                (0..area.width)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(slider.selected, 0, "the nearest level is highlighted");
        assert!(rendered.contains("Current: 128k (custom)"));
        assert!(
            rx.try_recv().is_err(),
            "opening the selector must not persist"
        );
    }
}
