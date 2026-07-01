mod anchor;
mod app;
mod artifact;
mod config;
mod diff;
mod file_tree;
mod generated;
mod jj;
mod state;
mod syntax;
mod tui;

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use color_eyre::eyre::Context;

use crate::{
    app::ReviewSession,
    artifact::{ArtifactFormat, write_artifact},
    config::{ArtifactFormatConfig, Config},
    diff::DiffSet,
    generated::{GeneratedMatcher, GeneratedPolicy, GeneratedPreset},
    jj::{JjCommand, ReviewTarget},
    state::ReviewState,
};

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Repository root. Defaults to the current directory.
    #[arg(long, global = true)]
    repo: Option<PathBuf>,

    /// jj revision to review. Defaults to the working copy commit.
    #[arg(short, long, default_value = "@", global = true)]
    rev: String,

    /// jj revision/revset to compare from. Defaults to trunk().
    #[arg(short, long, default_value = "trunk()", global = true)]
    base: String,

    /// Hide files matching this glob. Can be repeated.
    #[arg(long = "ignore", global = true)]
    ignore: Vec<String>,

    /// Treat files matching this generated/noisy preset as generated. Can be repeated.
    #[arg(long = "generated-preset", value_enum, global = true)]
    generated_preset: Vec<GeneratedPresetArg>,

    /// Treat files matching this glob as generated/noisy. Can be repeated.
    #[arg(long = "generated-glob", global = true)]
    generated_glob: Vec<String>,

    /// Path to the persistent review state file.
    #[arg(long, global = true)]
    state: Option<PathBuf>,

    /// Path to a jj-change-viewer config file. Defaults to .jj-change-viewer/config.toml if present.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Launch the terminal UI. This is the default command.
    Tui,
    /// Export the current review as JSON or Markdown.
    Export {
        #[arg(value_enum)]
        format: Option<OutputFormat>,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Mark all currently visible files as viewed without opening the TUI.
    MarkViewed,
    /// Mark generated/noisy files as viewed without opening the TUI.
    MarkGeneratedViewed,
    /// Print a terse summary of the current change.
    Summary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    Json,
    Markdown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum GeneratedPresetArg {
    Lockfiles,
    ApiClients,
    VendoredAssets,
}

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();
    let repo = cli.repo.unwrap_or(std::env::current_dir()?);
    let config = Config::load(&repo, cli.config.as_deref())?;
    let target = ReviewTarget::new(cli.base, cli.rev);
    let jj = JjCommand::new(repo.clone(), target.clone());
    let command = cli.command.unwrap_or(Command::Tui);
    let generated_policy = merge_generated(&config, cli.generated_preset, cli.generated_glob);
    let generated_matcher = GeneratedMatcher::new(&generated_policy)?;

    let diff_text = jj
        .diff()
        .with_context(|| format!("failed to read jj diff for {target}"))?;
    let mut diff = DiffSet::parse(&diff_text).wrap_err("failed to parse jj git diff")?;
    let ignore_globs = merge_ignores(&config, cli.ignore);
    if !matches!(command, Command::MarkGeneratedViewed) {
        diff.apply_ignores(&ignore_globs)?;
    }

    let state_path = cli
        .state
        .unwrap_or_else(|| repo.join(".jj-change-viewer").join("state.json"));
    let mut state = ReviewState::load_or_default(&state_path)?;
    let mut session = ReviewSession::new(repo.clone(), target, diff, state.clone());
    session.annotate_generated_where(|file| generated_matcher.is_match(&file.path));
    session.apply_viewed_state();

    match command {
        Command::Tui => {
            tui::run(
                &mut session,
                &config.keybindings,
                ignore_globs,
                generated_matcher,
            )?;
            state = session.into_state();
            state.save(&state_path)?;
        }
        Command::Export { format, output } => {
            let (format, output) = resolve_export_options(&repo, &config, format, output);
            write_artifact(
                &session,
                match format {
                    OutputFormat::Json => ArtifactFormat::Json,
                    OutputFormat::Markdown => ArtifactFormat::Markdown,
                },
                &output,
            )?;
        }
        Command::MarkViewed => {
            session.mark_all_viewed();
            state = session.into_state();
            state.save(&state_path)?;
        }
        Command::MarkGeneratedViewed => {
            session.mark_files_viewed_where(|file| file.generated);
            state = session.into_state();
            state.save(&state_path)?;
        }
        Command::Summary => {
            println!("Reviewing {}", session.target);
            println!("{}", session.summary_line());
            for file in &session.files {
                println!(
                    "{mark} {status:>7} {generated:>5} {path} (+{additions}/-{deletions})",
                    mark = if file.viewed { "✓" } else { "•" },
                    status = file.status,
                    generated = if file.generated { "gen" } else { "" },
                    path = file.path,
                    additions = file.additions,
                    deletions = file.deletions
                );
            }
        }
    }

    Ok(())
}

fn merge_generated(
    config: &Config,
    cli_presets: Vec<GeneratedPresetArg>,
    cli_globs: Vec<String>,
) -> GeneratedPolicy {
    let mut policy: GeneratedPolicy = config.generated.clone().into();
    policy
        .presets
        .extend(cli_presets.into_iter().map(GeneratedPreset::from));
    policy.globs.extend(cli_globs);
    policy
}

fn merge_ignores(config: &Config, cli_ignores: Vec<String>) -> Vec<String> {
    let mut ignores = config.ignore.globs.clone();
    ignores.extend(cli_ignores);
    ignores
}

fn resolve_export_options(
    repo: &std::path::Path,
    config: &Config,
    cli_format: Option<OutputFormat>,
    cli_output: Option<PathBuf>,
) -> (OutputFormat, PathBuf) {
    let format = cli_format.unwrap_or_else(|| config.artifact.format.into());
    let output = cli_output.unwrap_or_else(|| config.artifact.output_path(repo, format.into()));
    (format, output)
}

impl From<ArtifactFormatConfig> for OutputFormat {
    fn from(value: ArtifactFormatConfig) -> Self {
        match value {
            ArtifactFormatConfig::Json => Self::Json,
            ArtifactFormatConfig::Markdown => Self::Markdown,
        }
    }
}

impl From<OutputFormat> for ArtifactFormatConfig {
    fn from(value: OutputFormat) -> Self {
        match value {
            OutputFormat::Json => Self::Json,
            OutputFormat::Markdown => Self::Markdown,
        }
    }
}

impl From<GeneratedPresetArg> for GeneratedPreset {
    fn from(value: GeneratedPresetArg) -> Self {
        match value {
            GeneratedPresetArg::Lockfiles => Self::Lockfiles,
            GeneratedPresetArg::ApiClients => Self::ApiClients,
            GeneratedPresetArg::VendoredAssets => Self::VendoredAssets,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_ignores_appends_cli_to_config() {
        let config = Config {
            ignore: crate::config::IgnoreConfig {
                globs: vec!["Cargo.lock".to_owned()],
            },
            ..Config::default()
        };

        assert_eq!(
            merge_ignores(&config, vec!["**/*.lock".to_owned()]),
            ["Cargo.lock", "**/*.lock"]
        );
    }

    #[test]
    fn export_options_use_config_defaults() {
        let repo = tempfile::tempdir().unwrap();
        let config = Config::default();

        let (format, output) = resolve_export_options(repo.path(), &config, None, None);

        assert_eq!(format, OutputFormat::Markdown);
        assert_eq!(
            output,
            repo.path().join(".jj-change-viewer").join("review.md")
        );
    }

    #[test]
    fn export_options_cli_format_changes_derived_extension() {
        let repo = tempfile::tempdir().unwrap();
        let config = Config::default();

        let (format, output) =
            resolve_export_options(repo.path(), &config, Some(OutputFormat::Json), None);

        assert_eq!(format, OutputFormat::Json);
        assert_eq!(
            output,
            repo.path().join(".jj-change-viewer").join("review.json")
        );
    }

    #[test]
    fn merge_generated_appends_cli_policy() {
        let config = Config {
            generated: crate::config::GeneratedConfig {
                presets: vec![GeneratedPreset::Lockfiles],
                globs: vec!["schemas/*.json".to_owned()],
            },
            ..Config::default()
        };

        let policy = merge_generated(
            &config,
            vec![GeneratedPresetArg::ApiClients],
            vec!["dist/**".to_owned()],
        );

        assert_eq!(
            policy.presets,
            [GeneratedPreset::Lockfiles, GeneratedPreset::ApiClients]
        );
        assert_eq!(policy.globs, ["schemas/*.json", "dist/**"]);
    }
}
