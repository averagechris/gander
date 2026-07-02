//! Agent-flagged critical sections: list popup state and navigation.

use crate::{agent::AgentFlag, app::ReviewSession};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FlagListState {
    /// Flags sorted critical-first (see [`ReviewSession::flags_sorted`]).
    pub(super) flags: Vec<AgentFlag>,
    pub(super) selected: usize,
}

impl FlagListState {
    pub(super) fn new(session: &ReviewSession) -> Self {
        Self {
            flags: session.flags_sorted(),
            selected: 0,
        }
    }

    pub(super) fn move_selection(&mut self, delta: isize) {
        if self.flags.is_empty() {
            self.selected = 0;
            return;
        }
        let max = self.flags.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
    }

    pub(super) fn selected_flag(&self) -> Option<&AgentFlag> {
        self.flags.get(self.selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentOverlay, FlagPriority};
    use crate::tui::test_support::snapshot_session;

    fn flag(path: &str, line: Option<usize>, priority: FlagPriority) -> AgentFlag {
        AgentFlag {
            id: format!("{path}:{line:?}"),
            path: path.to_owned(),
            line,
            reason: "risky".to_owned(),
            priority,
        }
    }

    #[test]
    fn flags_are_sorted_critical_first() {
        let mut session = snapshot_session("");
        session.apply_agent_overlay(&AgentOverlay {
            flags: vec![
                flag("b.rs", Some(3), FlagPriority::Low),
                flag("a.rs", Some(1), FlagPriority::Critical),
                flag("a.rs", None, FlagPriority::High),
            ],
            ..Default::default()
        });

        let state = FlagListState::new(&session);

        let order: Vec<(&str, FlagPriority)> = state
            .flags
            .iter()
            .map(|flag| (flag.path.as_str(), flag.priority))
            .collect();
        assert_eq!(
            order,
            [
                ("a.rs", FlagPriority::Critical),
                ("a.rs", FlagPriority::High),
                ("b.rs", FlagPriority::Low),
            ]
        );
    }

    #[test]
    fn selection_clamps_to_flag_count() {
        let mut session = snapshot_session("");
        session.apply_agent_overlay(&AgentOverlay {
            flags: vec![flag("a.rs", Some(1), FlagPriority::High)],
            ..Default::default()
        });
        let mut state = FlagListState::new(&session);

        state.move_selection(5);
        assert_eq!(state.selected, 0);
        assert_eq!(state.selected_flag().unwrap().path, "a.rs");
    }
}
