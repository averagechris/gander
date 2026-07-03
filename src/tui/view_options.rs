//! View options popup: runtime, session-only toggles for the diff visual
//! cues (docs/focused-diff-ux.md). Config sets the defaults; this popup
//! flips them for the current session.

use crate::app::ReviewSession;

/// One toggleable entry in the view options popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ViewOption {
    WordHighlight,
    LineBackground,
    GutterBar,
    FilePane,
    SideBySide,
}

impl ViewOption {
    pub(super) const ALL: [Self; 5] = [
        Self::WordHighlight,
        Self::LineBackground,
        Self::GutterBar,
        Self::FilePane,
        Self::SideBySide,
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::WordHighlight => "word-level change highlights",
            Self::LineBackground => "line backgrounds",
            Self::GutterBar => "gutter change bar",
            Self::FilePane => "file pane",
            Self::SideBySide => "side-by-side view",
        }
    }

    pub(super) fn enabled(self, session: &ReviewSession) -> bool {
        match self {
            Self::WordHighlight => session.diff_cues.word_highlight,
            Self::LineBackground => session.diff_cues.line_background,
            Self::GutterBar => session.diff_cues.gutter_bar,
            Self::FilePane => session.file_pane_visible,
            Self::SideBySide => {
                session.diff_cues.view == crate::config::DiffViewModeConfig::SideBySide
            }
        }
    }

    pub(super) fn toggle(self, session: &mut ReviewSession) {
        match self {
            Self::WordHighlight => session.toggle_word_highlight(),
            Self::LineBackground => session.toggle_line_background(),
            Self::GutterBar => session.toggle_gutter_bar(),
            Self::FilePane => session.toggle_file_pane(),
            Self::SideBySide => session.toggle_diff_view(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ViewOptionsState {
    pub(super) selected: usize,
}

impl ViewOptionsState {
    pub(super) fn move_selection(&mut self, delta: isize) {
        let max = ViewOption::ALL.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
    }

    pub(super) fn selected_option(&self) -> ViewOption {
        ViewOption::ALL[self.selected.min(ViewOption::ALL.len() - 1)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_support::snapshot_session;

    #[test]
    fn selection_clamps_to_option_count() {
        let mut state = ViewOptionsState::default();

        state.move_selection(10);
        assert_eq!(state.selected_option(), ViewOption::SideBySide);

        state.move_selection(-10);
        assert_eq!(state.selected_option(), ViewOption::WordHighlight);
    }

    #[test]
    fn toggling_options_flips_session_cues() {
        let mut session = snapshot_session("");
        assert!(ViewOption::WordHighlight.enabled(&session));
        assert!(ViewOption::LineBackground.enabled(&session));
        assert!(!ViewOption::GutterBar.enabled(&session));

        ViewOption::WordHighlight.toggle(&mut session);
        ViewOption::GutterBar.toggle(&mut session);

        assert!(!session.diff_cues.word_highlight);
        assert!(session.diff_cues.gutter_bar);
    }
}
