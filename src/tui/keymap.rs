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

#[derive(Debug, Clone)]
struct KeyPress {
    code: KeyCode,
    modifiers: KeyModifiers,
    label: String,
}

impl PartialEq for KeyPress {
    fn eq(&self, other: &Self) -> bool {
        self.code == other.code && self.modifiers == other.modifiers
    }
}

impl Eq for KeyPress {}

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
    PopupMoveDown,
    PopupMoveUp,
    PopupSelect,
    PopupToggle,
    PopupClose,
    PopupCloseQ,
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
    ScrollDiffLeft,
    ScrollDiffRight,
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
    ToggleDiffWrap,
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
    CommentListNewGeneral,
    CommentListReady,
    CommentListCycleIntent,
    CommentListCycleKind,
    DraftAccept,
    DraftEdit,
    DraftDiscard,
    WalkthroughDelete,
    WalkthroughMoveDown,
    WalkthroughMoveUp,
    ZenNext,
    ZenPrevious,
    ZenToggleView,
    ZenGlance,
    ZenArtifact,
    ZenToggleDetails,
    ZenRefocus,
    ZenAcknowledge,
    ZenArtifactNext,
    ZenArtifactPrevious,
    SubmitComment,
    CancelComment,
    InsertNewline,
    DeleteChar,
}

/// An effective input surface. Bindings may be reused freely when their
/// actions never coexist in one of these contexts (for example Enter in normal
/// review, a popup, and the comment editor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KeyContext {
    NormalFiles,
    NormalDiff,
    Help,
    TargetChooser,
    RevsetInput,
    OperationPicker,
    JjHelpers,
    FlagList,
    OpenWork,
    Activity,
    WalkthroughList,
    DraftList,
    FileSearch,
    SymbolOutline,
    CommentList,
    ViewOptions,
    CommentEditor,
    ZenFocus,
    ZenGlance,
    ZenArtifact,
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
        add_bindings(
            &mut bindings,
            Action::PopupMoveDown,
            &config.popup_move_down,
        )?;
        add_bindings(&mut bindings, Action::PopupMoveUp, &config.popup_move_up)?;
        add_bindings(&mut bindings, Action::PopupSelect, &config.popup_select)?;
        add_bindings(&mut bindings, Action::PopupToggle, &config.popup_toggle)?;
        add_bindings(&mut bindings, Action::PopupClose, &config.popup_close)?;
        add_bindings(&mut bindings, Action::PopupCloseQ, &config.popup_close_q)?;
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
        add_bindings(
            &mut bindings,
            Action::ScrollDiffLeft,
            &config.scroll_diff_left,
        )?;
        add_bindings(
            &mut bindings,
            Action::ScrollDiffRight,
            &config.scroll_diff_right,
        )?;
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
            Action::ToggleDiffWrap,
            &config.toggle_diff_wrap,
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
        add_bindings(
            &mut bindings,
            Action::CommentListNewGeneral,
            &config.comment_list_new_general,
        )?;
        add_bindings(
            &mut bindings,
            Action::CommentListReady,
            &config.comment_list_ready,
        )?;
        add_bindings(
            &mut bindings,
            Action::CommentListCycleIntent,
            &config.comment_list_cycle_action,
        )?;
        add_bindings(
            &mut bindings,
            Action::CommentListCycleKind,
            &config.comment_list_cycle_kind,
        )?;
        add_bindings(&mut bindings, Action::DraftAccept, &config.draft_accept)?;
        add_bindings(&mut bindings, Action::DraftEdit, &config.draft_edit)?;
        add_bindings(&mut bindings, Action::DraftDiscard, &config.draft_discard)?;
        add_bindings(
            &mut bindings,
            Action::WalkthroughDelete,
            &config.walkthrough_delete,
        )?;
        add_bindings(
            &mut bindings,
            Action::WalkthroughMoveDown,
            &config.walkthrough_move_down,
        )?;
        add_bindings(
            &mut bindings,
            Action::WalkthroughMoveUp,
            &config.walkthrough_move_up,
        )?;
        add_bindings(&mut bindings, Action::ZenNext, &config.zen_next)?;
        add_bindings(&mut bindings, Action::ZenPrevious, &config.zen_previous)?;
        add_bindings(
            &mut bindings,
            Action::ZenToggleView,
            &config.zen_toggle_view,
        )?;
        add_bindings(&mut bindings, Action::ZenGlance, &config.zen_glance)?;
        add_bindings(&mut bindings, Action::ZenArtifact, &config.zen_artifact)?;
        add_bindings(
            &mut bindings,
            Action::ZenToggleDetails,
            &config.zen_toggle_details,
        )?;
        add_bindings(&mut bindings, Action::ZenRefocus, &config.zen_refocus)?;
        add_bindings(
            &mut bindings,
            Action::ZenAcknowledge,
            &config.zen_acknowledge,
        )?;
        add_bindings(
            &mut bindings,
            Action::ZenArtifactNext,
            &config.zen_artifact_next,
        )?;
        add_bindings(
            &mut bindings,
            Action::ZenArtifactPrevious,
            &config.zen_artifact_previous,
        )?;
        add_bindings(&mut bindings, Action::SubmitComment, &config.submit_comment)?;
        add_bindings(&mut bindings, Action::CancelComment, &config.cancel_comment)?;
        add_bindings(&mut bindings, Action::InsertNewline, &config.insert_newline)?;
        add_bindings(&mut bindings, Action::DeleteChar, &config.delete_char)?;
        validate_collisions(&bindings)?;
        Ok(Self { bindings })
    }
}

impl KeyMap {
    /// Normal-review lookup retained for focused keymap regression tests.
    #[cfg(test)]
    pub(super) fn action_for(&self, key: &KeyEvent) -> Option<Action> {
        self.bindings
            .iter()
            .find(|binding| {
                binding.action.contexts().iter().any(|context| {
                    matches!(context, KeyContext::NormalFiles | KeyContext::NormalDiff)
                }) && binding.key.matches(key)
            })
            .map(|binding| binding.action)
            .or_else(|| normal_movement_action_for(key))
    }

    pub(super) fn action_for_context(&self, context: KeyContext, key: &KeyEvent) -> Option<Action> {
        self.bindings.iter().find_map(|binding| {
            (binding.action.contexts().contains(&context) && binding.key.matches(key))
                .then_some(binding.action)
        })
    }

    pub(super) fn normal_action_for(&self, key: &KeyEvent, diff_focus: bool) -> Option<Action> {
        let context = if diff_focus {
            KeyContext::NormalDiff
        } else {
            KeyContext::NormalFiles
        };
        self.action_for_context(context, key)
            .or_else(|| normal_movement_action_for(key))
    }

    pub(super) fn comment_action_for(&self, key: &KeyEvent) -> Option<Action> {
        self.action_for_context(KeyContext::CommentEditor, key)
    }

    pub(super) fn filter_action_for(&self, context: KeyContext, key: &KeyEvent) -> Option<Action> {
        self.action_for_context(context, key)
            .or_else(|| filter_movement_action_for(key))
            .or_else(|| popup_safety_action_for(key))
    }

    pub(super) fn popup_action_for(&self, context: KeyContext, key: &KeyEvent) -> Option<Action> {
        self.action_for_context(context, key)
            .or_else(|| popup_movement_action_for(key))
            .or_else(|| popup_safety_action_for(key))
    }

    #[cfg(test)]
    pub(super) fn target_picker_action_for(&self, key: &KeyEvent) -> Option<Action> {
        self.filter_action_for(KeyContext::TargetChooser, key)
    }

    pub(super) fn hint(&self, action: Action) -> &str {
        self.bindings
            .iter()
            .find(|binding| binding.action == action)
            .map(|binding| binding.key.label.as_str())
            .unwrap_or("?")
    }
}

impl Action {
    fn contexts(self) -> &'static [KeyContext] {
        use Action::*;
        const NORMAL: &[KeyContext] = &[KeyContext::NormalFiles, KeyContext::NormalDiff];
        const DIFF: &[KeyContext] = &[KeyContext::NormalDiff];
        const FILES: &[KeyContext] = &[KeyContext::NormalFiles];
        const FILTERS: &[KeyContext] = &[KeyContext::TargetChooser, KeyContext::FileSearch];
        const LISTS: &[KeyContext] = &[
            KeyContext::Help,
            KeyContext::OperationPicker,
            KeyContext::JjHelpers,
            KeyContext::FlagList,
            KeyContext::OpenWork,
            KeyContext::Activity,
            KeyContext::WalkthroughList,
            KeyContext::DraftList,
            KeyContext::SymbolOutline,
            KeyContext::CommentList,
            KeyContext::ViewOptions,
            KeyContext::ZenGlance,
            KeyContext::ZenArtifact,
        ];
        const SELECTS: &[KeyContext] = &[
            KeyContext::TargetChooser,
            KeyContext::RevsetInput,
            KeyContext::OperationPicker,
            KeyContext::JjHelpers,
            KeyContext::FlagList,
            KeyContext::OpenWork,
            KeyContext::Activity,
            KeyContext::WalkthroughList,
            KeyContext::FileSearch,
            KeyContext::SymbolOutline,
            KeyContext::CommentList,
            KeyContext::ViewOptions,
            KeyContext::ZenGlance,
            KeyContext::ZenArtifact,
        ];
        const POPUPS: &[KeyContext] = &[
            KeyContext::Help,
            KeyContext::TargetChooser,
            KeyContext::RevsetInput,
            KeyContext::OperationPicker,
            KeyContext::JjHelpers,
            KeyContext::FlagList,
            KeyContext::OpenWork,
            KeyContext::Activity,
            KeyContext::WalkthroughList,
            KeyContext::DraftList,
            KeyContext::FileSearch,
            KeyContext::SymbolOutline,
            KeyContext::CommentList,
            KeyContext::ViewOptions,
            KeyContext::ZenGlance,
            KeyContext::ZenArtifact,
        ];
        match self {
            Quit | SummonAgent | YankHandoff | MoveDown | MoveUp | ToggleFocus | CompareTrunk
            | CompareParent | TargetChooser | RevsetInput | StackNext | StackPrevious
            | OperationPicker | JjHelpers | ToggleAgentOrder | FlagList | OpenWork | Activity
            | WalkthroughList | Zen | DraftList | NextUnviewed | PreviousUnviewed | NextComment
            | PreviousComment | FileSearch | ToggleFilePane | ViewOptions | Comment
            | CommentList | CancelRangeComment => NORMAL,
            Help => &[
                KeyContext::NormalFiles,
                KeyContext::NormalDiff,
                KeyContext::Help,
            ],
            DiffTop | DiffBottom | SymbolOutline | NextSymbol | PreviousSymbol
            | NextChangedHunk | PreviousChangedHunk | ScrollDown | ScrollUp | ScrollDiffLeft
            | ScrollDiffRight | ToggleContextFold | ExpandContext | ExpandContextAll
            | CollapseContext | ToggleWordHighlight | ToggleLineBackground | ToggleGutterBar
            | ToggleDiffWrap | ToggleDiffView | ToggleLargeDiff | RangeComment
            | MarkWalkthrough => DIFF,
            MarkViewed | ToggleViewed | MarkAllViewed | ToggleGenerated | CycleViewedFilter => {
                NORMAL
            }
            ToggleFold | CollapseFold | ExpandFold => FILES,
            CycleCommentState | EditComment | DeleteComment => &[
                KeyContext::NormalFiles,
                KeyContext::NormalDiff,
                KeyContext::CommentList,
            ],
            TargetPickerMoveDown | TargetPickerMoveUp => FILTERS,
            PopupMoveDown | PopupMoveUp => LISTS,
            PopupSelect => SELECTS,
            PopupToggle => &[KeyContext::ViewOptions],
            PopupClose => POPUPS,
            PopupCloseQ => &[
                KeyContext::Help,
                KeyContext::ViewOptions,
                KeyContext::ZenArtifact,
            ],
            CommentListNewGeneral
            | CommentListReady
            | CommentListCycleIntent
            | CommentListCycleKind => &[KeyContext::CommentList],
            DraftAccept | DraftEdit | DraftDiscard => &[KeyContext::DraftList],
            WalkthroughDelete | WalkthroughMoveDown | WalkthroughMoveUp => {
                &[KeyContext::WalkthroughList]
            }
            ZenNext | ZenToggleView | ZenGlance | ZenToggleDetails | ZenRefocus => {
                &[KeyContext::ZenFocus]
            }
            ZenArtifact => &[KeyContext::ZenFocus, KeyContext::ZenArtifact],
            ZenPrevious => &[KeyContext::ZenFocus, KeyContext::ZenGlance],
            ZenAcknowledge => &[KeyContext::ZenGlance],
            ZenArtifactNext | ZenArtifactPrevious => &[KeyContext::ZenArtifact],
            SubmitComment | CancelComment | InsertNewline | DeleteChar => {
                &[KeyContext::CommentEditor]
            }
        }
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

fn filter_movement_action_for(key: &KeyEvent) -> Option<Action> {
    if !key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
        return None;
    }
    match key.code {
        KeyCode::Down => Some(Action::TargetPickerMoveDown),
        KeyCode::Up => Some(Action::TargetPickerMoveUp),
        KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(Action::TargetPickerMoveDown)
        }
        KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(Action::TargetPickerMoveUp)
        }
        _ => None,
    }
}

fn popup_movement_action_for(key: &KeyEvent) -> Option<Action> {
    if !key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
        return None;
    }
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => Some(Action::PopupMoveDown),
        KeyCode::Char('k') | KeyCode::Up => Some(Action::PopupMoveUp),
        _ => None,
    }
}

fn popup_safety_action_for(key: &KeyEvent) -> Option<Action> {
    if !key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
        return None;
    }
    match key.code {
        KeyCode::Enter => Some(Action::PopupSelect),
        KeyCode::Esc => Some(Action::PopupClose),
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

fn validate_collisions(bindings: &[KeyBinding]) -> Result<()> {
    for (index, left) in bindings.iter().enumerate() {
        for right in &bindings[index + 1..] {
            if left.key.code != right.key.code || left.key.modifiers != right.key.modifiers {
                continue;
            }
            let Some(context) = left
                .action
                .contexts()
                .iter()
                .find(|context| right.action.contexts().contains(context))
            else {
                continue;
            };
            bail!(
                "duplicate keybinding `{}` (canonical `{}`) for {:?} and {:?} in {} context",
                right.key.label,
                left.key.canonical_label(),
                left.action,
                right.action,
                context.label(),
            );
        }
    }
    Ok(())
}

impl KeyContext {
    fn label(self) -> &'static str {
        match self {
            Self::NormalFiles => "normal/files",
            Self::NormalDiff => "normal/diff",
            Self::Help => "help",
            Self::TargetChooser => "target chooser",
            Self::RevsetInput => "revset input",
            Self::OperationPicker => "operation picker",
            Self::JjHelpers => "jj helpers",
            Self::FlagList => "flag list",
            Self::OpenWork => "open work",
            Self::Activity => "activity",
            Self::WalkthroughList => "walkthrough list",
            Self::DraftList => "draft list",
            Self::FileSearch => "file search",
            Self::SymbolOutline => "symbol outline",
            Self::CommentList => "comment list",
            Self::ViewOptions => "view options",
            Self::CommentEditor => "comment editor",
            Self::ZenFocus => "zen focus",
            Self::ZenGlance => "zen glance",
            Self::ZenArtifact => "zen artifact",
        }
    }
}

impl KeyPress {
    fn canonical_label(&self) -> String {
        let mut parts = Vec::new();
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            parts.push("ctrl".to_owned());
        }
        if self.modifiers.contains(KeyModifiers::ALT) {
            parts.push("alt".to_owned());
        }
        if self.modifiers.contains(KeyModifiers::SHIFT) {
            parts.push("shift".to_owned());
        }
        parts.push(match self.code {
            KeyCode::Char(' ') => "space".to_owned(),
            KeyCode::Char(ch) => ch.to_string(),
            KeyCode::Esc => "esc".to_owned(),
            KeyCode::Enter => "enter".to_owned(),
            KeyCode::Tab => "tab".to_owned(),
            KeyCode::Backspace => "backspace".to_owned(),
            KeyCode::Up => "up".to_owned(),
            KeyCode::Down => "down".to_owned(),
            KeyCode::Left => "left".to_owned(),
            KeyCode::Right => "right".to_owned(),
            KeyCode::PageUp => "pageup".to_owned(),
            KeyCode::PageDown => "pagedown".to_owned(),
            _ => format!("{:?}", self.code).to_ascii_lowercase(),
        });
        parts.join("-")
    }
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
        assert_eq!(
            keymap.action_for(&KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT)),
            Some(Action::ScrollDiffLeft)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT)),
            Some(Action::ScrollDiffRight)
        );
        // Direct cue toggles ship unbound but stay bindable via config.
        let config = KeybindingsConfig {
            toggle_word_highlight: vec!["alt-h".to_owned()],
            toggle_diff_wrap: vec!["alt-w".to_owned()],
            ..KeybindingsConfig::default()
        };
        let keymap = KeyMap::try_from(&config).unwrap();
        assert_eq!(
            keymap.action_for(&KeyEvent::new(KeyCode::Char('h'), KeyModifiers::ALT)),
            Some(Action::ToggleWordHighlight)
        );
        assert_eq!(
            keymap.action_for(&KeyEvent::new(KeyCode::Char('w'), KeyModifiers::ALT)),
            Some(Action::ToggleDiffWrap)
        );
    }

    #[test]
    fn rejects_unsupported_key_names() {
        assert!(parse_key("hyper-space").is_err());
    }

    #[test]
    fn keymap_validation_reports_invalid_syntax_with_the_raw_key() {
        let config = KeybindingsConfig {
            help: vec!["hyper-space".to_owned()],
            ..KeybindingsConfig::default()
        };

        let error = KeyMap::try_from(&config).unwrap_err().to_string();
        assert!(error.contains("unsupported keybinding `hyper-space`"));
    }

    #[test]
    fn canonical_key_aliases_compare_equal() {
        assert_eq!(parse_key("esc").unwrap(), parse_key("escape").unwrap());
        assert_eq!(parse_key("enter").unwrap(), parse_key("return").unwrap());
        assert_eq!(
            parse_key("ctrl-j").unwrap(),
            parse_key("control+j").unwrap()
        );
        assert_eq!(parse_key("page-up").unwrap(), parse_key("pageup").unwrap());
    }

    #[test]
    fn default_map_is_collision_free() {
        KeyMap::try_from(&KeybindingsConfig::default()).unwrap();
    }

    #[test]
    fn rejects_overlapping_bindings_after_canonicalization() {
        let config = KeybindingsConfig {
            quit: vec!["escape".to_owned()],
            cancel_range_comment: vec!["esc".to_owned()],
            ..KeybindingsConfig::default()
        };

        let error = KeyMap::try_from(&config).unwrap_err().to_string();
        assert!(error.contains("duplicate keybinding"));
        assert!(error.contains("canonical `esc`"));
        assert!(error.contains("normal/files"));
    }

    #[test]
    fn allows_reuse_across_disjoint_modal_contexts() {
        let config = KeybindingsConfig {
            quit: vec!["q".to_owned()],
            cancel_comment: vec!["q".to_owned()],
            popup_close: vec!["q".to_owned()],
            popup_close_q: Vec::new(),
            ..KeybindingsConfig::default()
        };

        let keymap = KeyMap::try_from(&config).unwrap();
        let q = KeyEvent::from(KeyCode::Char('q'));
        assert_eq!(keymap.action_for(&q), Some(Action::Quit));
        assert_eq!(keymap.comment_action_for(&q), Some(Action::CancelComment));
        assert_eq!(
            keymap.popup_action_for(KeyContext::CommentList, &q),
            Some(Action::PopupClose)
        );
    }

    #[test]
    fn effective_mode_hints_follow_overrides() {
        let config = KeybindingsConfig {
            popup_move_down: vec!["n".to_owned()],
            popup_move_up: vec!["e".to_owned()],
            comment_list_new_general: vec!["g".to_owned()],
            edit_comment: vec!["alt-e".to_owned()],
            draft_edit: vec!["alt-e".to_owned()],
            zen_artifact: vec!["i".to_owned()],
            ..KeybindingsConfig::default()
        };
        let keymap = KeyMap::try_from(&config).unwrap();

        assert_eq!(keymap.hint(Action::PopupMoveDown), "n");
        assert_eq!(keymap.hint(Action::PopupMoveUp), "e");
        assert_eq!(keymap.hint(Action::CommentListNewGeneral), "g");
    }

    #[test]
    fn documented_colemak_mod_dh_override_is_collision_free() {
        let config = KeybindingsConfig {
            move_down: vec!["n".to_owned(), "down".to_owned()],
            move_up: vec!["e".to_owned(), "up".to_owned()],
            next_unviewed: vec!["j".to_owned()],
            previous_unviewed: vec!["J".to_owned()],
            edit_comment: vec!["alt-e".to_owned()],
            popup_move_down: vec!["n".to_owned(), "down".to_owned()],
            popup_move_up: vec!["e".to_owned(), "up".to_owned()],
            comment_list_new_general: vec!["ctrl-n".to_owned()],
            draft_edit: vec!["alt-e".to_owned()],
            zen_artifact: vec!["i".to_owned()],
            ..KeybindingsConfig::default()
        };

        KeyMap::try_from(&config).unwrap();
    }

    #[test]
    fn configured_key_overrides_default_action() {
        let config = KeybindingsConfig {
            move_down: vec!["alt-n".to_owned()],
            ..KeybindingsConfig::default()
        };
        let keymap = KeyMap::try_from(&config).unwrap();

        let key = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT);

        assert_eq!(keymap.action_for(&key), Some(Action::MoveDown));
        assert_eq!(keymap.hint(Action::MoveDown), "alt-n");
    }

    #[test]
    fn movement_fallbacks_keep_j_k_and_arrows_available() {
        let config = KeybindingsConfig {
            move_down: vec!["alt-n".to_owned()],
            move_up: vec!["alt-e".to_owned()],
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
            keymap.filter_action_for(
                KeyContext::TargetChooser,
                &KeyEvent::from(KeyCode::Char('j'))
            ),
            None
        );
        assert_eq!(
            keymap.filter_action_for(
                KeyContext::TargetChooser,
                &KeyEvent::from(KeyCode::Char('k'))
            ),
            None
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
