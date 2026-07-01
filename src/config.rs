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
"#,
        )
        .unwrap();

        let config = Config::load(repo.path(), Some(&config_path)).unwrap();

        assert_eq!(config.ignore.globs, ["Cargo.lock", "**/*.min.js"]);
        assert_eq!(config.artifact.format, ArtifactFormatConfig::Json);
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
}
