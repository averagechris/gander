mod acp;
mod agent;
mod anchor;
mod app;
mod artifact;
mod config;
mod diff;
mod file_tree;
mod fuzzy;
mod generated;
mod jj;
mod paths;
mod registry;
mod state;
mod syntax;
mod tui;

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use color_eyre::eyre::{Context, eyre};

use crate::{
    app::ReviewSession,
    artifact::{
        ArtifactFormat, ArtifactProfile, OwnedReviewArtifact, import_json_artifact_into_state,
        write_artifact, write_artifact_to,
    },
    config::{ArtifactFormatConfig, ArtifactProfileConfig, Config, TuiArtifactOnQuitConfig},
    diff::DiffSet,
    generated::{GeneratedMatcher, GeneratedPolicy, GeneratedPreset},
    jj::{JjBackend, JjCliBackend, ReviewTarget},
    paths::{PathsEnv, WorkspacePaths},
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

    /// Path to a gander config file. Layered over XDG user config and a
    /// committed gander.toml at the repo root.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Launch the terminal UI. This is the default command.
    Tui {
        /// Emit an artifact after quitting the TUI.
        #[arg(long, value_enum)]
        artifact_on_quit: Option<TuiArtifactOnQuitArg>,

        /// Artifact format for --artifact-on-quit.
        #[arg(long = "artifact-format", value_enum)]
        artifact_format: Option<OutputFormat>,

        /// Artifact profile for --artifact-on-quit (agent adds raw excerpts).
        #[arg(long = "artifact-profile", value_enum)]
        artifact_profile: Option<OutputProfile>,

        /// Artifact output path for --artifact-on-quit write.
        #[arg(long = "artifact-output")]
        artifact_output: Option<PathBuf>,
    },
    /// Export the current review as JSON or Markdown.
    Export {
        #[arg(value_enum)]
        format: Option<OutputFormat>,
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Artifact profile; agent adds raw hunks and comment excerpts.
        #[arg(short, long, value_enum)]
        profile: Option<OutputProfile>,
    },
    /// Import comments/viewed state from a JSON review artifact.
    Import {
        #[arg(value_name = "JSON_ARTIFACT")]
        input: PathBuf,
    },
    /// Mark all currently visible files as viewed without opening the TUI.
    MarkViewed,
    /// Mark generated/noisy files as viewed without opening the TUI.
    MarkGeneratedViewed,
    /// Serve the review session to agents over line-delimited JSON-RPC on
    /// stdio (ACP). Bridges to a running TUI's live session when one is
    /// serving this workspace's ACP socket; otherwise serves a snapshot
    /// directly. See docs/acp.md.
    Acp,
    /// Print resolved state/runtime/config locations for this workspace.
    Paths,
    /// Print a terse summary of the current change.
    Summary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    Json,
    Markdown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum OutputProfile {
    Human,
    Agent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum GeneratedPresetArg {
    Lockfiles,
    ApiClients,
    VendoredAssets,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum TuiArtifactOnQuitArg {
    Never,
    Write,
    Stdout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TuiArtifactRequest {
    format: OutputFormat,
    profile: OutputProfile,
    destination: TuiArtifactDestination,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TuiArtifactDestination {
    File(PathBuf),
    Stdout,
}

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();
    let repo = cli.repo.unwrap_or(std::env::current_dir()?);
    let config = Config::load(&repo, cli.config.as_deref())?;
    warn_deprecated_config_layer(&repo);
    let workspace_paths = WorkspacePaths::resolve(&repo, &PathsEnv::from_env())?;
    let command = cli.command.unwrap_or(Command::Tui {
        artifact_on_quit: None,
        artifact_format: None,
        artifact_profile: None,
        artifact_output: None,
    });
    if matches!(command, Command::Paths) {
        print_paths(&workspace_paths, &repo, cli.state.as_deref());
        return Ok(());
    }
    // One-release migration fallback (docs/decisions.md D6): pick up legacy
    // `.gander/` state before anything reads the new locations.
    workspace_paths
        .migrate_legacy_state()
        .wrap_err("failed to migrate legacy .gander state")?;
    let jj = JjCliBackend::from_configured(&config.jj.binary)?;
    let target = ReviewTarget::new(cli.base, cli.rev);
    let generated_policy = merge_generated(&config, cli.generated_preset, cli.generated_glob);
    let generated_matcher = GeneratedMatcher::new(&generated_policy)?;

    let diff_text = jj
        .diff(&repo, &target)
        .with_context(|| format!("failed to read jj diff for {target}"))?;
    let mut diff = DiffSet::parse(&diff_text).wrap_err("failed to parse jj git diff")?;
    let ignore_globs = merge_ignores(&config, cli.ignore);
    if !matches!(command, Command::MarkGeneratedViewed) {
        diff.apply_ignores(&ignore_globs)?;
    }

    let state_path = cli.state.unwrap_or_else(|| workspace_paths.state_file());
    let mut state = ReviewState::load_or_default(&state_path)?;
    let mut session =
        ReviewSession::new_with_config(repo.clone(), target, diff, state.clone(), &config);
    session.annotate_generated_where(|file| {
        generated_matcher.is_match(&file.path)
            || crate::generated::diff_content_looks_generated(&file.diff)
    });

    match command {
        Command::Tui {
            artifact_on_quit,
            artifact_format,
            artifact_profile,
            artifact_output,
        } => {
            // Resolve before entering the TUI so a misconfiguration (write
            // mode without a destination) fails fast instead of after the
            // review session.
            let artifact_request = resolve_tui_artifact_options(
                &repo,
                &config,
                artifact_on_quit,
                artifact_format,
                artifact_profile,
                artifact_output,
            )?;
            tui::run(
                &mut session,
                &config.keybindings,
                ignore_globs,
                generated_matcher,
                &jj,
                tui::TuiPaths {
                    state_file: Some(state_path.clone()),
                    agent_overlay: Some(workspace_paths.overlay_file()),
                    acp_socket: Some(workspace_paths.instance_socket_file(std::process::id())),
                    agent_log: Some(workspace_paths.agent_log_file()),
                    registry_dir: Some(workspace_paths.registry_dir.clone()),
                    workspace_root: Some(workspace_paths.workspace_root.clone()),
                },
                config.agent.clone(),
            )?;
            state = session.clone().into_state();
            state.save(&state_path)?;
            if let Some(request) = artifact_request {
                let format = match request.format {
                    OutputFormat::Json => ArtifactFormat::Json,
                    OutputFormat::Markdown => ArtifactFormat::Markdown,
                };
                let profile = ArtifactProfile::from(request.profile);
                match request.destination {
                    TuiArtifactDestination::File(path) => {
                        write_artifact(&session, format, profile, &path)?
                    }
                    TuiArtifactDestination::Stdout => {
                        let stdout = std::io::stdout();
                        write_artifact_to(&session, format, profile, stdout.lock())?;
                    }
                }
            }
        }
        Command::Export {
            format,
            output,
            profile,
        } => {
            let (format, destination, profile) =
                resolve_export_options(&repo, &config, format, output, profile);
            let format = match format {
                OutputFormat::Json => ArtifactFormat::Json,
                OutputFormat::Markdown => ArtifactFormat::Markdown,
            };
            let profile = ArtifactProfile::from(profile);
            match destination {
                TuiArtifactDestination::File(path) => {
                    write_artifact(&session, format, profile, &path)?
                }
                TuiArtifactDestination::Stdout => {
                    let stdout = std::io::stdout();
                    write_artifact_to(&session, format, profile, stdout.lock())?;
                }
            }
        }
        Command::Import { input } => {
            let contents = std::fs::read_to_string(&input)
                .with_context(|| format!("failed to read artifact {}", input.display()))?;
            let artifact: OwnedReviewArtifact = serde_json::from_str(&contents)
                .with_context(|| format!("failed to parse JSON artifact {}", input.display()))?;
            if artifact.base != session.target.base || artifact.revision != session.target.rev {
                eprintln!(
                    "warning: importing artifact for {}..{} into current target {}",
                    artifact.base, artifact.revision, session.target
                );
            }
            state = session.clone().into_state();
            let summary = import_json_artifact_into_state(&mut state, &artifact);
            state.save(&state_path)?;
            println!(
                "Imported {} comments, skipped {} duplicates, restored {} viewed files",
                summary.comments_imported,
                summary.duplicate_comments_skipped,
                summary.viewed_files_imported
            );
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
        Command::Acp => {
            // Prefer a live TUI session for this workspace (found through
            // the instance registry): agents then see current viewed state,
            // comments, and target instead of this process's startup
            // snapshot. With several instances, the most recently touched
            // one wins.
            #[cfg(unix)]
            if let Some(instance) = crate::registry::find_live_for_workspace(
                &workspace_paths.registry_dir,
                &workspace_paths.workspace_root,
            ) {
                return crate::acp::socket::bridge_stdio(&instance.socket_path);
            }
            let overlay_path = workspace_paths.overlay_file();
            let mut server = crate::acp::AcpServer::new(session, overlay_path)?;
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            server.serve(stdin.lock(), stdout.lock())?;
        }
        Command::Paths => unreachable!("handled before loading the diff"),
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

/// The `.gander/config.toml` layer is deprecated along with the rest of the
/// project-local directory (docs/decisions.md D6). It still loads for one
/// release; nudge users toward the committed `gander.toml`.
fn warn_deprecated_config_layer(repo: &std::path::Path) {
    let legacy = repo.join(".gander").join("config.toml");
    if legacy.exists() {
        eprintln!(
            "warning: {} is deprecated and will stop loading in a future release; \
             move it to gander.toml at the repo root or the XDG user config",
            legacy.display()
        );
    }
}

/// `gander paths`: print resolved locations so users can find state, debug
/// endpoint issues, and verify config layering.
fn print_paths(
    paths: &WorkspacePaths,
    repo: &std::path::Path,
    state_override: Option<&std::path::Path>,
) {
    let presence = |path: &std::path::Path| if path.exists() { "present" } else { "absent" };
    println!("workspace root:   {}", paths.workspace_root.display());
    println!("workspace key:    {}", paths.key);
    println!("state dir:        {}", paths.state_dir.display());
    println!("runtime dir:      {}", paths.runtime_dir.display());
    let state_file = state_override
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| paths.state_file());
    println!(
        "review state:     {} ({})",
        state_file.display(),
        presence(&state_file)
    );
    println!(
        "agent overlay:    {} ({})",
        paths.overlay_file().display(),
        presence(&paths.overlay_file())
    );
    println!(
        "acp socket:       {} (per instance)",
        paths.runtime_dir.join("acp-<pid>.sock").display()
    );
    println!("instance registry: {}", paths.registry_dir.display());
    println!("agent log:        {}", paths.agent_log_file().display());
    if let Some(xdg_config) = crate::config::xdg_config_path() {
        println!(
            "user config:      {} ({})",
            xdg_config.display(),
            presence(&xdg_config)
        );
    }
    let project_config = repo.join("gander.toml");
    println!(
        "project config:   {} ({})",
        project_config.display(),
        presence(&project_config)
    );
    let legacy_config = paths.legacy_dir.join("config.toml");
    println!(
        "legacy config:    {} ({}, deprecated)",
        legacy_config.display(),
        presence(&legacy_config)
    );
    println!(
        "legacy state dir: {} ({}, deprecated)",
        paths.legacy_dir.display(),
        presence(&paths.legacy_dir)
    );
}

fn resolve_export_options(
    repo: &std::path::Path,
    config: &Config,
    cli_format: Option<OutputFormat>,
    cli_output: Option<PathBuf>,
    cli_profile: Option<OutputProfile>,
) -> (OutputFormat, TuiArtifactDestination, OutputProfile) {
    let format = cli_format.unwrap_or_else(|| config.artifact.format.into());
    // Default artifact output is stdout (docs/decisions.md D6): files are
    // written only to explicit CLI paths or a configured output dir.
    let destination = cli_output
        .or_else(|| config.artifact.output_path(repo, format.into()))
        .map_or(TuiArtifactDestination::Stdout, TuiArtifactDestination::File);
    let profile = cli_profile.unwrap_or_else(|| config.artifact.profile.into());
    (format, destination, profile)
}

fn resolve_tui_artifact_options(
    repo: &std::path::Path,
    config: &Config,
    cli_mode: Option<TuiArtifactOnQuitArg>,
    cli_format: Option<OutputFormat>,
    cli_profile: Option<OutputProfile>,
    cli_output: Option<PathBuf>,
) -> color_eyre::Result<Option<TuiArtifactRequest>> {
    let mode = cli_mode
        .map(TuiArtifactOnQuitConfig::from)
        .unwrap_or(config.artifact.on_tui_quit);
    let format = cli_format.unwrap_or_else(|| config.artifact.format.into());
    let profile = cli_profile.unwrap_or_else(|| config.artifact.profile.into());
    Ok(match mode {
        TuiArtifactOnQuitConfig::Never => None,
        TuiArtifactOnQuitConfig::Write => {
            let output = cli_output
                .or_else(|| config.artifact.output_path(repo, format.into()))
                .ok_or_else(|| {
                    eyre!(
                        "artifact on-tui-quit `write` needs a destination: \
                         pass --artifact-output or set [artifact] output-dir"
                    )
                })?;
            Some(TuiArtifactRequest {
                format,
                profile,
                destination: TuiArtifactDestination::File(output),
            })
        }
        TuiArtifactOnQuitConfig::Stdout => Some(TuiArtifactRequest {
            format,
            profile,
            destination: TuiArtifactDestination::Stdout,
        }),
    })
}

impl From<ArtifactFormatConfig> for OutputFormat {
    fn from(value: ArtifactFormatConfig) -> Self {
        match value {
            ArtifactFormatConfig::Json => Self::Json,
            ArtifactFormatConfig::Markdown => Self::Markdown,
        }
    }
}

impl From<ArtifactProfileConfig> for OutputProfile {
    fn from(value: ArtifactProfileConfig) -> Self {
        match value {
            ArtifactProfileConfig::Human => Self::Human,
            ArtifactProfileConfig::Agent => Self::Agent,
        }
    }
}

impl From<OutputProfile> for ArtifactProfile {
    fn from(value: OutputProfile) -> Self {
        match value {
            OutputProfile::Human => Self::Human,
            OutputProfile::Agent => Self::Agent,
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

impl From<TuiArtifactOnQuitArg> for TuiArtifactOnQuitConfig {
    fn from(value: TuiArtifactOnQuitArg) -> Self {
        match value {
            TuiArtifactOnQuitArg::Never => Self::Never,
            TuiArtifactOnQuitArg::Write => Self::Write,
            TuiArtifactOnQuitArg::Stdout => Self::Stdout,
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
    fn export_options_default_to_stdout() {
        let repo = tempfile::tempdir().unwrap();
        let config = Config::default();

        let (format, destination, profile) =
            resolve_export_options(repo.path(), &config, None, None, None);

        assert_eq!(format, OutputFormat::Markdown);
        assert_eq!(destination, TuiArtifactDestination::Stdout);
        assert_eq!(profile, OutputProfile::Human);
    }

    #[test]
    fn export_options_use_configured_output_dir() {
        let repo = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.artifact.output_dir = Some(PathBuf::from("artifacts"));

        let (format, destination, profile) = resolve_export_options(
            repo.path(),
            &config,
            Some(OutputFormat::Json),
            None,
            Some(OutputProfile::Agent),
        );

        assert_eq!(format, OutputFormat::Json);
        assert_eq!(
            destination,
            TuiArtifactDestination::File(repo.path().join("artifacts").join("review.json"))
        );
        assert_eq!(profile, OutputProfile::Agent);
    }

    #[test]
    fn export_options_cli_output_wins() {
        let repo = tempfile::tempdir().unwrap();
        let config = Config::default();

        let (_, destination, _) = resolve_export_options(
            repo.path(),
            &config,
            None,
            Some(PathBuf::from("/tmp/out.md")),
            None,
        );

        assert_eq!(
            destination,
            TuiArtifactDestination::File(PathBuf::from("/tmp/out.md"))
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

    #[test]
    fn tui_artifact_defaults_to_stdout_markdown() {
        let repo = tempfile::tempdir().unwrap();
        let config = Config::default();

        let request = resolve_tui_artifact_options(repo.path(), &config, None, None, None, None)
            .unwrap()
            .unwrap();

        assert_eq!(request.format, OutputFormat::Markdown);
        assert_eq!(request.profile, OutputProfile::Human);
        assert_eq!(request.destination, TuiArtifactDestination::Stdout);
    }

    #[test]
    fn tui_artifact_write_uses_configured_output() {
        let repo = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.artifact.on_tui_quit = TuiArtifactOnQuitConfig::Write;
        config.artifact.output_dir = Some(PathBuf::from("artifacts"));

        let request = resolve_tui_artifact_options(repo.path(), &config, None, None, None, None)
            .unwrap()
            .unwrap();

        assert_eq!(request.format, OutputFormat::Markdown);
        assert_eq!(
            request.destination,
            TuiArtifactDestination::File(repo.path().join("artifacts").join("review.md"))
        );
    }

    #[test]
    fn tui_artifact_write_without_destination_errors() {
        let repo = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.artifact.on_tui_quit = TuiArtifactOnQuitConfig::Write;

        let error =
            resolve_tui_artifact_options(repo.path(), &config, None, None, None, None).unwrap_err();

        assert!(error.to_string().contains("--artifact-output"));
    }

    #[test]
    fn tui_artifact_cli_stdout_overrides_config_write() {
        let repo = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.artifact.on_tui_quit = TuiArtifactOnQuitConfig::Write;

        let request = resolve_tui_artifact_options(
            repo.path(),
            &config,
            Some(TuiArtifactOnQuitArg::Stdout),
            Some(OutputFormat::Json),
            Some(OutputProfile::Agent),
            None,
        )
        .unwrap()
        .unwrap();

        assert_eq!(request.format, OutputFormat::Json);
        assert_eq!(request.profile, OutputProfile::Agent);
        assert_eq!(request.destination, TuiArtifactDestination::Stdout);
    }
}
