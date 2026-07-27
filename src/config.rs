use std::{
    env, fs,
    path::{Path, PathBuf},
};

use color_eyre::eyre::{Context, Result, bail};
use serde::Deserialize;

use crate::{
    generated::{GeneratedPolicy, GeneratedPreset},
    state::{AuthorKind, Channel, CommentState, Identity},
    syntax::{SyntaxConfig, SyntaxThemeConfig},
    theme::{BasePalette, Rgb, accepted_theme_names, builtin_theme},
};

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Config {
    pub ignore: IgnoreConfig,
    pub jj: JjConfig,
    pub artifact: ArtifactConfig,
    pub keybindings: KeybindingsConfig,
    pub generated: GeneratedConfig,
    pub syntax: SyntaxConfig,
    pub limits: LimitsConfig,
    pub agent: AgentConfig,
    pub identity: IdentityConfig,
    pub diff: DiffConfig,
    pub comments: CommentsConfig,
    pub theme: ThemeConfig,
    pub ui: UiConfig,
    pub web: WebConfig,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct WebConfig {
    /// Optional local user stylesheet loaded after Gander's embedded theme and
    /// component CSS. This is intentionally the only web asset read from disk.
    pub extra_css: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct UiConfig {
    /// Hide the files pane automatically below this terminal width unless the
    /// user explicitly toggles it visible for the session.
    pub file_pane_auto_hide_width: u16,
    /// Percentage of terminal width used by the files pane on wide terminals.
    pub file_pane_split_percent: u16,
    /// Show a compact top menu built from the effective keymap. Default off.
    pub menu_bar: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            file_pane_auto_hide_width: 50,
            file_pane_split_percent: 30,
            menu_bar: false,
        }
    }
}

/// Derived TUI theme (docs/roadmap.md M19). All chrome colors derive from a
/// light or dark base palette; `[diff.theme]`/`[syntax.theme]` entries stay
/// literal user values on top of it.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct ThemeConfig {
    /// Built-in palette pair name. Accepted names are normalized lowercase with
    /// `_`/space treated as `-`; aliases resolve to their canonical name.
    pub name: String,
    #[serde(skip)]
    pub(crate) syntax_theme_is_explicit: bool,
    /// `auto` (default) detects light/dark from the terminal background via
    /// a one-shot OSC 11 query at TUI startup; `dark`/`light` never query.
    pub mode: ThemeModeConfig,
    /// Leave the terminal's own background visible instead of painting the
    /// palette background (default: true, Gander's historical look).
    pub transparent: bool,
    pub palette: ThemePaletteConfig,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct ThemePaletteConfig {
    pub dark: ThemePaletteOverride,
    pub light: ThemePaletteOverride,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct ThemePaletteOverride {
    pub background: Option<Rgb>,
    pub foreground: Option<Rgb>,
    pub accent: Option<Rgb>,
    pub positive: Option<Rgb>,
    pub negative: Option<Rgb>,
    pub info: Option<Rgb>,
}

impl ThemePaletteOverride {
    pub(crate) fn apply_to(self, palette: &mut BasePalette) {
        if let Some(value) = self.background {
            palette.background = value;
        }
        if let Some(value) = self.foreground {
            palette.foreground = value;
        }
        if let Some(value) = self.accent {
            palette.accent = value;
        }
        if let Some(value) = self.positive {
            palette.positive = value;
        }
        if let Some(value) = self.negative {
            palette.negative = value;
        }
        if let Some(value) = self.info {
            palette.info = value;
        }
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        parse_hex_rgb(&raw).map_err(serde::de::Error::custom)
    }
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            name: "gander".to_owned(),
            syntax_theme_is_explicit: false,
            mode: ThemeModeConfig::default(),
            transparent: true,
            palette: ThemePaletteConfig::default(),
        }
    }
}

impl ThemeConfig {
    pub(crate) fn base_palette(&self, kind: crate::theme::ThemeKind) -> BasePalette {
        let builtin = builtin_theme(&self.name).unwrap_or_else(|| builtin_theme("gander").unwrap());
        let mut palette = BasePalette::from_pair(builtin.palettes, kind);
        match kind {
            crate::theme::ThemeKind::Dark => self.palette.dark.apply_to(&mut palette),
            crate::theme::ThemeKind::Light => self.palette.light.apply_to(&mut palette),
        }
        palette
    }
}

fn parse_hex_rgb(raw: &str) -> std::result::Result<Rgb, String> {
    let value = raw
        .trim()
        .strip_prefix('#')
        .ok_or_else(|| format!("theme palette color {raw:?} must be #rrggbb"))?;
    if value.len() != 6 || !value.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("theme palette color {raw:?} must be #rrggbb"));
    }
    u32::from_str_radix(value, 16)
        .map(Rgb::hex)
        .map_err(|_| format!("theme palette color {raw:?} must be #rrggbb"))
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum NamedThemeConfig {
    #[serde(alias = "gander-dark", alias = "gander-light")]
    Gander,
    #[serde(alias = "mocha", alias = "latte")]
    Catppuccin,
    Gruvbox,
    Solarized,
    Nord,
    TokyoNight,
    Dracula,
    Monochrome,
}

impl NamedThemeConfig {
    pub const ALL: &'static [Self] = &[
        Self::Gander,
        Self::Catppuccin,
        Self::Gruvbox,
        Self::Solarized,
        Self::Nord,
        Self::TokyoNight,
        Self::Dracula,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Gander => "gander",
            Self::Catppuccin => "catppuccin",
            Self::Gruvbox => "gruvbox",
            Self::Solarized => "solarized",
            Self::Nord => "nord",
            Self::TokyoNight => "tokyo-night",
            Self::Dracula => "dracula",
            Self::Monochrome => "monochrome",
        }
    }

    pub const fn aliases(self) -> &'static [&'static str] {
        match self {
            Self::Gander => &["default", "gander-default"],
            Self::Catppuccin => &["catppuccin-mocha", "mocha", "catppuccin-latte", "latte"],
            Self::Gruvbox => &["gruvbox-dark", "gruvbox-light"],
            Self::Solarized => &["solarized-dark", "solarized-light"],
            Self::Nord => &["nordic"],
            Self::TokyoNight => &["tokyonight", "tokyo", "tokyo-night-storm"],
            Self::Dracula => &["dracula-pro"],
            Self::Monochrome => &[],
        }
    }

    pub fn from_canonical(name: &str) -> Option<Self> {
        match name {
            "gander" => Some(Self::Gander),
            "catppuccin" => Some(Self::Catppuccin),
            "gruvbox" => Some(Self::Gruvbox),
            "solarized" => Some(Self::Solarized),
            "nord" => Some(Self::Nord),
            "tokyo-night" => Some(Self::TokyoNight),
            "dracula" => Some(Self::Dracula),
            "monochrome" => Some(Self::Monochrome),
            _ => None,
        }
    }

    pub const fn syntax_default_name(self, mode: ThemeModeConfig) -> Option<&'static str> {
        match self.syntax_theme_kind(mode) {
            Some(SyntaxThemeKind::GanderDark) => Some("gander-dark"),
            Some(SyntaxThemeKind::GanderLight) => Some("gander-light"),
            Some(SyntaxThemeKind::Monochrome) => Some("monochrome"),
            None => None,
        }
    }

    pub const fn syntax_default_dark(self) -> Option<&'static str> {
        self.syntax_default_name(ThemeModeConfig::Dark)
    }

    pub const fn syntax_default_light(self) -> Option<&'static str> {
        self.syntax_default_name(ThemeModeConfig::Light)
    }

    pub fn syntax_theme(self, mode: ThemeModeConfig) -> Option<SyntaxThemeConfig> {
        match self.syntax_theme_kind(mode) {
            Some(SyntaxThemeKind::GanderDark) => Some(SyntaxThemeConfig::gander_dark()),
            Some(SyntaxThemeKind::GanderLight) => Some(SyntaxThemeConfig::gander_light()),
            Some(SyntaxThemeKind::Monochrome) => Some(SyntaxThemeConfig::monochrome()),
            None => None,
        }
    }

    const fn syntax_theme_kind(self, mode: ThemeModeConfig) -> Option<SyntaxThemeKind> {
        let light = matches!(mode, ThemeModeConfig::Light);
        match self {
            Self::Gander | Self::Solarized | Self::Gruvbox => Some(if light {
                SyntaxThemeKind::GanderLight
            } else {
                SyntaxThemeKind::GanderDark
            }),
            Self::Catppuccin | Self::Nord | Self::TokyoNight | Self::Dracula => {
                Some(SyntaxThemeKind::GanderDark)
            }
            Self::Monochrome => Some(SyntaxThemeKind::Monochrome),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyntaxThemeKind {
    GanderDark,
    GanderLight,
    Monochrome,
}

/// Light/dark selection for the derived theme.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeModeConfig {
    /// Detect from the terminal background (OSC 11); falls back to dark.
    #[default]
    Auto,
    Dark,
    Light,
}

/// Defaults for new durable comments. This deliberately cannot represent
/// `resolved`: closed history is never a valid creation state.
#[derive(Debug, Clone, Copy, Default, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum InitialCommentState {
    Draft,
    #[default]
    Todo,
}

impl From<InitialCommentState> for CommentState {
    fn from(value: InitialCommentState) -> Self {
        match value {
            InitialCommentState::Draft => Self::Draft,
            InitialCommentState::Todo => Self::Todo,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct CommentsConfig {
    pub initial_state: InitialCommentState,
    /// Pin the initial channel instead of applying contextual inference.
    pub default_channel: Option<Channel>,
}

/// Diff-pane visual cues. All runtime-toggleable from the view options
/// popup; config sets the session defaults (docs/focused-diff-ux.md).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct DiffConfig {
    /// Emphasize changed tokens within modified line pairs.
    pub word_highlight: bool,
    /// Tint the background of added/removed lines.
    pub line_background: bool,
    /// Show a colored `▎` marker in the gutter of changed lines.
    pub gutter_bar: bool,
    /// Diff layout: unified (default) or side-by-side removed/added panes.
    /// Side-by-side falls back to unified on narrow terminals.
    pub view: DiffViewModeConfig,
    /// Soft-wrap diff text instead of requiring horizontal scrolling.
    pub soft_wrap: bool,
    /// Lines revealed per press when expanding hidden hunk context (`+`).
    pub context_step: usize,
    pub theme: DiffThemeConfig,
}

/// Diff pane layout mode.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DiffViewModeConfig {
    #[default]
    Unified,
    SideBySide,
}

impl Default for DiffConfig {
    fn default() -> Self {
        Self {
            word_highlight: true,
            line_background: true,
            gutter_bar: false,
            view: DiffViewModeConfig::default(),
            soft_wrap: true,
            context_step: 10,
            theme: DiffThemeConfig::default(),
        }
    }
}

/// Style specs for the diff cues. Specs use the same grammar as syntax
/// theme entries (colors by name, `22`-style indexed values, `#rrggbb`,
/// modifiers, and `on <color>` for backgrounds).
///
/// Every field defaults to `None`, meaning the value derives from the
/// `[theme]` base palette (contrast-guarded for the active light/dark
/// background). Explicitly configured specs are literal user values: they
/// render exactly as written (quantized to xterm-256 on terminals without
/// truecolor) and are never reinterpreted by the derived theme.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct DiffThemeConfig {
    /// Background color for added lines (color spec, background applied).
    pub added_line_bg: Option<String>,
    /// Background color for removed lines.
    pub removed_line_bg: Option<String>,
    /// Style patched onto emphasized (changed) tokens on added lines.
    pub added_word: Option<String>,
    /// Style patched onto emphasized tokens on removed lines.
    pub removed_word: Option<String>,
    /// Gutter bar style for added lines.
    pub gutter_added: Option<String>,
    /// Gutter bar style for removed lines.
    pub gutter_removed: Option<String>,
}

/// Agent identity configuration. Gander never spawns or owns agent
/// processes (docs/decisions.md D9): harnesses drive gander from the
/// outside via CLI/MCP/ACP. This section only names the agent identity
/// stamped on agent-authored annotations.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct AgentConfig {
    /// Name stamped on agent-authored annotations.
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct IdentityConfig {
    /// Name stamped on local human-authored annotations.
    pub name: Option<String>,
    /// Optional email compared against jj change authors during channel
    /// inference, so benign display-name drift still counts as ownership.
    pub email: Option<String>,
}

impl Config {
    pub fn human_identity(&self) -> Identity {
        configured_identity(AuthorKind::Human, self.identity.name.as_deref())
            .unwrap_or_else(Identity::local_human)
    }

    pub fn agent_identity(&self) -> Identity {
        configured_identity(AuthorKind::Agent, self.agent.name.as_deref())
            .unwrap_or_else(Identity::agent)
    }
}

fn configured_identity(kind: AuthorKind, name: Option<&str>) -> Option<Identity> {
    let name = name?.trim();
    if name.is_empty() {
        return None;
    }
    Some(Identity {
        kind,
        name: name.to_owned(),
    })
}

/// Size thresholds that keep huge inputs from overwhelming the TUI.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct LimitsConfig {
    /// Diffs with more lines than this render as a placeholder until
    /// explicitly expanded.
    pub max_diff_lines: usize,
    /// Reviews with at least this many changed lines trigger the
    /// large-change nudge suggesting an agent organize the review.
    /// 0 disables the line criterion.
    pub nudge_diff_lines: usize,
    /// Reviews with at least this many changed files trigger the
    /// large-change nudge. 0 disables the file criterion.
    pub nudge_files: usize,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_diff_lines: 5000,
            nudge_diff_lines: 1000,
            nudge_files: 25,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct JjConfig {
    pub binary: PathBuf,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct IgnoreConfig {
    pub globs: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct GeneratedConfig {
    pub presets: Vec<GeneratedPreset>,
    pub globs: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ArtifactConfig {
    pub format: ArtifactFormatConfig,
    pub profile: ArtifactProfileConfig,
    /// Directory for written artifacts. `None` (the default) means artifacts
    /// go to stdout unless an explicit output path is given; gander no
    /// longer drops files into the project directory by default.
    pub output_dir: Option<PathBuf>,
    pub basename: String,
    #[serde(alias = "on-tui-quit")]
    pub on_tui_quit: TuiArtifactOnQuitConfig,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct KeybindingsConfig {
    pub preset: KeybindingPresetConfig,
    pub quit: Vec<String>,
    pub help: Vec<String>,
    pub yank_handoff: Vec<String>,
    pub move_down: Vec<String>,
    pub move_up: Vec<String>,
    pub toggle_focus: Vec<String>,
    pub diff_top: Vec<String>,
    pub diff_bottom: Vec<String>,
    pub compare_trunk: Vec<String>,
    pub compare_parent: Vec<String>,
    pub target_chooser: Vec<String>,
    pub revset_input: Vec<String>,
    pub stack_next: Vec<String>,
    pub stack_previous: Vec<String>,
    pub operation_picker: Vec<String>,
    pub jj_helpers: Vec<String>,
    pub toggle_large_diff: Vec<String>,
    pub toggle_agent_order: Vec<String>,
    pub flag_list: Vec<String>,
    /// Open work popup. `task-list` remains a deserialization alias for
    /// configurations written before durable action items were renamed.
    #[serde(alias = "task-list")]
    pub open_work: Vec<String>,
    pub activity: Vec<String>,
    pub walkthrough_list: Vec<String>,
    pub draft_list: Vec<String>,
    /// Movement in text-filter popups. Literal `j`/`k` remain text input;
    /// arrows and Ctrl-J/Ctrl-K are permanent safety bindings.
    pub target_picker_down: Vec<String>,
    pub target_picker_up: Vec<String>,
    /// Shared controls for non-text list popups.
    pub popup_move_down: Vec<String>,
    pub popup_move_up: Vec<String>,
    pub popup_select: Vec<String>,
    pub popup_toggle: Vec<String>,
    pub popup_close: Vec<String>,
    /// Secondary close key only for help and View Options,
    /// where `q` was already established before popup bindings were unified.
    pub popup_close_q: Vec<String>,
    pub next_unviewed: Vec<String>,
    pub previous_unviewed: Vec<String>,
    pub next_comment: Vec<String>,
    pub previous_comment: Vec<String>,
    pub file_search: Vec<String>,
    pub symbol_outline: Vec<String>,
    pub next_symbol: Vec<String>,
    pub previous_symbol: Vec<String>,
    pub next_changed_hunk: Vec<String>,
    pub previous_changed_hunk: Vec<String>,
    pub next_file: Vec<String>,
    pub previous_file: Vec<String>,
    pub attention_promote: Vec<String>,
    pub attention_demote: Vec<String>,
    pub attention_focus: Vec<String>,
    pub attention_glance: Vec<String>,
    pub glance_peek: Vec<String>,
    pub glance_acknowledge: Vec<String>,
    pub glance_acknowledge_all: Vec<String>,
    pub spotlight_next: Vec<String>,
    pub spotlight_previous: Vec<String>,
    pub advance_review: Vec<String>,
    pub scroll_down: Vec<String>,
    pub scroll_up: Vec<String>,
    pub scroll_diff_left: Vec<String>,
    pub scroll_diff_right: Vec<String>,
    pub mark_viewed: Vec<String>,
    pub toggle_viewed: Vec<String>,
    pub mark_all_viewed: Vec<String>,
    pub toggle_generated: Vec<String>,
    pub cycle_viewed_filter: Vec<String>,
    pub toggle_fold: Vec<String>,
    pub collapse_fold: Vec<String>,
    pub expand_fold: Vec<String>,
    pub toggle_context_fold: Vec<String>,
    pub expand_context: Vec<String>,
    pub expand_context_all: Vec<String>,
    pub collapse_context: Vec<String>,
    pub view_options: Vec<String>,
    pub toggle_word_highlight: Vec<String>,
    pub toggle_line_background: Vec<String>,
    pub toggle_gutter_bar: Vec<String>,
    pub toggle_diff_wrap: Vec<String>,
    pub toggle_annotation_artifacts: Vec<String>,
    pub toggle_file_pane: Vec<String>,
    pub toggle_diff_view: Vec<String>,
    pub widen_file_pane: Vec<String>,
    pub narrow_file_pane: Vec<String>,
    pub range_comment: Vec<String>,
    pub mark_walkthrough: Vec<String>,
    pub cancel_range_comment: Vec<String>,
    pub comment: Vec<String>,
    pub cycle_comment_state: Vec<String>,
    pub edit_comment: Vec<String>,
    pub delete_comment: Vec<String>,
    pub comment_list: Vec<String>,
    pub comment_list_new_general: Vec<String>,
    pub comment_list_ready: Vec<String>,
    pub comment_list_cycle_action: Vec<String>,
    pub comment_list_cycle_kind: Vec<String>,
    pub draft_accept: Vec<String>,
    pub draft_edit: Vec<String>,
    pub draft_discard: Vec<String>,
    pub walkthrough_delete: Vec<String>,
    pub walkthrough_move_down: Vec<String>,
    pub walkthrough_move_up: Vec<String>,
    pub submit_comment: Vec<String>,
    pub cancel_comment: Vec<String>,
    pub insert_newline: Vec<String>,
    pub delete_char: Vec<String>,
    pub cycle_comment_channel: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum KeybindingPresetConfig {
    #[default]
    Gander,
    Hunk,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactFormatConfig {
    Json,
    Markdown,
    Html,
}

/// Artifact audience: agent adds raw hunks and comment excerpts to JSON.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactProfileConfig {
    #[default]
    Human,
    Agent,
    Team,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TuiArtifactOnQuitConfig {
    Never,
    Write,
    Stdout,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct ConfigPatch {
    ignore: IgnoreConfigPatch,
    jj: JjConfigPatch,
    artifact: ArtifactConfigPatch,
    keybindings: KeybindingsConfigPatch,
    generated: GeneratedConfigPatch,
    syntax: Option<SyntaxConfig>,
    limits: LimitsConfigPatch,
    agent: AgentConfigPatch,
    identity: IdentityConfigPatch,
    diff: DiffConfigPatch,
    comments: CommentsConfigPatch,
    theme: ThemeConfigPatch,
    ui: UiConfigPatch,
    #[serde(skip)]
    syntax_theme_overridden: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
struct UiConfigPatch {
    file_pane_auto_hide_width: Option<u16>,
    file_pane_split_percent: Option<u16>,
    menu_bar: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
struct ThemeConfigPatch {
    name: Option<String>,
    mode: Option<ThemeModeConfig>,
    transparent: Option<bool>,
    palette: ThemePaletteConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct CommentsConfigPatch {
    initial_state: Option<InitialCommentState>,
    default_channel: Option<Channel>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct DiffConfigPatch {
    word_highlight: Option<bool>,
    line_background: Option<bool>,
    gutter_bar: Option<bool>,
    view: Option<DiffViewModeConfig>,
    soft_wrap: Option<bool>,
    context_step: Option<usize>,
    theme: DiffThemeConfigPatch,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct DiffThemeConfigPatch {
    added_line_bg: Option<String>,
    removed_line_bg: Option<String>,
    added_word: Option<String>,
    removed_word: Option<String>,
    gutter_added: Option<String>,
    gutter_removed: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct AgentConfigPatch {
    name: Option<String>,
    /// Removed spawn fields (docs/decisions.md D9). Still parsed so old
    /// configs fail loudly with a migration message instead of silently
    /// no longer launching anything.
    command: Option<toml::Value>,
    autostart: Option<toml::Value>,
    prompt: Option<toml::Value>,
}

impl AgentConfigPatch {
    /// The first removed `[agent]` spawn field present, for the migration
    /// error.
    fn removed_field(&self) -> Option<&'static str> {
        if self.command.is_some() {
            Some("command")
        } else if self.autostart.is_some() {
            Some("autostart")
        } else if self.prompt.is_some() {
            Some("prompt")
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct IdentityConfigPatch {
    name: Option<String>,
    email: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct LimitsConfigPatch {
    max_diff_lines: Option<usize>,
    nudge_diff_lines: Option<usize>,
    nudge_files: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct JjConfigPatch {
    binary: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct IgnoreConfigPatch {
    globs: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct GeneratedConfigPatch {
    presets: Option<Vec<GeneratedPreset>>,
    globs: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct ArtifactConfigPatch {
    format: Option<ArtifactFormatConfig>,
    profile: Option<ArtifactProfileConfig>,
    output_dir: Option<PathBuf>,
    basename: Option<String>,
    on_tui_quit: Option<TuiArtifactOnQuitConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
struct KeybindingsConfigPatch {
    preset: Option<KeybindingPresetConfig>,
    quit: Option<Vec<String>>,
    help: Option<Vec<String>>,
    yank_handoff: Option<Vec<String>>,
    move_down: Option<Vec<String>>,
    move_up: Option<Vec<String>>,
    toggle_focus: Option<Vec<String>>,
    diff_top: Option<Vec<String>>,
    diff_bottom: Option<Vec<String>>,
    compare_trunk: Option<Vec<String>>,
    compare_parent: Option<Vec<String>>,
    target_chooser: Option<Vec<String>>,
    revset_input: Option<Vec<String>>,
    stack_next: Option<Vec<String>>,
    stack_previous: Option<Vec<String>>,
    operation_picker: Option<Vec<String>>,
    jj_helpers: Option<Vec<String>>,
    toggle_large_diff: Option<Vec<String>>,
    toggle_agent_order: Option<Vec<String>>,
    flag_list: Option<Vec<String>>,
    #[serde(alias = "task-list")]
    open_work: Option<Vec<String>>,
    activity: Option<Vec<String>>,
    walkthrough_list: Option<Vec<String>>,
    draft_list: Option<Vec<String>>,
    target_picker_down: Option<Vec<String>>,
    target_picker_up: Option<Vec<String>>,
    popup_move_down: Option<Vec<String>>,
    popup_move_up: Option<Vec<String>>,
    popup_select: Option<Vec<String>>,
    popup_toggle: Option<Vec<String>>,
    popup_close: Option<Vec<String>>,
    popup_close_q: Option<Vec<String>>,
    next_unviewed: Option<Vec<String>>,
    previous_unviewed: Option<Vec<String>>,
    next_comment: Option<Vec<String>>,
    previous_comment: Option<Vec<String>>,
    file_search: Option<Vec<String>>,
    symbol_outline: Option<Vec<String>>,
    next_symbol: Option<Vec<String>>,
    previous_symbol: Option<Vec<String>>,
    next_changed_hunk: Option<Vec<String>>,
    previous_changed_hunk: Option<Vec<String>>,
    next_file: Option<Vec<String>>,
    previous_file: Option<Vec<String>>,
    attention_promote: Option<Vec<String>>,
    attention_demote: Option<Vec<String>>,
    attention_focus: Option<Vec<String>>,
    attention_glance: Option<Vec<String>>,
    glance_peek: Option<Vec<String>>,
    glance_acknowledge: Option<Vec<String>>,
    glance_acknowledge_all: Option<Vec<String>>,
    spotlight_next: Option<Vec<String>>,
    spotlight_previous: Option<Vec<String>>,
    advance_review: Option<Vec<String>>,
    scroll_down: Option<Vec<String>>,
    scroll_up: Option<Vec<String>>,
    scroll_diff_left: Option<Vec<String>>,
    scroll_diff_right: Option<Vec<String>>,
    mark_viewed: Option<Vec<String>>,
    toggle_viewed: Option<Vec<String>>,
    mark_all_viewed: Option<Vec<String>>,
    toggle_generated: Option<Vec<String>>,
    cycle_viewed_filter: Option<Vec<String>>,
    toggle_fold: Option<Vec<String>>,
    collapse_fold: Option<Vec<String>>,
    expand_fold: Option<Vec<String>>,
    toggle_context_fold: Option<Vec<String>>,
    expand_context: Option<Vec<String>>,
    expand_context_all: Option<Vec<String>>,
    collapse_context: Option<Vec<String>>,
    view_options: Option<Vec<String>>,
    toggle_word_highlight: Option<Vec<String>>,
    toggle_line_background: Option<Vec<String>>,
    toggle_gutter_bar: Option<Vec<String>>,
    toggle_diff_wrap: Option<Vec<String>>,
    toggle_annotation_artifacts: Option<Vec<String>>,
    toggle_file_pane: Option<Vec<String>>,
    toggle_diff_view: Option<Vec<String>>,
    widen_file_pane: Option<Vec<String>>,
    narrow_file_pane: Option<Vec<String>>,
    range_comment: Option<Vec<String>>,
    mark_walkthrough: Option<Vec<String>>,
    cancel_range_comment: Option<Vec<String>>,
    comment: Option<Vec<String>>,
    cycle_comment_state: Option<Vec<String>>,
    edit_comment: Option<Vec<String>>,
    delete_comment: Option<Vec<String>>,
    comment_list: Option<Vec<String>>,
    comment_list_new_general: Option<Vec<String>>,
    comment_list_ready: Option<Vec<String>>,
    comment_list_cycle_action: Option<Vec<String>>,
    comment_list_cycle_kind: Option<Vec<String>>,
    draft_accept: Option<Vec<String>>,
    draft_edit: Option<Vec<String>>,
    draft_discard: Option<Vec<String>>,
    walkthrough_delete: Option<Vec<String>>,
    walkthrough_move_down: Option<Vec<String>>,
    walkthrough_move_up: Option<Vec<String>>,
    submit_comment: Option<Vec<String>>,
    cancel_comment: Option<Vec<String>>,
    insert_newline: Option<Vec<String>>,
    delete_char: Option<Vec<String>>,
    cycle_comment_channel: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
struct ConfigSource {
    path: PathBuf,
    required: bool,
}

impl Default for ArtifactConfig {
    fn default() -> Self {
        Self {
            format: ArtifactFormatConfig::Markdown,
            profile: ArtifactProfileConfig::default(),
            output_dir: None,
            basename: "review".to_owned(),
            on_tui_quit: TuiArtifactOnQuitConfig::Never,
        }
    }
}

impl Default for JjConfig {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("jj"),
        }
    }
}

impl Default for KeybindingsConfig {
    fn default() -> Self {
        Self::preset(KeybindingPresetConfig::Gander)
    }
}

impl KeybindingsConfig {
    pub(crate) fn preset(preset: KeybindingPresetConfig) -> Self {
        let mut config = Self {
            preset,
            // Esc intentionally does not quit: it dismisses the current layer
            // (range selection, notices, popups) so it stays safe to mash.
            quit: keys(["q"]),
            help: keys(["?"]),
            yank_handoff: keys(["ctrl-y"]),
            move_down: keys(["j", "down"]),
            move_up: keys(["k", "up"]),
            toggle_focus: keys(["tab"]),
            diff_top: keys(["g"]),
            diff_bottom: keys(["G"]),
            compare_trunk: keys(["t"]),
            compare_parent: keys(["p"]),
            target_chooser: keys(["b"]),
            revset_input: keys(["R"]),
            stack_next: keys([">"]),
            stack_previous: keys(["<"]),
            operation_picker: keys(["I"]),
            jj_helpers: keys(["!"]),
            toggle_large_diff: keys(["L"]),
            toggle_agent_order: keys(["A"]),
            flag_list: keys(["F"]),
            open_work: keys(["X"]),
            activity: keys(["ctrl-a"]),
            walkthrough_list: keys(["W"]),
            draft_list: keys(["D"]),
            target_picker_down: keys(["down", "ctrl-j"]),
            target_picker_up: keys(["up", "ctrl-k"]),
            popup_move_down: keys(["j", "down"]),
            popup_move_up: keys(["k", "up"]),
            popup_select: keys(["enter"]),
            popup_toggle: keys(["space"]),
            popup_close: keys(["esc"]),
            popup_close_q: keys(["q"]),
            next_unviewed: keys(["n"]),
            previous_unviewed: keys(["N"]),
            next_comment: keys(["m"]),
            previous_comment: keys(["M"]),
            file_search: keys(["/"]),
            symbol_outline: keys(["o"]),
            next_symbol: keys(["}"]),
            previous_symbol: keys(["{"]),
            next_changed_hunk: keys(["]"]),
            previous_changed_hunk: keys(["["]),
            next_file: keys(["."]),
            previous_file: keys([","]),
            attention_promote: keys(["alt-up"]),
            attention_demote: keys(["alt-down"]),
            attention_focus: keys(["Z"]),
            attention_glance: keys(["alt-g"]),
            glance_peek: keys(["space"]),
            glance_acknowledge: keys(["a"]),
            glance_acknowledge_all: keys(["A"]),
            spotlight_next: keys(["alt-n"]),
            spotlight_previous: keys(["alt-p"]),
            advance_review: keys(["enter"]),
            scroll_down: keys(["d", "pagedown"]),
            scroll_up: keys(["u", "pageup"]),
            scroll_diff_left: keys(["shift-left"]),
            scroll_diff_right: keys(["shift-right"]),
            mark_viewed: keys(["enter"]),
            toggle_viewed: keys(["v"]),
            mark_all_viewed: keys(["a"]),
            toggle_generated: keys(["h"]),
            cycle_viewed_filter: keys(["f"]),
            toggle_fold: keys(["space"]),
            collapse_fold: keys(["left"]),
            expand_fold: keys(["right"]),
            toggle_context_fold: keys(["z"]),
            expand_context: keys(["+"]),
            expand_context_all: keys(["="]),
            collapse_context: keys(["-"]),
            view_options: keys(["V"]),
            // Direct toggle keys ship unbound; the view options popup (V)
            // covers them and users can bind keys via [keybindings].
            toggle_word_highlight: keys([]),
            toggle_line_background: keys([]),
            toggle_gutter_bar: keys([]),
            toggle_diff_wrap: keys([]),
            toggle_annotation_artifacts: keys(["E"]),
            toggle_file_pane: keys(["w"]),
            toggle_diff_view: keys(["|"]),
            widen_file_pane: keys(["alt-right"]),
            narrow_file_pane: keys(["alt-left"]),
            range_comment: keys(["r"]),
            mark_walkthrough: keys(["Y"]),
            cancel_range_comment: keys(["ctrl-g", "esc"]),
            comment: keys(["c"]),
            cycle_comment_state: keys(["s"]),
            edit_comment: keys(["e"]),
            delete_comment: keys(["x"]),
            comment_list: keys(["C"]),
            comment_list_new_general: keys(["n"]),
            comment_list_ready: keys(["R"]),
            comment_list_cycle_action: keys(["a"]),
            comment_list_cycle_kind: keys(["K"]),
            draft_accept: keys(["enter", "a"]),
            draft_edit: keys(["e"]),
            draft_discard: keys(["x"]),
            walkthrough_delete: keys(["d"]),
            walkthrough_move_down: keys(["J"]),
            walkthrough_move_up: keys(["K"]),
            submit_comment: keys(["ctrl-s"]),
            cancel_comment: keys(["esc"]),
            insert_newline: keys(["enter"]),
            delete_char: keys(["backspace"]),
            cycle_comment_channel: keys(["tab"]),
        };
        if preset == KeybindingPresetConfig::Hunk {
            config.scroll_down = keys(["space", "f", "pagedown", "d"]);
            config.scroll_up = keys(["b", "pageup", "u"]);
            config.target_chooser = keys(["alt-b"]);
            config.next_comment = keys(["}", "m"]);
            config.previous_comment = keys(["{", "M"]);
            config.next_symbol = keys(["alt-]"]);
            config.previous_symbol = keys(["alt-["]);
            config.next_changed_hunk = keys(["alt-j", "]"]);
            config.previous_changed_hunk = keys(["alt-k", "["]);
            config.next_file = keys(["alt-l", "."]);
            config.previous_file = keys(["alt-h", ","]);
            config.cycle_viewed_filter = keys(["alt-f"]);
            config.toggle_fold = keys([]);
            config.toggle_diff_wrap = keys(["w"]);
            config.toggle_file_pane = keys(["s"]);
            config.cycle_comment_state = keys(["S"]);
        }
        config
    }
}

fn keys<const N: usize>(keys: [&str; N]) -> Vec<String> {
    keys.into_iter().map(str::to_owned).collect()
}

impl Config {
    pub fn load(repo: &Path, explicit_path: Option<&Path>) -> Result<Self> {
        let mut sources = Vec::new();
        if let Some(path) = xdg_config_path() {
            sources.push(ConfigSource {
                path,
                required: false,
            });
        }
        sources.push(ConfigSource {
            path: repo.join("gander.toml"),
            required: false,
        });
        // Deprecated layer (docs/decisions.md D6): still loads for one
        // release; main prints a warning when it exists.
        sources.push(ConfigSource {
            path: repo.join(".gander").join("config.toml"),
            required: false,
        });
        if let Some(path) = explicit_path {
            sources.push(ConfigSource {
                path: path.to_path_buf(),
                required: true,
            });
        }

        Self::load_layers(&sources)
    }

    fn load_layers(sources: &[ConfigSource]) -> Result<Self> {
        let mut config = Self::default();
        let mut keybinding_patches = Vec::new();
        for source in sources {
            if !source.path.exists() {
                if source.required {
                    fs::read_to_string(&source.path).with_context(|| {
                        format!("failed to read config {}", source.path.display())
                    })?;
                }
                continue;
            }

            let contents = fs::read_to_string(&source.path)
                .with_context(|| format!("failed to read config {}", source.path.display()))?;
            let syntax_theme_overridden = contents
                .parse::<toml::Value>()
                .ok()
                .and_then(|value| {
                    value
                        .get("syntax")
                        .and_then(|syntax| syntax.get("theme"))
                        .cloned()
                })
                .is_some();
            let mut patch: ConfigPatch = toml::from_str(&contents)
                .with_context(|| format!("failed to parse config {}", source.path.display()))?;
            patch.syntax_theme_overridden = syntax_theme_overridden;
            if let Some(field) = patch.agent.removed_field() {
                bail!(
                    "failed to parse config {}: `[agent] {field}` was removed: gander no \
                     longer spawns agents; run your harness against `gander mcp` or the \
                     CLI instead (see docs/harness-setup.md and docs/decisions.md D9). \
                     `[agent] name` remains supported as annotation identity.",
                    source.path.display()
                );
            }
            keybinding_patches.push(patch.keybindings.clone());
            patch.keybindings = KeybindingsConfigPatch::default();
            config.apply_patch(patch)?;
        }

        let final_preset = keybinding_patches
            .iter()
            .filter_map(|patch| patch.preset)
            .next_back()
            .unwrap_or_default();
        config.keybindings = KeybindingsConfig::preset(final_preset);
        for patch in keybinding_patches {
            config.keybindings.apply_action_patch(patch);
        }

        Ok(config)
    }

    fn apply_patch(&mut self, patch: ConfigPatch) -> Result<()> {
        if let Some(globs) = patch.ignore.globs {
            self.ignore.globs = globs;
        }

        if let Some(binary) = patch.jj.binary {
            self.jj.binary = binary;
        }

        if let Some(format) = patch.artifact.format {
            self.artifact.format = format;
        }
        if let Some(profile) = patch.artifact.profile {
            self.artifact.profile = profile;
        }
        if let Some(output_dir) = patch.artifact.output_dir {
            self.artifact.output_dir = Some(output_dir);
        }
        if let Some(basename) = patch.artifact.basename {
            self.artifact.basename = basename;
        }
        if let Some(on_tui_quit) = patch.artifact.on_tui_quit {
            self.artifact.on_tui_quit = on_tui_quit;
        }

        self.keybindings.apply_patch(patch.keybindings);

        if let Some(presets) = patch.generated.presets {
            self.generated.presets = presets;
        }
        if let Some(globs) = patch.generated.globs {
            self.generated.globs = globs;
        }

        let syntax_override = patch.syntax_theme_overridden;
        if let Some(syntax) = patch.syntax {
            self.syntax = syntax;
            self.theme.syntax_theme_is_explicit = syntax_override;
            if !self.theme.syntax_theme_is_explicit
                && let Some(named) = NamedThemeConfig::from_canonical(&self.theme.name)
                && let Some(syntax_theme) = named.syntax_theme(self.theme.mode)
            {
                self.syntax.theme = syntax_theme;
            }
        }

        if let Some(max_diff_lines) = patch.limits.max_diff_lines {
            self.limits.max_diff_lines = max_diff_lines;
        }
        if let Some(nudge_diff_lines) = patch.limits.nudge_diff_lines {
            self.limits.nudge_diff_lines = nudge_diff_lines;
        }
        if let Some(nudge_files) = patch.limits.nudge_files {
            self.limits.nudge_files = nudge_files;
        }

        if let Some(name) = patch.agent.name {
            self.agent.name = (!name.trim().is_empty()).then(|| name.trim().to_owned());
        }

        if let Some(initial_state) = patch.comments.initial_state {
            self.comments.initial_state = initial_state;
        }
        if let Some(default_channel) = patch.comments.default_channel {
            self.comments.default_channel = Some(default_channel);
        }

        if let Some(name) = patch.identity.name {
            self.identity.name = (!name.trim().is_empty()).then(|| name.trim().to_owned());
        }
        if let Some(email) = patch.identity.email {
            self.identity.email = (!email.trim().is_empty()).then(|| email.trim().to_owned());
        }

        if let Some(width) = patch.ui.file_pane_auto_hide_width {
            self.ui.file_pane_auto_hide_width = width;
        }
        if let Some(percent) = patch.ui.file_pane_split_percent {
            self.ui.file_pane_split_percent = percent.clamp(10, 60);
        }
        if let Some(menu_bar) = patch.ui.menu_bar {
            self.ui.menu_bar = menu_bar;
        }

        if let Some(word_highlight) = patch.diff.word_highlight {
            self.diff.word_highlight = word_highlight;
        }
        if let Some(line_background) = patch.diff.line_background {
            self.diff.line_background = line_background;
        }
        if let Some(gutter_bar) = patch.diff.gutter_bar {
            self.diff.gutter_bar = gutter_bar;
        }
        if let Some(view) = patch.diff.view {
            self.diff.view = view;
        }
        if let Some(soft_wrap) = patch.diff.soft_wrap {
            self.diff.soft_wrap = soft_wrap;
        }
        if let Some(context_step) = patch.diff.context_step {
            self.diff.context_step = context_step;
        }
        apply_some(
            &mut self.diff.theme.added_line_bg,
            patch.diff.theme.added_line_bg,
        );
        apply_some(
            &mut self.diff.theme.removed_line_bg,
            patch.diff.theme.removed_line_bg,
        );
        apply_some(&mut self.diff.theme.added_word, patch.diff.theme.added_word);
        apply_some(
            &mut self.diff.theme.removed_word,
            patch.diff.theme.removed_word,
        );
        apply_some(
            &mut self.diff.theme.gutter_added,
            patch.diff.theme.gutter_added,
        );
        apply_some(
            &mut self.diff.theme.gutter_removed,
            patch.diff.theme.gutter_removed,
        );

        if let Some(mode) = patch.theme.mode {
            self.theme.mode = mode;
            if !self.theme.syntax_theme_is_explicit
                && let Some(named) = NamedThemeConfig::from_canonical(&self.theme.name)
                && let Some(syntax_theme) = named.syntax_theme(self.theme.mode)
            {
                self.syntax.theme = syntax_theme;
            }
        }
        if let Some(name) = patch.theme.name {
            let normalized = crate::theme::normalize_theme_name(&name);
            let Some(builtin) = builtin_theme(&normalized) else {
                bail!(
                    "invalid [theme] name {name:?}; accepted names: {} (aliases: default, gander-default, catppuccin-mocha, catppuccin-latte, gruvbox-dark, gruvbox-light, solarized-dark, solarized-light, nordic, tokyonight, tokyo, tokyo-night-storm, dracula-pro)",
                    accepted_theme_names()
                );
            };
            self.theme.name = builtin.name.to_owned();
            if !self.theme.syntax_theme_is_explicit
                && let Some(named) = NamedThemeConfig::from_canonical(builtin.name)
                && let Some(syntax_theme) = named.syntax_theme(self.theme.mode)
            {
                self.syntax.theme = syntax_theme;
            }
        }
        if let Some(transparent) = patch.theme.transparent {
            self.theme.transparent = transparent;
        }
        self.theme.palette.dark =
            merge_palette_override(self.theme.palette.dark, patch.theme.palette.dark);
        self.theme.palette.light =
            merge_palette_override(self.theme.palette.light, patch.theme.palette.light);
        Ok(())
    }
}

fn merge_palette_override(
    mut base: ThemePaletteOverride,
    patch: ThemePaletteOverride,
) -> ThemePaletteOverride {
    if patch.background.is_some() {
        base.background = patch.background;
    }
    if patch.foreground.is_some() {
        base.foreground = patch.foreground;
    }
    if patch.accent.is_some() {
        base.accent = patch.accent;
    }
    if patch.positive.is_some() {
        base.positive = patch.positive;
    }
    if patch.negative.is_some() {
        base.negative = patch.negative;
    }
    if patch.info.is_some() {
        base.info = patch.info;
    }
    base
}

impl From<GeneratedConfig> for GeneratedPolicy {
    fn from(value: GeneratedConfig) -> Self {
        Self {
            presets: value.presets,
            globs: value.globs,
        }
    }
}

impl KeybindingsConfig {
    fn apply_patch(&mut self, patch: KeybindingsConfigPatch) {
        if let Some(preset) = patch.preset {
            *self = Self::preset(preset);
        }
        self.apply_action_patch(patch);
    }

    fn apply_action_patch(&mut self, patch: KeybindingsConfigPatch) {
        apply_optional(&mut self.quit, patch.quit);
        apply_optional(&mut self.help, patch.help);
        apply_optional(&mut self.yank_handoff, patch.yank_handoff);
        apply_optional(&mut self.move_down, patch.move_down);
        apply_optional(&mut self.move_up, patch.move_up);
        apply_optional(&mut self.toggle_focus, patch.toggle_focus);
        apply_optional(&mut self.diff_top, patch.diff_top);
        apply_optional(&mut self.diff_bottom, patch.diff_bottom);
        apply_optional(&mut self.compare_trunk, patch.compare_trunk);
        apply_optional(&mut self.compare_parent, patch.compare_parent);
        apply_optional(&mut self.target_chooser, patch.target_chooser);
        apply_optional(&mut self.revset_input, patch.revset_input);
        apply_optional(&mut self.stack_next, patch.stack_next);
        apply_optional(&mut self.stack_previous, patch.stack_previous);
        apply_optional(&mut self.operation_picker, patch.operation_picker);
        apply_optional(&mut self.jj_helpers, patch.jj_helpers);
        apply_optional(&mut self.toggle_large_diff, patch.toggle_large_diff);
        apply_optional(&mut self.toggle_agent_order, patch.toggle_agent_order);
        apply_optional(&mut self.flag_list, patch.flag_list);
        apply_optional(&mut self.open_work, patch.open_work);
        apply_optional(&mut self.activity, patch.activity);
        apply_optional(&mut self.walkthrough_list, patch.walkthrough_list);
        apply_optional(&mut self.draft_list, patch.draft_list);
        apply_optional(&mut self.target_picker_down, patch.target_picker_down);
        apply_optional(&mut self.target_picker_up, patch.target_picker_up);
        apply_optional(&mut self.popup_move_down, patch.popup_move_down);
        apply_optional(&mut self.popup_move_up, patch.popup_move_up);
        apply_optional(&mut self.popup_select, patch.popup_select);
        apply_optional(&mut self.popup_toggle, patch.popup_toggle);
        apply_optional(&mut self.popup_close, patch.popup_close);
        apply_optional(&mut self.popup_close_q, patch.popup_close_q);
        apply_optional(&mut self.next_unviewed, patch.next_unviewed);
        apply_optional(&mut self.previous_unviewed, patch.previous_unviewed);
        apply_optional(&mut self.next_comment, patch.next_comment);
        apply_optional(&mut self.previous_comment, patch.previous_comment);
        apply_optional(&mut self.file_search, patch.file_search);
        apply_optional(&mut self.symbol_outline, patch.symbol_outline);
        apply_optional(&mut self.next_symbol, patch.next_symbol);
        apply_optional(&mut self.previous_symbol, patch.previous_symbol);
        apply_optional(&mut self.next_changed_hunk, patch.next_changed_hunk);
        apply_optional(&mut self.previous_changed_hunk, patch.previous_changed_hunk);
        apply_optional(&mut self.next_file, patch.next_file);
        apply_optional(&mut self.previous_file, patch.previous_file);
        apply_optional(&mut self.attention_promote, patch.attention_promote);
        apply_optional(&mut self.attention_demote, patch.attention_demote);
        apply_optional(&mut self.attention_focus, patch.attention_focus);
        apply_optional(&mut self.attention_glance, patch.attention_glance);
        apply_optional(&mut self.glance_peek, patch.glance_peek);
        apply_optional(&mut self.glance_acknowledge, patch.glance_acknowledge);
        apply_optional(
            &mut self.glance_acknowledge_all,
            patch.glance_acknowledge_all,
        );
        apply_optional(&mut self.spotlight_next, patch.spotlight_next);
        apply_optional(&mut self.spotlight_previous, patch.spotlight_previous);
        apply_optional(&mut self.advance_review, patch.advance_review);
        apply_optional(&mut self.scroll_down, patch.scroll_down);
        apply_optional(&mut self.scroll_up, patch.scroll_up);
        apply_optional(&mut self.scroll_diff_left, patch.scroll_diff_left);
        apply_optional(&mut self.scroll_diff_right, patch.scroll_diff_right);
        apply_optional(&mut self.mark_viewed, patch.mark_viewed);
        apply_optional(&mut self.toggle_viewed, patch.toggle_viewed);
        apply_optional(&mut self.mark_all_viewed, patch.mark_all_viewed);
        apply_optional(&mut self.toggle_generated, patch.toggle_generated);
        apply_optional(&mut self.cycle_viewed_filter, patch.cycle_viewed_filter);
        apply_optional(&mut self.toggle_fold, patch.toggle_fold);
        apply_optional(&mut self.collapse_fold, patch.collapse_fold);
        apply_optional(&mut self.expand_fold, patch.expand_fold);
        apply_optional(&mut self.toggle_context_fold, patch.toggle_context_fold);
        apply_optional(&mut self.expand_context, patch.expand_context);
        apply_optional(&mut self.expand_context_all, patch.expand_context_all);
        apply_optional(&mut self.collapse_context, patch.collapse_context);
        apply_optional(&mut self.view_options, patch.view_options);
        apply_optional(&mut self.toggle_word_highlight, patch.toggle_word_highlight);
        apply_optional(
            &mut self.toggle_line_background,
            patch.toggle_line_background,
        );
        apply_optional(&mut self.toggle_gutter_bar, patch.toggle_gutter_bar);
        apply_optional(&mut self.toggle_diff_wrap, patch.toggle_diff_wrap);
        apply_optional(
            &mut self.toggle_annotation_artifacts,
            patch.toggle_annotation_artifacts,
        );
        apply_optional(&mut self.toggle_file_pane, patch.toggle_file_pane);
        apply_optional(&mut self.toggle_diff_view, patch.toggle_diff_view);
        apply_optional(&mut self.widen_file_pane, patch.widen_file_pane);
        apply_optional(&mut self.narrow_file_pane, patch.narrow_file_pane);
        apply_optional(&mut self.range_comment, patch.range_comment);
        apply_optional(&mut self.mark_walkthrough, patch.mark_walkthrough);
        apply_optional(&mut self.cancel_range_comment, patch.cancel_range_comment);
        apply_optional(&mut self.comment, patch.comment);
        apply_optional(&mut self.cycle_comment_state, patch.cycle_comment_state);
        apply_optional(&mut self.edit_comment, patch.edit_comment);
        apply_optional(&mut self.delete_comment, patch.delete_comment);
        apply_optional(&mut self.comment_list, patch.comment_list);
        apply_optional(
            &mut self.comment_list_new_general,
            patch.comment_list_new_general,
        );
        apply_optional(&mut self.comment_list_ready, patch.comment_list_ready);
        apply_optional(
            &mut self.comment_list_cycle_action,
            patch.comment_list_cycle_action,
        );
        apply_optional(
            &mut self.comment_list_cycle_kind,
            patch.comment_list_cycle_kind,
        );
        apply_optional(&mut self.draft_accept, patch.draft_accept);
        apply_optional(&mut self.draft_edit, patch.draft_edit);
        apply_optional(&mut self.draft_discard, patch.draft_discard);
        apply_optional(&mut self.walkthrough_delete, patch.walkthrough_delete);
        apply_optional(&mut self.walkthrough_move_down, patch.walkthrough_move_down);
        apply_optional(&mut self.walkthrough_move_up, patch.walkthrough_move_up);
        apply_optional(&mut self.submit_comment, patch.submit_comment);
        apply_optional(&mut self.cancel_comment, patch.cancel_comment);
        apply_optional(&mut self.insert_newline, patch.insert_newline);
        apply_optional(&mut self.delete_char, patch.delete_char);
        apply_optional(&mut self.cycle_comment_channel, patch.cycle_comment_channel);
    }
}

fn apply_optional<T>(target: &mut T, value: Option<T>) {
    if let Some(value) = value {
        *target = value;
    }
}

/// Layer an optional-by-default field: a later layer's explicit value wins;
/// absence leaves earlier layers (or the derived default) untouched.
fn apply_some<T>(target: &mut Option<T>, value: Option<T>) {
    if let Some(value) = value {
        *target = Some(value);
    }
}

pub fn xdg_config_path() -> Option<PathBuf> {
    if let Some(config_home) = env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(config_home).join("gander/config.toml"));
    }
    env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(|home| PathBuf::from(home).join(".config/gander/config.toml"))
}

impl ArtifactConfig {
    /// Resolved artifact file path, or `None` when no output directory is
    /// configured (artifacts then default to stdout).
    pub fn output_path(&self, repo: &Path, format: ArtifactFormatConfig) -> Option<PathBuf> {
        let output_dir = self.output_dir.as_ref()?;
        let extension = match format {
            ArtifactFormatConfig::Json => "json",
            ArtifactFormatConfig::Markdown => "md",
            ArtifactFormatConfig::Html => "html",
        };
        let output_dir = if output_dir.is_absolute() {
            output_dir.clone()
        } else {
            repo.join(output_dir)
        };
        Some(output_dir.join(format!("{}.{extension}", self.basename)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_default_config_returns_defaults() {
        let config = Config::load_layers(&[]).unwrap();

        assert_eq!(config, Config::default());
    }

    #[test]
    fn named_themes_apply_documented_syntax_defaults() {
        for theme in NamedThemeConfig::ALL {
            for mode in [ThemeModeConfig::Dark, ThemeModeConfig::Light] {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("theme.toml");
                fs::write(
                    &path,
                    format!(
                        "[theme]\nname = \"{}\"\nmode = \"{}\"\n",
                        theme.name(),
                        match mode {
                            ThemeModeConfig::Dark => "dark",
                            ThemeModeConfig::Light => "light",
                            ThemeModeConfig::Auto => "auto",
                        }
                    ),
                )
                .unwrap();

                let config = Config::load_layers(&[ConfigSource {
                    path,
                    required: true,
                }])
                .unwrap();

                assert_eq!(config.theme.name, theme.name());
                assert_eq!(config.syntax.theme, theme.syntax_theme(mode).unwrap());
            }
        }
    }

    #[test]
    fn named_theme_aliases_resolve_to_canonical_palettes() {
        for (alias, canonical) in [
            ("default", NamedThemeConfig::Gander),
            ("gander-default", NamedThemeConfig::Gander),
            ("catppuccin-mocha", NamedThemeConfig::Catppuccin),
            ("mocha", NamedThemeConfig::Catppuccin),
            ("catppuccin-latte", NamedThemeConfig::Catppuccin),
            ("latte", NamedThemeConfig::Catppuccin),
            ("gruvbox-dark", NamedThemeConfig::Gruvbox),
            ("gruvbox-light", NamedThemeConfig::Gruvbox),
            ("solarized-dark", NamedThemeConfig::Solarized),
            ("solarized-light", NamedThemeConfig::Solarized),
            ("nordic", NamedThemeConfig::Nord),
            ("tokyonight", NamedThemeConfig::TokyoNight),
            ("tokyo", NamedThemeConfig::TokyoNight),
            ("tokyo-night-storm", NamedThemeConfig::TokyoNight),
            ("dracula-pro", NamedThemeConfig::Dracula),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("theme.toml");
            fs::write(&path, format!("[theme]\nname = \"{alias}\"\n")).unwrap();

            let config = Config::load_layers(&[ConfigSource {
                path,
                required: true,
            }])
            .unwrap();

            assert_eq!(config.theme.name, canonical.name());
        }
    }

    #[test]
    fn explicit_syntax_theme_wins_over_named_theme_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("theme.toml");
        fs::write(
            &path,
            "[theme]\nname = \"solarized\"\nmode = \"light\"\n\n[syntax.theme]\nkeyword = \"red bold\"\n",
        )
        .unwrap();

        let config = Config::load_layers(&[ConfigSource {
            path,
            required: true,
        }])
        .unwrap();

        assert_eq!(config.theme.name, NamedThemeConfig::Solarized.name());
        assert_eq!(config.syntax.theme.keyword, "red bold");
        assert_ne!(config.syntax.theme, SyntaxThemeConfig::gander_light());
    }

    #[test]
    fn unspecified_later_syntax_config_keeps_inheriting_named_theme_default() {
        let dir = tempfile::tempdir().unwrap();
        let theme = dir.path().join("theme.toml");
        let syntax = dir.path().join("syntax.toml");
        fs::write(&theme, "[theme]\nname = \"gander\"\nmode = \"light\"\n").unwrap();
        fs::write(&syntax, "[syntax]\nenabled = false\n").unwrap();

        let config = Config::load_layers(&[
            ConfigSource {
                path: theme,
                required: true,
            },
            ConfigSource {
                path: syntax,
                required: true,
            },
        ])
        .unwrap();

        assert!(!config.syntax.enabled);
        assert_eq!(config.syntax.theme, SyntaxThemeConfig::gander_light());
    }

    #[test]
    fn explicit_missing_config_errors() {
        let repo = tempfile::tempdir().unwrap();
        let missing = repo.path().join("missing.toml");

        assert!(
            Config::load_layers(&[ConfigSource {
                path: missing,
                required: true,
            }])
            .is_err()
        );
    }

    #[test]
    fn parses_ignore_globs_and_artifact_defaults() {
        let repo = tempfile::tempdir().unwrap();
        let config_path = repo.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
[ignore]
globs = ["Cargo.lock", "**/*.min.js"]

[jj]
binary = "/nix/store/example-jj/bin/jj"

[generated]
presets = ["lockfiles", "api-clients"]
globs = ["schemas/*.json"]

[artifact]
format = "json"
output_dir = "artifacts"
basename = "review-current"
on_tui_quit = "stdout"

[limits]
max-diff-lines = 123

[syntax]
enabled = true
languages = ["rust", "python"]

[[syntax.mappings]]
name = "python"
extensions = ["custompy"]
filenames = ["SConstruct"]

[syntax.theme]
keyword = "red bold"
string = "green italic"

[keybindings]
move-down = ["s", "down"]
quit = ["q"]
toggle-fold = ["f"]
collapse-fold = ["h"]
expand-fold = ["l"]
insert-newline = ["enter"]
submit-comment = ["ctrl-s"]
"#,
        )
        .unwrap();

        let config = Config::load_layers(&[ConfigSource {
            path: config_path,
            required: true,
        }])
        .unwrap();

        assert_eq!(config.ignore.globs, ["Cargo.lock", "**/*.min.js"]);
        assert_eq!(
            config.jj.binary,
            PathBuf::from("/nix/store/example-jj/bin/jj")
        );
        assert_eq!(
            config.generated.presets,
            [GeneratedPreset::Lockfiles, GeneratedPreset::ApiClients]
        );
        assert_eq!(config.generated.globs, ["schemas/*.json"]);
        assert_eq!(config.artifact.format, ArtifactFormatConfig::Json);
        assert_eq!(config.artifact.on_tui_quit, TuiArtifactOnQuitConfig::Stdout);
        assert_eq!(config.limits.max_diff_lines, 123);
        assert!(config.syntax.enabled);
        assert_eq!(config.syntax.languages, ["rust", "python"]);
        assert_eq!(config.syntax.mappings.len(), 1);
        assert_eq!(config.syntax.mappings[0].name, "python");
        assert_eq!(config.syntax.mappings[0].extensions, ["custompy"]);
        assert_eq!(config.syntax.mappings[0].filenames, ["SConstruct"]);
        assert_eq!(config.syntax.theme.keyword, "red bold");
        assert_eq!(config.syntax.theme.string, "green italic");
        assert_eq!(config.keybindings.move_down, ["s", "down"]);
        assert_eq!(config.keybindings.quit, ["q"]);
        assert_eq!(config.keybindings.toggle_fold, ["f"]);
        assert_eq!(config.keybindings.collapse_fold, ["h"]);
        assert_eq!(config.keybindings.expand_fold, ["l"]);
        assert_eq!(config.keybindings.insert_newline, ["enter"]);
        assert_eq!(config.keybindings.submit_comment, ["ctrl-s"]);
        assert_eq!(
            config
                .artifact
                .output_path(repo.path(), ArtifactFormatConfig::Json),
            Some(repo.path().join("artifacts").join("review-current.json"))
        );
    }

    #[test]
    fn output_path_uses_selected_format_extension() {
        let repo = tempfile::tempdir().unwrap();
        let artifact = ArtifactConfig {
            output_dir: Some(PathBuf::from("artifacts")),
            ..ArtifactConfig::default()
        };

        assert_eq!(
            artifact.output_path(repo.path(), ArtifactFormatConfig::Markdown),
            Some(repo.path().join("artifacts").join("review.md"))
        );
        assert_eq!(
            artifact.output_path(repo.path(), ArtifactFormatConfig::Json),
            Some(repo.path().join("artifacts").join("review.json"))
        );
    }

    #[test]
    fn output_path_is_none_without_configured_output_dir() {
        let repo = tempfile::tempdir().unwrap();
        let artifact = ArtifactConfig::default();

        assert_eq!(
            artifact.output_path(repo.path(), ArtifactFormatConfig::Markdown),
            None
        );
    }

    #[test]
    fn agent_config_keeps_name_only() {
        assert_eq!(Config::default().agent, AgentConfig::default());
        assert!(Config::default().agent.name.is_none());

        let repo = tempfile::tempdir().unwrap();
        let config_path = repo.path().join("config.toml");
        fs::write(
            &config_path,
            r#"
[agent]
name = "reviewbot"
"#,
        )
        .unwrap();

        let config = Config::load_layers(&[ConfigSource {
            path: config_path,
            required: true,
        }])
        .unwrap();

        assert_eq!(config.agent.name.as_deref(), Some("reviewbot"));
    }

    #[test]
    fn removed_agent_spawn_fields_error_with_migration_message() {
        // docs/decisions.md D9: gander no longer spawns agents. Old configs
        // must fail loudly rather than silently launching nothing.
        let dir = tempfile::tempdir().unwrap();
        for (field, value) in [
            ("command", "\"opencode run\""),
            ("autostart", "true"),
            ("prompt", "\"review {repo} at {base}..{rev}\""),
        ] {
            let path = dir.path().join(format!("{field}.toml"));
            fs::write(&path, format!("[agent]\n{field} = {value}\n")).unwrap();
            let error = Config::load_layers(&[ConfigSource {
                path,
                required: true,
            }])
            .unwrap_err();
            let message = format!("{error:?}");
            assert!(
                message.contains(&format!("`[agent] {field}` was removed")),
                "{message}"
            );
            assert!(message.contains("no longer spawns agents"), "{message}");
            assert!(message.contains("docs/harness-setup.md"), "{message}");
        }
    }

    #[test]
    fn diff_config_defaults_and_parses_from_toml() {
        let defaults = Config::default().diff;
        assert!(defaults.word_highlight);
        assert!(defaults.line_background);
        assert!(!defaults.gutter_bar);
        assert_eq!(defaults.view, DiffViewModeConfig::Unified);
        assert!(defaults.soft_wrap);
        assert_eq!(defaults.context_step, 10);
        // Diff theme entries default to "derived from [theme]".
        assert_eq!(defaults.theme, DiffThemeConfig::default());
        assert_eq!(defaults.theme.added_line_bg, None);
        assert_eq!(defaults.theme.removed_word, None);
        assert_eq!(Config::default().keybindings.expand_context, ["+"]);
        assert_eq!(Config::default().keybindings.expand_context_all, ["="]);
        assert_eq!(Config::default().keybindings.collapse_context, ["-"]);
        assert!(Config::default().keybindings.toggle_diff_wrap.is_empty());
        assert_eq!(
            Config::default().keybindings.toggle_annotation_artifacts,
            ["E"]
        );

        let repo = tempfile::tempdir().unwrap();
        let config_path = repo.path().join("config.toml");
        fs::write(
            &config_path,
            r##"
[diff]
word-highlight = false
gutter-bar = true
view = "side-by-side"
soft-wrap = false
context-step = 25

[diff.theme]
added-line-bg = "#103010"
gutter-added = "cyan"

[keybindings]
view-options = ["ctrl-v"]
toggle-gutter-bar = ["B"]
toggle-diff-wrap = ["alt-w"]
scroll-diff-left = ["alt-h"]
scroll-diff-right = ["alt-l"]
expand-context = ["ctrl-e"]
"##,
        )
        .unwrap();

        let config = Config::load_layers(&[ConfigSource {
            path: config_path,
            required: true,
        }])
        .unwrap();

        assert!(!config.diff.word_highlight);
        assert!(config.diff.line_background);
        assert!(config.diff.gutter_bar);
        assert_eq!(config.diff.view, DiffViewModeConfig::SideBySide);
        assert!(!config.diff.soft_wrap);
        assert_eq!(config.diff.context_step, 25);
        assert_eq!(config.diff.theme.added_line_bg.as_deref(), Some("#103010"));
        assert_eq!(config.diff.theme.gutter_added.as_deref(), Some("cyan"));
        // Untouched theme entries stay derived.
        assert_eq!(config.diff.theme.removed_line_bg, None);
        assert_eq!(config.keybindings.view_options, ["ctrl-v"]);
        assert_eq!(config.keybindings.toggle_gutter_bar, ["B"]);
        assert_eq!(config.keybindings.toggle_diff_wrap, ["alt-w"]);
        assert_eq!(config.keybindings.scroll_diff_left, ["alt-h"]);
        assert_eq!(config.keybindings.scroll_diff_right, ["alt-l"]);
        assert_eq!(config.keybindings.expand_context, ["ctrl-e"]);
        assert_eq!(config.keybindings.expand_context_all, ["="]);
    }

    #[test]
    fn default_keybindings_include_vim_and_arrow_navigation() {
        let config = Config::default();

        assert_eq!(config.keybindings.move_down, ["j", "down"]);
        assert_eq!(config.keybindings.move_up, ["k", "up"]);
        assert_eq!(config.keybindings.quit, ["q"]);
        assert_eq!(config.keybindings.compare_trunk, ["t"]);
        assert_eq!(config.keybindings.compare_parent, ["p"]);
        assert_eq!(config.keybindings.target_chooser, ["b"]);
        assert_eq!(config.keybindings.revset_input, ["R"]);
        assert_eq!(config.keybindings.stack_next, [">"]);
        assert_eq!(config.keybindings.stack_previous, ["<"]);
        assert_eq!(config.keybindings.operation_picker, ["I"]);
        assert_eq!(config.keybindings.jj_helpers, ["!"]);
        assert_eq!(config.keybindings.toggle_large_diff, ["L"]);
        assert_eq!(config.keybindings.toggle_agent_order, ["A"]);
        assert_eq!(config.keybindings.flag_list, ["F"]);
        assert_eq!(config.keybindings.open_work, ["X"]);
        assert_eq!(config.keybindings.draft_list, ["D"]);
        assert_eq!(config.limits.max_diff_lines, 5000);
        assert_eq!(config.keybindings.target_picker_down, ["down", "ctrl-j"]);
        assert_eq!(config.keybindings.target_picker_up, ["up", "ctrl-k"]);
        assert_eq!(config.keybindings.toggle_generated, ["h"]);
        assert_eq!(config.keybindings.toggle_fold, ["space"]);
        assert_eq!(config.keybindings.collapse_fold, ["left"]);
        assert_eq!(config.keybindings.expand_fold, ["right"]);
        assert_eq!(config.keybindings.scroll_diff_left, ["shift-left"]);
        assert_eq!(config.keybindings.scroll_diff_right, ["shift-right"]);
        assert_eq!(config.keybindings.range_comment, ["r"]);
        assert_eq!(config.keybindings.cancel_range_comment, ["ctrl-g", "esc"]);
        assert_eq!(config.keybindings.edit_comment, ["e"]);
        assert_eq!(config.keybindings.delete_comment, ["x"]);
        assert_eq!(config.keybindings.insert_newline, ["enter"]);
        assert_eq!(config.keybindings.submit_comment, ["ctrl-s"]);
        assert_eq!(config.artifact.on_tui_quit, TuiArtifactOnQuitConfig::Never);
    }

    #[test]
    fn layers_configs_in_source_order() {
        let dir = tempfile::tempdir().unwrap();
        let xdg = dir.path().join("xdg.toml");
        let project = dir.path().join("project.toml");
        let local = dir.path().join("local.toml");
        fs::write(
            &xdg,
            r#"
[artifact]
format = "json"

[comments]
initial-state = "draft"

[keybindings]
move-down = ["s"]
move-up = ["r"]
"#,
        )
        .unwrap();
        fs::write(
            &project,
            r#"
[ignore]
globs = ["Cargo.lock"]

[artifact]
basename = "project-review"

[generated]
presets = ["lockfiles"]

[comments]
initial-state = "todo"
default-channel = "collaboration"
"#,
        )
        .unwrap();
        fs::write(
            &local,
            r#"
[keybindings]
move-down = ["n", "down"]
"#,
        )
        .unwrap();

        let config = Config::load_layers(&[
            ConfigSource {
                path: xdg,
                required: false,
            },
            ConfigSource {
                path: project,
                required: false,
            },
            ConfigSource {
                path: local,
                required: false,
            },
        ])
        .unwrap();

        assert_eq!(config.artifact.format, ArtifactFormatConfig::Json);
        assert_eq!(config.artifact.basename, "project-review");
        assert_eq!(config.ignore.globs, ["Cargo.lock"]);
        assert_eq!(config.generated.presets, [GeneratedPreset::Lockfiles]);
        assert_eq!(config.keybindings.move_down, ["n", "down"]);
        assert_eq!(config.keybindings.move_up, ["r"]);
        assert_eq!(config.comments.initial_state, InitialCommentState::Todo);
        assert_eq!(
            config.comments.default_channel,
            Some(Channel::Collaboration)
        );
    }

    #[test]
    fn theme_name_and_palette_overrides_load_from_config() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("theme.toml");
        fs::write(
            &config_path,
            r##"
[theme]
name = "Tokyo_Night"

[theme.palette.dark]
accent = "#123456"

[theme.palette.light]
background = "#fedcba"
"##,
        )
        .unwrap();

        let config = Config::load_layers(&[ConfigSource {
            path: config_path,
            required: true,
        }])
        .unwrap();

        assert_eq!(config.theme.name, "tokyo-night");
        assert_eq!(config.theme.palette.dark.accent, Some(Rgb::hex(0x123456)));
        assert_eq!(
            config.theme.palette.light.background,
            Some(Rgb::hex(0xfedcba))
        );
    }

    #[test]
    fn invalid_theme_name_reports_accepted_names() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("theme.toml");
        fs::write(&config_path, "[theme]\nname = \"bogus\"\n").unwrap();

        let error = Config::load_layers(&[ConfigSource {
            path: config_path,
            required: true,
        }])
        .unwrap_err()
        .to_string();

        assert!(error.contains("invalid [theme] name \"bogus\""));
        assert!(error.contains("gander, catppuccin, gruvbox"));
    }

    #[test]
    fn keybinding_preset_resolves_after_layers_without_erasing_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let early = dir.path().join("early.toml");
        let late = dir.path().join("late.toml");
        fs::write(
            &early,
            r#"
[keybindings]
next-file = ["ctrl-n"]
previous-file = ["ctrl-p"]
"#,
        )
        .unwrap();
        fs::write(
            &late,
            r#"
[keybindings]
preset = "hunk"
"#,
        )
        .unwrap();

        let config = Config::load_layers(&[
            ConfigSource {
                path: early,
                required: false,
            },
            ConfigSource {
                path: late,
                required: false,
            },
        ])
        .unwrap();

        assert_eq!(config.keybindings.next_changed_hunk, ["alt-j", "]"]);
        assert_eq!(config.keybindings.next_file, ["ctrl-n"]);
        assert_eq!(config.keybindings.previous_file, ["ctrl-p"]);
    }

    #[test]
    fn theme_defaults_to_auto_transparent_and_parses_explicit_values() {
        let defaults = Config::default().theme;
        assert_eq!(defaults.mode, ThemeModeConfig::Auto);
        assert!(defaults.transparent);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("theme.toml");
        fs::write(
            &path,
            r#"
[theme]
mode = "light"
transparent = false
"#,
        )
        .unwrap();
        let config = Config::load_layers(&[ConfigSource {
            path,
            required: true,
        }])
        .unwrap();
        assert_eq!(config.theme.mode, ThemeModeConfig::Light);
        assert!(!config.theme.transparent);
    }

    #[test]
    fn theme_rejects_unknown_fields_and_modes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad-mode.toml");
        fs::write(&path, "[theme]\nmode = \"solarized\"\n").unwrap();
        assert!(
            Config::load_layers(&[ConfigSource {
                path,
                required: true,
            }])
            .is_err()
        );

        let path = dir.path().join("bad-field.toml");
        fs::write(&path, "[theme]\npalette = \"mono\"\n").unwrap();
        assert!(
            Config::load_layers(&[ConfigSource {
                path,
                required: true,
            }])
            .is_err()
        );
    }

    #[test]
    fn theme_layers_like_other_config_sections() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base.toml");
        let overlay = dir.path().join("overlay.toml");
        fs::write(&base, "[theme]\nmode = \"dark\"\ntransparent = false\n").unwrap();
        fs::write(&overlay, "[theme]\nmode = \"auto\"\n").unwrap();

        let config = Config::load_layers(&[
            ConfigSource {
                path: base,
                required: true,
            },
            ConfigSource {
                path: overlay,
                required: true,
            },
        ])
        .unwrap();
        // Later layer overrides mode; untouched transparent persists.
        assert_eq!(config.theme.mode, ThemeModeConfig::Auto);
        assert!(!config.theme.transparent);
    }

    #[test]
    fn comments_default_to_todo_and_reject_resolved() {
        assert_eq!(
            Config::default().comments.initial_state,
            InitialCommentState::Todo
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("resolved.toml");
        fs::write(&path, "[comments]\ninitial-state = \"resolved\"\n").unwrap();
        let error = Config::load_layers(&[ConfigSource {
            path,
            required: true,
        }])
        .unwrap_err();
        assert!(error.to_string().contains("failed to parse config"));
    }

    #[test]
    fn keybindings_reject_unknown_fields_clearly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unknown-key.toml");
        fs::write(&path, "[keybindings]\nmove-sideways = [\"h\"]\n").unwrap();

        let error = Config::load_layers(&[ConfigSource {
            path,
            required: true,
        }])
        .unwrap_err();
        let message = format!("{error:?}");
        assert!(
            message.contains("unknown field `move-sideways`"),
            "{message}"
        );
        assert!(message.contains("failed to parse config"), "{message}");
    }

    #[test]
    fn removed_zen_and_tour_keybinding_names_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        for field in ["zen", "tour", "zen-next", "zen-artifact"] {
            let path = dir.path().join(format!("{field}.toml"));
            fs::write(&path, format!("[keybindings]\n{field} = [\"T\"]\n")).unwrap();
            let error = Config::load_layers(&[ConfigSource {
                path,
                required: true,
            }])
            .unwrap_err();
            assert!(format!("{error:?}").contains("unknown field"));
        }
    }

    #[test]
    fn removed_summon_agent_keybinding_name_is_rejected() {
        // docs/decisions.md D9: the summon action is gone; a config still
        // binding it must fail clearly instead of silently doing nothing.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("summon-agent.toml");
        fs::write(&path, "[keybindings]\nsummon-agent = [\"@\"]\n").unwrap();
        let error = Config::load_layers(&[ConfigSource {
            path,
            required: true,
        }])
        .unwrap_err();
        let message = format!("{error:?}");
        assert!(
            message.contains("unknown field `summon-agent`"),
            "{message}"
        );
        assert!(message.contains("failed to parse config"), "{message}");
    }
}
