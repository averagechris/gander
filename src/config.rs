use std::{
    env, fs,
    path::{Path, PathBuf},
};

use color_eyre::eyre::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Config {
    pub ignore: IgnoreConfig,
    pub artifact: ArtifactConfig,
    pub keybindings: KeybindingsConfig,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct IgnoreConfig {
    pub globs: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ArtifactConfig {
    pub format: ArtifactFormatConfig,
    pub output_dir: PathBuf,
    pub basename: String,
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
    pub next_unviewed: Vec<String>,
    pub previous_unviewed: Vec<String>,
    pub next_comment: Vec<String>,
    pub previous_comment: Vec<String>,
    pub scroll_down: Vec<String>,
    pub scroll_up: Vec<String>,
    pub mark_viewed: Vec<String>,
    pub toggle_viewed: Vec<String>,
    pub mark_all_viewed: Vec<String>,
    pub toggle_fold: Vec<String>,
    pub collapse_fold: Vec<String>,
    pub expand_fold: Vec<String>,
    pub comment: Vec<String>,
    pub submit_comment: Vec<String>,
    pub cancel_comment: Vec<String>,
    pub insert_newline: Vec<String>,
    pub delete_char: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactFormatConfig {
    Json,
    Markdown,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct ConfigPatch {
    ignore: IgnoreConfigPatch,
    artifact: ArtifactConfigPatch,
    keybindings: KeybindingsConfigPatch,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct IgnoreConfigPatch {
    globs: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct ArtifactConfigPatch {
    format: Option<ArtifactFormatConfig>,
    output_dir: Option<PathBuf>,
    basename: Option<String>,
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
    next_unviewed: Option<Vec<String>>,
    previous_unviewed: Option<Vec<String>>,
    next_comment: Option<Vec<String>>,
    previous_comment: Option<Vec<String>>,
    scroll_down: Option<Vec<String>>,
    scroll_up: Option<Vec<String>>,
    mark_viewed: Option<Vec<String>>,
    toggle_viewed: Option<Vec<String>>,
    mark_all_viewed: Option<Vec<String>>,
    toggle_fold: Option<Vec<String>>,
    collapse_fold: Option<Vec<String>>,
    expand_fold: Option<Vec<String>>,
    comment: Option<Vec<String>>,
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
            output_dir: PathBuf::from(".jj-change-viewer"),
            basename: "review".to_owned(),
        }
    }
}

impl Default for KeybindingsConfig {
    fn default() -> Self {
        Self {
            quit: keys(["q", "esc"]),
            move_down: keys(["j", "down"]),
            move_up: keys(["k", "up"]),
            toggle_focus: keys(["tab"]),
            diff_top: keys(["g"]),
            diff_bottom: keys(["G"]),
            next_unviewed: keys(["n"]),
            previous_unviewed: keys(["N"]),
            next_comment: keys(["m"]),
            previous_comment: keys(["M"]),
            scroll_down: keys(["d", "pagedown"]),
            scroll_up: keys(["u", "pageup"]),
            mark_viewed: keys(["enter"]),
            toggle_viewed: keys(["v"]),
            mark_all_viewed: keys(["a"]),
            toggle_fold: keys(["space"]),
            collapse_fold: keys(["left"]),
            expand_fold: keys(["right"]),
            comment: keys(["c"]),
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
            path: repo.join("jj-change-viewer.toml"),
            required: false,
        });
        sources.push(ConfigSource {
            path: repo.join(".jj-change-viewer").join("config.toml"),
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

        if let Some(format) = patch.artifact.format {
            self.artifact.format = format;
        }
        if let Some(output_dir) = patch.artifact.output_dir {
            self.artifact.output_dir = output_dir;
        }
        if let Some(basename) = patch.artifact.basename {
            self.artifact.basename = basename;
        }

        self.keybindings.apply_patch(patch.keybindings);
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
        apply_optional(&mut self.next_unviewed, patch.next_unviewed);
        apply_optional(&mut self.previous_unviewed, patch.previous_unviewed);
        apply_optional(&mut self.next_comment, patch.next_comment);
        apply_optional(&mut self.previous_comment, patch.previous_comment);
        apply_optional(&mut self.scroll_down, patch.scroll_down);
        apply_optional(&mut self.scroll_up, patch.scroll_up);
        apply_optional(&mut self.mark_viewed, patch.mark_viewed);
        apply_optional(&mut self.toggle_viewed, patch.toggle_viewed);
        apply_optional(&mut self.mark_all_viewed, patch.mark_all_viewed);
        apply_optional(&mut self.toggle_fold, patch.toggle_fold);
        apply_optional(&mut self.collapse_fold, patch.collapse_fold);
        apply_optional(&mut self.expand_fold, patch.expand_fold);
        apply_optional(&mut self.comment, patch.comment);
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
        return Some(PathBuf::from(config_home).join("jj-change-viewer/config.toml"));
    }
    env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(|home| PathBuf::from(home).join(".config/jj-change-viewer/config.toml"))
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

[artifact]
format = "json"
output_dir = "artifacts"
basename = "review-current"

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
        assert_eq!(config.artifact.format, ArtifactFormatConfig::Json);
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
            repo.path().join(".jj-change-viewer").join("review.md")
        );
        assert_eq!(
            artifact.output_path(repo.path(), ArtifactFormatConfig::Json),
            repo.path().join(".jj-change-viewer").join("review.json")
        );
    }

    #[test]
    fn default_keybindings_include_vim_and_arrow_navigation() {
        let config = Config::default();

        assert_eq!(config.keybindings.move_down, ["j", "down"]);
        assert_eq!(config.keybindings.move_up, ["k", "up"]);
        assert_eq!(config.keybindings.quit, ["q", "esc"]);
        assert_eq!(config.keybindings.toggle_fold, ["space"]);
        assert_eq!(config.keybindings.collapse_fold, ["left"]);
        assert_eq!(config.keybindings.expand_fold, ["right"]);
        assert_eq!(config.keybindings.insert_newline, ["enter"]);
        assert_eq!(config.keybindings.submit_comment, ["ctrl-s"]);
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
        assert_eq!(config.keybindings.move_down, ["n", "down"]);
        assert_eq!(config.keybindings.move_up, ["r"]);
    }
}
