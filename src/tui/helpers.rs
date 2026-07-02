//! Split/squash helper affordances. Helpers shell out to jj only after the
//! user explicitly confirms the exact command in a two-step popup.

use crate::app::ReviewSession;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JjHelperState {
    pub(super) options: Vec<JjHelperOption>,
    pub(super) selected: usize,
    /// Second step: the selected command is shown verbatim and must be
    /// confirmed with Enter before anything shells out to jj.
    pub(super) confirming: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JjHelperOption {
    pub(super) label: String,
    pub(super) args: Vec<String>,
}

impl JjHelperOption {
    pub(super) fn command_line(&self) -> String {
        let mut parts = vec!["jj".to_owned()];
        parts.extend(self.args.iter().cloned());
        parts.join(" ")
    }
}

impl JjHelperState {
    /// Helper commands relevant to the current review target and selection.
    pub(super) fn for_session(session: &ReviewSession) -> Self {
        let rev = session.target.rev.clone();
        let mut options = vec![JjHelperOption {
            label: format!("squash {rev} into its parent"),
            args: string_args(["squash", "-r", &rev]),
        }];
        if let Some(file) = session.selected_visible_file() {
            options.push(JjHelperOption {
                label: format!("squash {} into the parent of {rev}", file.path),
                args: string_args(["squash", "-r", &rev, &file.path]),
            });
            options.push(JjHelperOption {
                label: format!("split {} out of {rev}", file.path),
                args: string_args(["split", "-r", &rev, &file.path]),
            });
        }
        Self {
            options,
            selected: 0,
            confirming: false,
        }
    }

    pub(super) fn move_selection(&mut self, delta: isize) {
        if self.confirming || self.options.is_empty() {
            return;
        }
        let max = self.options.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
    }

    pub(super) fn selected_option(&self) -> Option<&JjHelperOption> {
        self.options.get(self.selected)
    }
}

fn string_args<const N: usize>(args: [&str; N]) -> Vec<String> {
    args.into_iter().map(str::to_owned).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_support::snapshot_session;

    #[test]
    fn options_include_file_helpers_when_a_file_is_selected() {
        let session = snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1 +1 @@
-old
+new
"#,
        );

        let state = JjHelperState::for_session(&session);

        let commands: Vec<String> = state
            .options
            .iter()
            .map(JjHelperOption::command_line)
            .collect();
        assert_eq!(
            commands,
            [
                "jj squash -r @",
                "jj squash -r @ src/app.rs",
                "jj split -r @ src/app.rs",
            ]
        );
    }

    #[test]
    fn empty_session_only_offers_change_level_squash() {
        let session = snapshot_session("");

        let state = JjHelperState::for_session(&session);

        assert_eq!(state.options.len(), 1);
        assert_eq!(state.options[0].command_line(), "jj squash -r @");
    }

    #[test]
    fn selection_is_frozen_while_confirming() {
        let session = snapshot_session(
            r#"diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1 +1 @@
-old
+new
"#,
        );
        let mut state = JjHelperState::for_session(&session);

        state.move_selection(1);
        assert_eq!(state.selected, 1);

        state.confirming = true;
        state.move_selection(1);
        assert_eq!(state.selected, 1);
    }
}
