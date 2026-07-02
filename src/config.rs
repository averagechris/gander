use std::{
    env, fs,
    path::{Path, PathBuf},
};

use color_eyre::eyre::{Context, Result};
use serde::Deserialize;

use crate::{
    generated::{GeneratedPolicy, GeneratedPreset},
    syntax::SyntaxConfig,
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
}

/// Size thresholds that keep huge inputs from overwhelming the TUI.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct LimitsConfig {
    /// Diffs with more lines than this render as a placeholder until
    /// explicitly expanded.
    pub max_diff_lines: usize,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_diff_lines: 5000,
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
    pub output_dir: PathBuf,
    pub basename: String,
    pub on_tui_quit: TuiArtifactOnQuitConfig,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct KeybindingsConfig {
    pub quit: Vec<String>,
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
    pub target_picker_down: Vec<String>,
    pub target_picker_up: Vec<String>,
    pub next_unviewed: Vec<String>,
    pub previous_unviewed: Vec<String>,
    pub next_comment: Vec<String>,
    pub previous_comment: Vec<String>,
    pub file_search: Vec<String>,
    pub symbol_outline: Vec<String>,
    pub next_symbol: Vec<String>,
    pub previous_symbol: Vec<String>,
    pub scroll_down: Vec<String>,
    pub scroll_up: Vec<String>,
    pub mark_viewed: Vec<String>,
    pub toggle_viewed: Vec<String>,
    pub mark_all_viewed: Vec<String>,
    pub toggle_generated: Vec<String>,
    pub cycle_viewed_filter: Vec<String>,
    pub toggle_fold: Vec<String>,
    pub collapse_fold: Vec<String>,
    pub expand_fold: Vec<String>,
    pub toggle_context_fold: Vec<String>,
    pub range_comment: Vec<String>,
    pub cancel_range_comment: Vec<String>,
    pub comment: Vec<String>,
    pub edit_comment: Vec<String>,
    pub delete_comment: Vec<String>,
    pub comment_list: Vec<String>,
    pub submit_comment: Vec<String>,
    pub cancel_comment: Vec<String>,
    pub insert_newline: Vec<String>,
    pub delete_char: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactFormatConfig {
    Json,
    Markdown,
}

/// Artifact audience: agent adds raw hunks and comment excerpts to JSON.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactProfileConfig {
    #[default]
    Human,
    Agent,
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
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct LimitsConfigPatch {
    max_diff_lines: Option<usize>,
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
#[serde(default, rename_all = "kebab-case")]
struct KeybindingsConfigPatch {
    quit: Option<Vec<String>>,
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
    target_picker_down: Option<Vec<String>>,
    target_picker_up: Option<Vec<String>>,
    next_unviewed: Option<Vec<String>>,
    previous_unviewed: Option<Vec<String>>,
    next_comment: Option<Vec<String>>,
    previous_comment: Option<Vec<String>>,
    file_search: Option<Vec<String>>,
    symbol_outline: Option<Vec<String>>,
    next_symbol: Option<Vec<String>>,
    previous_symbol: Option<Vec<String>>,
    scroll_down: Option<Vec<String>>,
    scroll_up: Option<Vec<String>>,
    mark_viewed: Option<Vec<String>>,
    toggle_viewed: Option<Vec<String>>,
    mark_all_viewed: Option<Vec<String>>,
    toggle_generated: Option<Vec<String>>,
    cycle_viewed_filter: Option<Vec<String>>,
    toggle_fold: Option<Vec<String>>,
    collapse_fold: Option<Vec<String>>,
    expand_fold: Option<Vec<String>>,
    toggle_context_fold: Option<Vec<String>>,
    range_comment: Option<Vec<String>>,
    cancel_range_comment: Option<Vec<String>>,
    comment: Option<Vec<String>>,
    edit_comment: Option<Vec<String>>,
    delete_comment: Option<Vec<String>>,
    comment_list: Option<Vec<String>>,
    submit_comment: Option<Vec<String>>,
    cancel_comment: Option<Vec<String>>,
    insert_newline: Option<Vec<String>>,
    delete_char: Option<Vec<String>>,
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
            output_dir: PathBuf::from(".gander"),
            basename: "review".to_owned(),
            on_tui_quit: TuiArtifactOnQuitConfig::Stdout,
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
        Self {
            // Esc intentionally does not quit: it dismisses the current layer
            // (range selection, notices, popups) so it stays safe to mash.
            quit: keys(["q"]),
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
            target_picker_down: keys(["down", "ctrl-j"]),
            target_picker_up: keys(["up", "ctrl-k"]),
            next_unviewed: keys(["n"]),
            previous_unviewed: keys(["N"]),
            next_comment: keys(["m"]),
            previous_comment: keys(["M"]),
            file_search: keys(["/"]),
            symbol_outline: keys(["o"]),
            next_symbol: keys(["]"]),
            previous_symbol: keys(["["]),
            scroll_down: keys(["d", "pagedown"]),
            scroll_up: keys(["u", "pageup"]),
            mark_viewed: keys(["enter"]),
            toggle_viewed: keys(["v"]),
            mark_all_viewed: keys(["a"]),
            toggle_generated: keys(["h"]),
            cycle_viewed_filter: keys(["f"]),
            toggle_fold: keys(["space"]),
            collapse_fold: keys(["left"]),
            expand_fold: keys(["right"]),
            toggle_context_fold: keys(["z"]),
            range_comment: keys(["r"]),
            cancel_range_comment: keys(["ctrl-g", "esc"]),
            comment: keys(["c"]),
            edit_comment: keys(["e"]),
            delete_comment: keys(["x"]),
            comment_list: keys(["C"]),
            submit_comment: keys(["ctrl-s"]),
            cancel_comment: keys(["esc"]),
            insert_newline: keys(["enter"]),
            delete_char: keys(["backspace"]),
        }
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
            let patch: ConfigPatch = toml::from_str(&contents)
                .with_context(|| format!("failed to parse config {}", source.path.display()))?;
            config.apply_patch(patch);
        }

        Ok(config)
    }

    fn apply_patch(&mut self, patch: ConfigPatch) {
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
            self.artifact.output_dir = output_dir;
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

        if let Some(syntax) = patch.syntax {
            self.syntax = syntax;
        }

        if let Some(max_diff_lines) = patch.limits.max_diff_lines {
            self.limits.max_diff_lines = max_diff_lines;
        }
    }
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
        apply_optional(&mut self.quit, patch.quit);
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
        apply_optional(&mut self.target_picker_down, patch.target_picker_down);
        apply_optional(&mut self.target_picker_up, patch.target_picker_up);
        apply_optional(&mut self.next_unviewed, patch.next_unviewed);
        apply_optional(&mut self.previous_unviewed, patch.previous_unviewed);
        apply_optional(&mut self.next_comment, patch.next_comment);
        apply_optional(&mut self.previous_comment, patch.previous_comment);
        apply_optional(&mut self.file_search, patch.file_search);
        apply_optional(&mut self.symbol_outline, patch.symbol_outline);
        apply_optional(&mut self.next_symbol, patch.next_symbol);
        apply_optional(&mut self.previous_symbol, patch.previous_symbol);
        apply_optional(&mut self.scroll_down, patch.scroll_down);
        apply_optional(&mut self.scroll_up, patch.scroll_up);
        apply_optional(&mut self.mark_viewed, patch.mark_viewed);
        apply_optional(&mut self.toggle_viewed, patch.toggle_viewed);
        apply_optional(&mut self.mark_all_viewed, patch.mark_all_viewed);
        apply_optional(&mut self.toggle_generated, patch.toggle_generated);
        apply_optional(&mut self.cycle_viewed_filter, patch.cycle_viewed_filter);
        apply_optional(&mut self.toggle_fold, patch.toggle_fold);
        apply_optional(&mut self.collapse_fold, patch.collapse_fold);
        apply_optional(&mut self.expand_fold, patch.expand_fold);
        apply_optional(&mut self.toggle_context_fold, patch.toggle_context_fold);
        apply_optional(&mut self.range_comment, patch.range_comment);
        apply_optional(&mut self.cancel_range_comment, patch.cancel_range_comment);
        apply_optional(&mut self.comment, patch.comment);
        apply_optional(&mut self.edit_comment, patch.edit_comment);
        apply_optional(&mut self.delete_comment, patch.delete_comment);
        apply_optional(&mut self.comment_list, patch.comment_list);
        apply_optional(&mut self.submit_comment, patch.submit_comment);
        apply_optional(&mut self.cancel_comment, patch.cancel_comment);
        apply_optional(&mut self.insert_newline, patch.insert_newline);
        apply_optional(&mut self.delete_char, patch.delete_char);
    }
}

fn apply_optional<T>(target: &mut T, value: Option<T>) {
    if let Some(value) = value {
        *target = value;
    }
}

fn xdg_config_path() -> Option<PathBuf> {
    if let Some(config_home) = env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(config_home).join("gander/config.toml"));
    }
    env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(|home| PathBuf::from(home).join(".config/gander/config.toml"))
}

impl ArtifactConfig {
    pub fn output_path(&self, repo: &Path, format: ArtifactFormatConfig) -> PathBuf {
        let extension = match format {
            ArtifactFormatConfig::Json => "json",
            ArtifactFormatConfig::Markdown => "md",
        };
        let output_dir = if self.output_dir.is_absolute() {
            self.output_dir.clone()
        } else {
            repo.join(&self.output_dir)
        };
        output_dir.join(format!("{}.{extension}", self.basename))
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
            repo.path().join("artifacts").join("review-current.json")
        );
    }

    #[test]
    fn output_path_uses_selected_format_extension() {
        let repo = tempfile::tempdir().unwrap();
        let artifact = ArtifactConfig::default();

        assert_eq!(
            artifact.output_path(repo.path(), ArtifactFormatConfig::Markdown),
            repo.path().join(".gander").join("review.md")
        );
        assert_eq!(
            artifact.output_path(repo.path(), ArtifactFormatConfig::Json),
            repo.path().join(".gander").join("review.json")
        );
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
        assert_eq!(config.limits.max_diff_lines, 5000);
        assert_eq!(config.keybindings.target_picker_down, ["down", "ctrl-j"]);
        assert_eq!(config.keybindings.target_picker_up, ["up", "ctrl-k"]);
        assert_eq!(config.keybindings.toggle_generated, ["h"]);
        assert_eq!(config.keybindings.toggle_fold, ["space"]);
        assert_eq!(config.keybindings.collapse_fold, ["left"]);
        assert_eq!(config.keybindings.expand_fold, ["right"]);
        assert_eq!(config.keybindings.range_comment, ["r"]);
        assert_eq!(config.keybindings.cancel_range_comment, ["ctrl-g", "esc"]);
        assert_eq!(config.keybindings.edit_comment, ["e"]);
        assert_eq!(config.keybindings.delete_comment, ["x"]);
        assert_eq!(config.keybindings.insert_newline, ["enter"]);
        assert_eq!(config.keybindings.submit_comment, ["ctrl-s"]);
        assert_eq!(config.artifact.on_tui_quit, TuiArtifactOnQuitConfig::Stdout);
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
    }
}
