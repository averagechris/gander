//! Configurable keybindings: parsing key specs from config and mapping
//! crossterm key events to UI actions.

use color_eyre::eyre::{Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::KeybindingsConfig;

#[derive(Debug, Clone)]
pub struct KeyMap {
    bindings: Vec<KeyBinding>,
}

#[derive(Debug, Clone)]
struct KeyBinding {
    key: KeyPress,
    action: Action,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct KeyPress {
    code: KeyCode,
    modifiers: KeyModifiers,
    label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
    Quit,
    Help,
    SummonAgent,
    YankHandoff,
    MoveDown,
    MoveUp,
    ToggleFocus,
    DiffTop,
    DiffBottom,
    CompareTrunk,
    CompareParent,
    TargetChooser,
    RevsetInput,
    StackNext,
    StackPrevious,
    OperationPicker,
    JjHelpers,
    ToggleLargeDiff,
    ToggleAgentOrder,
    FlagList,
    OpenWork,
    Activity,
    WalkthroughList,
    Zen,
    DraftList,
    TargetPickerMoveDown,
    TargetPickerMoveUp,
    NextUnviewed,
    PreviousUnviewed,
    NextComment,
    PreviousComment,
    FileSearch,
    SymbolOutline,
    NextSymbol,
    PreviousSymbol,
    NextChangedHunk,
    PreviousChangedHunk,
    ScrollDown,
    ScrollUp,
    MarkViewed,
    ToggleViewed,
    MarkAllViewed,
    ToggleGenerated,
    CycleViewedFilter,
    ToggleFold,
    CollapseFold,
    ExpandFold,
    ToggleContextFold,
    ExpandContext,
    ExpandContextAll,
    CollapseContext,
    ViewOptions,
    ToggleWordHighlight,
    ToggleLineBackground,
    ToggleGutterBar,
    ToggleFilePane,
    ToggleDiffView,
    RangeComment,
    MarkWalkthrough,
    CancelRangeComment,
    Comment,
    CycleCommentState,
    EditComment,
    DeleteComment,
    CommentList,
    SubmitComment,
    CancelComment,
    InsertNewline,
    DeleteChar,
}

impl TryFrom<&KeybindingsConfig> for KeyMap {
    type Error = color_eyre::Report;

    fn try_from(config: &KeybindingsConfig) -> Result<Self> {
        let mut bindings = Vec::new();
        add_bindings(&mut bindings, Action::Quit, &config.quit)?;
        add_bindings(&mut bindings, Action::Help, &config.help)?;
        add_bindings(&mut bindings, Action::SummonAgent, &config.summon_agent)?;
        add_bindings(&mut bindings, Action::YankHandoff, &config.yank_handoff)?;
        add_bindings(&mut bindings, Action::MoveDown, &config.move_down)?;
        add_bindings(&mut bindings, Action::MoveUp, &config.move_up)?;
        add_bindings(&mut bindings, Action::ToggleFocus, &config.toggle_focus)?;
        add_bindings(&mut bindings, Action::DiffTop, &config.diff_top)?;
        add_bindings(&mut bindings, Action::DiffBottom, &config.diff_bottom)?;
        add_bindings(&mut bindings, Action::CompareTrunk, &config.compare_trunk)?;
        add_bindings(&mut bindings, Action::CompareParent, &config.compare_parent)?;
        add_bindings(&mut bindings, Action::TargetChooser, &config.target_chooser)?;
        add_bindings(&mut bindings, Action::RevsetInput, &config.revset_input)?;
        add_bindings(&mut bindings, Action::StackNext, &config.stack_next)?;
        add_bindings(&mut bindings, Action::StackPrevious, &config.stack_previous)?;
        add_bindings(
            &mut bindings,
            Action::OperationPicker,
            &config.operation_picker,
        )?;
        add_bindings(&mut bindings, Action::JjHelpers, &config.jj_helpers)?;
        add_bindings(
            &mut bindings,
            Action::ToggleLargeDiff,
            &config.toggle_large_diff,
        )?;
        add_bindings(
            &mut bindings,
            Action::ToggleAgentOrder,
            &config.toggle_agent_order,
        )?;
        add_bindings(&mut bindings, Action::FlagList, &config.flag_list)?;
        add_bindings(&mut bindings, Action::OpenWork, &config.open_work)?;
        add_bindings(&mut bindings, Action::Activity, &config.activity)?;
        add_bindings(&mut bindings, Action::Zen, &config.zen)?;
        add_bindings(&mut bindings, Action::DraftList, &config.draft_list)?;
        add_bindings(
            &mut bindings,
            Action::TargetPickerMoveDown,
            &config.target_picker_down,
        )?;
        add_bindings(
            &mut bindings,
            Action::TargetPickerMoveUp,
            &config.target_picker_up,
        )?;
        add_bindings(&mut bindings, Action::NextUnviewed, &config.next_unviewed)?;
        add_bindings(
            &mut bindings,
            Action::PreviousUnviewed,
            &config.previous_unviewed,
        )?;
        add_bindings(&mut bindings, Action::NextComment, &config.next_comment)?;
        add_bindings(
            &mut bindings,
            Action::PreviousComment,
            &config.previous_comment,
        )?;
        add_bindings(&mut bindings, Action::FileSearch, &config.file_search)?;
        add_bindings(&mut bindings, Action::SymbolOutline, &config.symbol_outline)?;
        add_bindings(&mut bindings, Action::NextSymbol, &config.next_symbol)?;
        add_bindings(
            &mut bindings,
            Action::PreviousSymbol,
            &config.previous_symbol,
        )?;
        add_bindings(
            &mut bindings,
            Action::NextChangedHunk,
            &config.next_changed_hunk,
        )?;
        add_bindings(
            &mut bindings,
            Action::PreviousChangedHunk,
            &config.previous_changed_hunk,
        )?;
        add_bindings(&mut bindings, Action::ScrollDown, &config.scroll_down)?;
        add_bindings(&mut bindings, Action::ScrollUp, &config.scroll_up)?;
        add_bindings(&mut bindings, Action::MarkViewed, &config.mark_viewed)?;
        add_bindings(&mut bindings, Action::ToggleViewed, &config.toggle_viewed)?;
        add_bindings(
            &mut bindings,
            Action::MarkAllViewed,
            &config.mark_all_viewed,
        )?;
        add_bindings(
            &mut bindings,
            Action::ToggleGenerated,
            &config.toggle_generated,
        )?;
        add_bindings(
            &mut bindings,
            Action::CycleViewedFilter,
            &config.cycle_viewed_filter,
        )?;
        add_bindings(&mut bindings, Action::ToggleFold, &config.toggle_fold)?;
        add_bindings(&mut bindings, Action::CollapseFold, &config.collapse_fold)?;
        add_bindings(&mut bindings, Action::ExpandFold, &config.expand_fold)?;
        add_bindings(
            &mut bindings,
            Action::ToggleContextFold,
            &config.toggle_context_fold,
        )?;
        add_bindings(&mut bindings, Action::ExpandContext, &config.expand_context)?;
        add_bindings(
            &mut bindings,
            Action::ExpandContextAll,
            &config.expand_context_all,
        )?;
        add_bindings(
            &mut bindings,
            Action::CollapseContext,
            &config.collapse_context,
        )?;
        add_bindings(&mut bindings, Action::ViewOptions, &config.view_options)?;
        add_bindings(
            &mut bindings,
            Action::ToggleWordHighlight,
            &config.toggle_word_highlight,
        )?;
        add_bindings(
            &mut bindings,
            Action::ToggleLineBackground,
            &config.toggle_line_background,
        )?;
        add_bindings(
            &mut bindings,
            Action::ToggleGutterBar,
            &config.toggle_gutter_bar,
        )?;
        add_bindings(
            &mut bindings,
            Action::WalkthroughList,
            &config.walkthrough_list,
        )?;
        add_bindings(
            &mut bindings,
            Action::ToggleFilePane,
            &config.toggle_file_pane,
        )?;
        add_bindings(
            &mut bindings,
            Action::ToggleDiffView,
            &config.toggle_diff_view,
        )?;
        add_bindings(&mut bindings, Action::RangeComment, &config.range_comment)?;
        add_bindings(
            &mut bindings,
            Action::MarkWalkthrough,
            &config.mark_walkthrough,
        )?;
        add_bindings(
            &mut bindings,
            Action::CancelRangeComment,
            &config.cancel_range_comment,
        )?;
        add_bindings(&mut bindings, Action::Comment, &config.comment)?;
        add_bindings(
            &mut bindings,
            Action::CycleCommentState,
            &config.cycle_comment_state,
        )?;
        add_bindings(&mut bindings, Action::EditComment, &config.edit_comment)?;
        add_bindings(&mut bindings, Action::DeleteComment, &config.delete_comment)?;
        add_bindings(&mut bindings, Action::CommentList, &config.comment_list)?;
        add_bindings(&mut bindings, Action::SubmitComment, &config.submit_comment)?;
        add_bindings(&mut bindings, Action::CancelComment, &config.cancel_comment)?;
        add_bindings(&mut bindings, Action::InsertNewline, &config.insert_newline)?;
        add_bindings(&mut bindings, Action::DeleteChar, &config.delete_char)?;
        Ok(Self { bindings })
    }
}

impl KeyMap {
    pub(super) fn action_for(&self, key: &KeyEvent) -> Option<Action> {
        self.bindings
            .iter()
            .find(|binding| binding.key.matches(key))
            .map(|binding| binding.action)
            .or_else(|| normal_movement_action_for(key))
    }

    pub(super) fn comment_action_for(&self, key: &KeyEvent) -> Option<Action> {
        self.bindings
            .iter()
            .find(|binding| {
                binding.key.matches(key)
                    && matches!(
                        binding.action,
                        Action::SubmitComment
                            | Action::CancelComment
                            | Action::InsertNewline
                            | Action::DeleteChar
                    )
            })
            .map(|binding| binding.action)
    }

    pub(super) fn target_picker_action_for(&self, key: &KeyEvent) -> Option<Action> {
        self.bindings
            .iter()
            .find(|binding| {
                binding.key.matches(key)
                    && matches!(
                        binding.action,
                        Action::TargetPickerMoveDown | Action::TargetPickerMoveUp
                    )
            })
            .map(|binding| binding.action)
            .or_else(|| picker_movement_action_for(key))
    }

    pub(super) fn hint(&self, action: Action) -> &str {
        self.bindings
            .iter()
            .find(|binding| binding.action == action)
            .map(|binding| binding.key.label.as_str())
            .unwrap_or("?")
    }
}

fn normal_movement_action_for(key: &KeyEvent) -> Option<Action> {
    if !key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
        return None;
    }
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => Some(Action::MoveDown),
        KeyCode::Char('k') | KeyCode::Up => Some(Action::MoveUp),
        _ => None,
    }
}

fn picker_movement_action_for(key: &KeyEvent) -> Option<Action> {
    if !key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
        return None;
    }
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => Some(Action::TargetPickerMoveDown),
        KeyCode::Char('k') | KeyCode::Up => Some(Action::TargetPickerMoveUp),
        _ => None,
    }
}

impl KeyPress {
    fn matches(&self, key: &KeyEvent) -> bool {
        if self.code != key.code {
            return false;
        }
        // Character keys already encode shift in the character itself
        // (`G` vs `g`), but terminals may or may not report the SHIFT
        // modifier alongside the uppercase char. Ignore SHIFT for char keys
        // unless the binding explicitly asks for it.
        if let KeyCode::Char(_) = self.code
            && !self.modifiers.contains(KeyModifiers::SHIFT)
        {
            return self.modifiers == key.modifiers.difference(KeyModifiers::SHIFT);
        }
        self.modifiers == key.modifiers
    }
}

fn add_bindings(bindings: &mut Vec<KeyBinding>, action: Action, keys: &[String]) -> Result<()> {
    for key in keys {
        bindings.push(KeyBinding {
            key: parse_key(key)?,
            action,
        });
    }
    Ok(())
}

fn parse_key(raw: &str) -> Result<KeyPress> {
    let normalized = raw.trim().to_ascii_lowercase();
    // Bare single characters (including `+` and `-`, which double as
    // modifier separators below) bind directly.
    if raw.trim().chars().count() == 1 {
        return Ok(KeyPress {
            code: KeyCode::Char(raw.trim().chars().next().unwrap()),
            modifiers: KeyModifiers::empty(),
            label: raw.to_owned(),
        });
    }
    if matches!(normalized.as_str(), "page-up" | "page-down") {
        return Ok(KeyPress {
            code: if normalized == "page-up" {
                KeyCode::PageUp
            } else {
                KeyCode::PageDown
            },
            modifiers: KeyModifiers::empty(),
            label: raw.to_owned(),
        });
    }
    let parts: Vec<_> = normalized.split(['-', '+']).collect();
    let (modifiers, key_name) = parse_key_parts(&parts, raw)?;
    let code = match key_name {
        "esc" | "escape" => KeyCode::Esc,
        "enter" | "return" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "backspace" | "bs" => KeyCode::Backspace,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "pageup" | "page-up" | "pgup" => KeyCode::PageUp,
        "pagedown" | "page-down" | "pgdn" => KeyCode::PageDown,
        "space" => KeyCode::Char(' '),
        _ if key_name.chars().count() == 1 => {
            KeyCode::Char(if modifiers.is_empty() && raw.trim().chars().count() == 1 {
                raw.trim().chars().next().unwrap()
            } else {
                key_name.chars().next().unwrap()
            })
        }
        _ => bail!("unsupported keybinding `{raw}`"),
    };
    Ok(KeyPress {
        code,
        modifiers,
        label: raw.to_owned(),
    })
}

fn parse_key_parts<'a>(parts: &'a [&'a str], raw: &str) -> Result<(KeyModifiers, &'a str)> {
    let Some((key_name, modifiers)) = parts.split_last() else {
        bail!("unsupported keybinding `{raw}`");
    };
    let mut parsed = KeyModifiers::empty();
    for modifier in modifiers {
        match *modifier {
            "ctrl" | "control" => parsed |= KeyModifiers::CONTROL,
            "alt" => parsed |= KeyModifiers::ALT,
            "shift" => parsed |= KeyModifiers::SHIFT,
            _ => bail!("unsupported keybinding `{raw}`"),
        }
    }
    Ok((parsed, key_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_named_and_character_keys() {
        assert_eq!(parse_key("down").unwrap().code, KeyCode::Down);
        assert_eq!(parse_key("pagedown").unwrap().code, KeyCode::PageDown);
        assert_eq!(parse_key("page-down").unwrap().code, KeyCode::PageDown);
        assert_eq!(parse_key("N").unwrap().code, KeyCode::Char('N'));
        assert_eq!(parse_key("space").unwrap().code, KeyCode::Char(' '));
        assert_eq!(parse_key("+").unwrap().code, KeyCode::Char('+'));
        assert_eq!(parse_key("-").unwrap().code, KeyCode::Char('-'));
        assert_eq!(parse_key("=").unwrap().code, KeyCode::Char('='));
    }

    #[test]
    fn parses_modified_keys() {
        let key = parse_key("ctrl-s").unwrap();

        assert_eq!(key.code, KeyCode::Char('s'));
        assert_eq!(key.modifiers, KeyModifiers::CONTROL);
    }

    #[test]
    fn uppercase_char_bindings_match_with_or_without_shift_modifier() {
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('G'))),
            Some(Action::DiffBottom)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT)),
            Some(Action::DiffBottom)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT)),
            Some(Action::RevsetInput)
        );
    }

    #[test]
    fn agent_and_jj_actions_have_default_bindings() {
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        // Regression: these actions used to be missing from KeyMap::try_from,
        // leaving them unbound (`?` hints in the footer, dead keys).
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('A'))),
            Some(Action::ToggleAgentOrder)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('F'))),
            Some(Action::FlagList)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('X'))),
            Some(Action::OpenWork)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            Some(Action::Activity)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('T'))),
            Some(Action::Zen)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('Z'))),
            Some(Action::Zen)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('D'))),
            Some(Action::DraftList)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('I'))),
            Some(Action::OperationPicker)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('!'))),
            Some(Action::JjHelpers)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('L'))),
            Some(Action::ToggleLargeDiff)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('?'))),
            Some(Action::Help)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL)),
            Some(Action::YankHandoff)
        );
        assert_eq!(keymap.hint(Action::ToggleAgentOrder), "A");
        assert_eq!(keymap.hint(Action::OpenWork), "X");
        assert_eq!(keymap.hint(Action::Activity), "ctrl-a");
        assert_eq!(keymap.hint(Action::Help), "?");
        assert_eq!(keymap.hint(Action::YankHandoff), "ctrl-y");
    }

    #[test]
    fn default_changed_hunk_keybindings_map_to_actions() {
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('}'))),
            Some(Action::NextChangedHunk)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('{'))),
            Some(Action::PreviousChangedHunk)
        );
    }

    #[test]
    fn default_view_options_keybinding_maps_to_action() {
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('V'))),
            Some(Action::ViewOptions)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('|'))),
            Some(Action::ToggleDiffView)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('w'))),
            Some(Action::ToggleFilePane)
        );
        // Direct cue toggles ship unbound but stay bindable via config.
        let config = KeybindingsConfig {
            toggle_word_highlight: vec!["W".to_owned()],
            ..KeybindingsConfig::default()
        };
        let keymap = KeyMap::try_from(&config).unwrap();
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('W'))),
            Some(Action::ToggleWordHighlight)
        );
    }

    #[test]
    fn rejects_unsupported_key_names() {
        assert!(parse_key("hyper-space").is_err());
    }

    #[test]
    fn configured_key_overrides_default_action() {
        let config = KeybindingsConfig {
            move_down: vec!["s".to_owned()],
            ..KeybindingsConfig::default()
        };
        let keymap = KeyMap::try_from(&config).unwrap();

        let key = KeyEvent::from(KeyCode::Char('s'));

        assert_eq!(keymap.action_for(&key), Some(Action::MoveDown));
        assert_eq!(keymap.hint(Action::MoveDown), "s");
    }

    #[test]
    fn movement_fallbacks_keep_j_k_and_arrows_available() {
        let config = KeybindingsConfig {
            move_down: vec!["n".to_owned()],
            move_up: vec!["p".to_owned()],
            target_picker_down: vec!["ctrl-j".to_owned()],
            target_picker_up: vec!["ctrl-k".to_owned()],
            ..KeybindingsConfig::default()
        };
        let keymap = KeyMap::try_from(&config).unwrap();

        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('j'))),
            Some(Action::MoveDown)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('k'))),
            Some(Action::MoveUp)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Down)),
            Some(Action::MoveDown)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Up)),
            Some(Action::MoveUp)
        );
        assert_eq!(
            keymap.target_picker_action_for(&KeyEvent::from(KeyCode::Char('j'))),
            Some(Action::TargetPickerMoveDown)
        );
        assert_eq!(
            keymap.target_picker_action_for(&KeyEvent::from(KeyCode::Char('k'))),
            Some(Action::TargetPickerMoveUp)
        );
    }

    #[test]
    fn default_fold_keybindings_map_to_actions() {
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char(' '))),
            Some(Action::ToggleFold)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Left)),
            Some(Action::CollapseFold)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Right)),
            Some(Action::ExpandFold)
        );
    }

    #[test]
    fn default_context_expansion_keybindings_map_to_actions() {
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('+'))),
            Some(Action::ExpandContext)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('='))),
            Some(Action::ExpandContextAll)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('-'))),
            Some(Action::CollapseContext)
        );
    }

    #[test]
    fn default_generated_keybinding_maps_to_action() {
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('h'))),
            Some(Action::ToggleGenerated)
        );
    }

    #[test]
    fn default_comment_edit_delete_keybindings_map_to_actions() {
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('e'))),
            Some(Action::EditComment)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('x'))),
            Some(Action::DeleteComment)
        );
    }

    #[test]
    fn comment_mode_uses_comment_actions_only() {
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        assert_eq!(
            keymap.comment_action_for(&KeyEvent::from(KeyCode::Down)),
            None
        );
        assert_eq!(
            keymap.comment_action_for(&KeyEvent::from(KeyCode::Enter)),
            Some(Action::InsertNewline)
        );
        assert_eq!(
            keymap.comment_action_for(&KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            Some(Action::SubmitComment)
        );
    }

    #[test]
    fn default_compare_keybindings_map_to_actions() {
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('t'))),
            Some(Action::CompareTrunk)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('p'))),
            Some(Action::CompareParent)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('b'))),
            Some(Action::TargetChooser)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('R'))),
            Some(Action::RevsetInput)
        );
    }

    #[test]
    fn default_target_picker_keybindings_map_to_actions() {
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        assert_eq!(
            keymap.target_picker_action_for(&KeyEvent::from(KeyCode::Down)),
            Some(Action::TargetPickerMoveDown)
        );
        assert_eq!(
            keymap.target_picker_action_for(&KeyEvent::new(
                KeyCode::Char('j'),
                KeyModifiers::CONTROL
            )),
            Some(Action::TargetPickerMoveDown)
        );
        assert_eq!(
            keymap.target_picker_action_for(&KeyEvent::from(KeyCode::Up)),
            Some(Action::TargetPickerMoveUp)
        );
        assert_eq!(
            keymap.target_picker_action_for(&KeyEvent::new(
                KeyCode::Char('k'),
                KeyModifiers::CONTROL
            )),
            Some(Action::TargetPickerMoveUp)
        );
    }

    #[test]
    fn default_range_keybindings_map_to_actions() {
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('r'))),
            Some(Action::RangeComment)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL)),
            Some(Action::CancelRangeComment)
        );
    }

    #[test]
    fn esc_dismisses_instead_of_quitting_by_default() {
        let keymap = KeyMap::try_from(&KeybindingsConfig::default()).unwrap();

        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Esc)),
            Some(Action::CancelRangeComment)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::from(KeyCode::Char('q'))),
            Some(Action::Quit)
        );
    }
}
