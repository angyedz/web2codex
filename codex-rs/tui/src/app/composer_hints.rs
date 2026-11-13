//! Optional composer hints appear above the fullscreen composer when useful.

use super::*;
use crate::terminal_hyperlinks::HyperlinkLine;

impl App {
    pub(super) fn composer_hint(&self, width: u16) -> Option<HyperlinkLine> {
        if !self.chat_widget.no_modal_or_popup_active()
            || !self.transcript_view.is_following()
            || self.transcript_view.has_active_interaction()
            || self.backtrack.primed
            || self.backtrack.overlay_preview_active
        {
            return None;
        }
        let width = width.checked_sub(/*rhs*/ 2)?;
        if width == 0 {
            return None;
        }
        let _ = width;
        None
    }
}

#[cfg(test)]
#[path = "composer_hints_tests.rs"]
mod tests;
