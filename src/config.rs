use std::{
    fs,
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
    pub comment: Vec<String>,
    pub submit_comment: Vec<String>,
    pub cancel_comment: Vec<String>,
    pub delete_char: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactFormatConfig {
    Json,
    Markdown,
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
            comment: keys(["c"]),
            submit_comment: keys(["enter"]),
            cancel_comment: keys(["esc"]),
            delete_char: keys(["backspace"]),
        }
    }
}

fn keys<const N: usize>(keys: [&str; N]) -> Vec<String> {
    keys.into_iter().map(str::to_owned).collect()
}

impl Config {
    pub fn load(repo: &Path, explicit_path: Option<&Path>) -> Result<Self> {
        let (path, explicit) = match explicit_path {
            Some(path) => (path.to_path_buf(), true),
            None => (repo.join(".jj-change-viewer").join("config.toml"), false),
        };

        if !path.exists() {
            if explicit {
                fs::read_to_string(&path)
                    .with_context(|| format!("failed to read config {}", path.display()))?;
            }
            return Ok(Self::default());
        }

        let contents = fs::read_to_string(&path)
            .with_context(|| format!("failed to read config {}", path.display()))?;
        toml::from_str(&contents)
            .with_context(|| format!("failed to parse config {}", path.display()))
    }
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
        let repo = tempfile::tempdir().unwrap();
        let config = Config::load(repo.path(), None).unwrap();

        assert_eq!(config, Config::default());
    }

    #[test]
    fn explicit_missing_config_errors() {
        let repo = tempfile::tempdir().unwrap();
        let missing = repo.path().join("missing.toml");

        assert!(Config::load(repo.path(), Some(&missing)).is_err());
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
"#,
        )
        .unwrap();

        let config = Config::load(repo.path(), Some(&config_path)).unwrap();

        assert_eq!(config.ignore.globs, ["Cargo.lock", "**/*.min.js"]);
        assert_eq!(config.artifact.format, ArtifactFormatConfig::Json);
        assert_eq!(config.keybindings.move_down, ["s", "down"]);
        assert_eq!(config.keybindings.quit, ["q"]);
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
    }
}
