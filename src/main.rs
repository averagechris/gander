mod acp;
mod agent;
mod anchor;
mod app;
mod artifact;
mod clipboard;
mod config;
mod diff;
mod file_tree;
mod fuzzy;
mod generated;
mod jj;
mod mcp;
mod paths;
mod registry;
mod review;
mod state;
mod syntax;
mod tui;
mod walkthrough;
mod web_export;

use std::{io::Write as _, path::PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use color_eyre::eyre::{Context, eyre};
use serde::Serialize;

use crate::{
    anchor::comment_anchor_for_file_lines,
    app::ReviewSession,
    artifact::{
        ArtifactBuildOptions, ArtifactFormat, ArtifactProfile, OwnedReviewArtifact,
        import_json_artifact_into_state, render_artifact_with_options, write_artifact,
        write_artifact_to,
    },
    clipboard::copy_to_clipboard,
    config::{ArtifactFormatConfig, ArtifactProfileConfig, Config, TuiArtifactOnQuitConfig},
    diff::DiffSet,
    generated::{GeneratedMatcher, GeneratedPolicy, GeneratedPreset},
    jj::{JjBackend, JjCliBackend, ReviewTarget},
    paths::{PathsEnv, WorkspacePaths},
    review::SessionTargetSpec,
    state::{
        ActionIntent, CommentKind, CommentState, ReviewState, ReviewTarget as StateReviewTarget,
        WalkthroughStep,
    },
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
    /// Print or copy a prompt-style handoff for a coding agent.
    Handoff {
        #[arg(long, value_enum, default_value_t = HandoffFormat::Markdown)]
        format: HandoffFormat,
        /// Include only unresolved comments and open tasks.
        #[arg(long)]
        only_open: bool,
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Copy the rendered handoff to the clipboard instead of printing it.
        #[arg(long)]
        copy: bool,
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
    /// Serve the review session to agent harnesses as MCP tools on stdio
    /// (rmcp SDK). Routes each tool call to this workspace's live TUI
    /// instance via the instance registry; without one, serves a snapshot.
    Mcp,
    /// Print resolved state/runtime/config locations for this workspace.
    Paths,
    /// Print a terse summary of the current change.
    Summary,
    /// Machine-readable file queries for the current review session.
    Files {
        #[command(subcommand)]
        command: FilesCommand,
    },
    /// Machine-readable hunk queries for the current review session.
    Hunks {
        #[command(subcommand)]
        command: HunksCommand,
    },
    /// Machine-readable comment queries for the current review session.
    Comments {
        #[command(subcommand)]
        command: CommentsCommand,
    },
    /// Durable review session lifecycle commands.
    Reviews {
        #[command(subcommand)]
        command: ReviewsCommand,
    },
    /// Machine-readable task queries for the current review session.
    Tasks {
        #[command(subcommand)]
        command: TasksCommand,
    },
    /// Walkthrough queries and exports over persisted local review state.
    Walkthrough {
        #[command(subcommand)]
        command: WalkthroughCommand,
    },
}

#[derive(Debug, Subcommand)]
enum FilesCommand {
    /// List changed files as JSON.
    List,
}

#[derive(Debug, Subcommand)]
enum HunksCommand {
    /// List hunks as JSON. Optionally narrow to one file.
    List {
        #[arg(long)]
        file: Option<String>,
    },
    /// Show one hunk as JSON by id (`<path>:<index>`, as returned by list).
    Show { id: String },
}

#[derive(Debug, Subcommand)]
enum CommentsCommand {
    /// List comments as JSON.
    List,
    Add {
        #[arg(long, alias = "file")]
        path: String,
        #[arg(long)]
        line: Option<usize>,
        #[arg(long = "end-line")]
        end_line: Option<usize>,
        #[arg(long)]
        body: String,
        #[arg(long, value_enum)]
        kind: Option<CommentKindArg>,
        #[arg(long, value_enum)]
        action: Option<ActionIntentArg>,
    },
    Resolve {
        id: String,
    },
    SetState {
        id: String,
        #[arg(long, value_enum)]
        state: CommentStateArg,
    },
}

#[derive(Debug, Subcommand)]
enum ReviewsCommand {
    Create {
        #[arg(long)]
        title: Option<String>,
    },
    List,
    Show {
        id: String,
    },
}

#[derive(Debug, Subcommand)]
enum TasksCommand {
    /// List tasks as JSON. Currently returns comment-backed todo items.
    List,
    Add {
        #[arg(long)]
        title: String,
        #[arg(long)]
        body: Option<String>,
        #[arg(long, value_enum)]
        action: Option<ActionIntentArg>,
        #[arg(long = "comment")]
        comment: Option<String>,
        #[arg(long, alias = "file")]
        path: Option<String>,
        #[arg(long)]
        line: Option<usize>,
    },
    Complete {
        id: String,
        #[arg(long)]
        summary: Option<String>,
    },
    Reopen {
        id: String,
    },
}

#[derive(Debug, Subcommand)]
enum WalkthroughCommand {
    /// Export persisted walkthroughs as Markdown.
    Export,
    AddStep {
        #[arg(long)]
        title: String,
        #[arg(long = "path", alias = "file")]
        file: Option<String>,
        #[arg(long)]
        line: Option<usize>,
        #[arg(long = "end-line")]
        end_line: Option<usize>,
        #[arg(long)]
        symbol: Option<String>,
        #[arg(long)]
        why: Option<String>,
        #[arg(long)]
        body: Option<String>,
    },
    RemoveStep {
        id: String,
    },
    MoveStep {
        id: String,
        #[arg(long = "to")]
        to: usize,
    },
    Show,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum CommentKindArg {
    Note,
    Issue,
    Question,
    Praise,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum ActionIntentArg {
    Fix,
    Explain,
    Test,
    FollowUp,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum CommentStateArg {
    Draft,
    Todo,
    Resolved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    Json,
    Markdown,
    Html,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum HandoffFormat {
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
    if let Err(error) = run() {
        if is_broken_pipe_report(&error) {
            return Ok(());
        }
        return Err(error);
    }
    Ok(())
}

fn run() -> color_eyre::Result<()> {
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
    // `gander mcp` rebuilds its snapshot session on a dedicated thread
    // (ReviewSession is single-threaded); capture the Send ingredients
    // before they move into the main-thread session below.
    let mcp_ingredients = matches!(command, Command::Mcp).then(|| {
        (
            target.clone(),
            diff.clone(),
            state.clone(),
            config.clone(),
            generated_matcher.clone(),
        )
    });
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
                Some(Box::new(jj.clone())),
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
                if request.format == OutputFormat::Html {
                    let html = web_export::render_html(&session, &state);
                    match request.destination {
                        TuiArtifactDestination::File(path) => std::fs::write(&path, html)
                            .with_context(|| {
                                format!("failed to write artifact {}", path.display())
                            })?,
                        TuiArtifactDestination::Stdout => print!("{html}"),
                    }
                } else {
                    let format = match request.format {
                        OutputFormat::Json => ArtifactFormat::Json,
                        OutputFormat::Markdown => ArtifactFormat::Markdown,
                        OutputFormat::Html => unreachable!(),
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
        }
        Command::Export {
            format,
            output,
            profile,
        } => {
            let (format, destination, profile) =
                resolve_export_options(&repo, &config, format, output, profile);
            if format == OutputFormat::Html {
                let html = web_export::render_html(&session, &state);
                match destination {
                    TuiArtifactDestination::File(path) => std::fs::write(&path, html)
                        .with_context(|| format!("failed to write artifact {}", path.display()))?,
                    TuiArtifactDestination::Stdout => print!("{html}"),
                }
            } else {
                let format = match format {
                    OutputFormat::Json => ArtifactFormat::Json,
                    OutputFormat::Markdown => ArtifactFormat::Markdown,
                    OutputFormat::Html => unreachable!(),
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
        }
        Command::Handoff {
            format,
            only_open,
            output,
            copy,
        } => {
            let format = match format {
                HandoffFormat::Json => ArtifactFormat::Json,
                HandoffFormat::Markdown => ArtifactFormat::Markdown,
            };
            let body = render_artifact_with_options(
                &session,
                format,
                ArtifactProfile::Agent,
                ArtifactBuildOptions { only_open },
            )?;
            if let Some(path) = output {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, &body)
                    .with_context(|| format!("failed to write handoff {}", path.display()))?;
                eprintln!("Wrote handoff to {}", path.display());
                if copy {
                    let method = copy_to_clipboard(&body)?;
                    eprintln!("Copied handoff to clipboard via {method}");
                }
            } else if copy {
                let method = copy_to_clipboard(&body)?;
                eprintln!("Copied handoff to clipboard via {method}");
            } else {
                print!("{body}");
                if !body.ends_with('\n') {
                    println!();
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
            let mut server =
                crate::acp::AcpServer::new(session, overlay_path)?.with_jj(Box::new(jj.clone()));
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            server.serve(stdin.lock(), stdout.lock())?;
        }
        Command::Mcp => {
            let (target, diff, state, config, generated_matcher) =
                mcp_ingredients.expect("captured above for the mcp command");
            let session_repo = repo.clone();
            crate::mcp::run(
                move || {
                    let mut session =
                        ReviewSession::new_with_config(session_repo, target, diff, state, &config);
                    session.annotate_generated_where(|file| {
                        generated_matcher.is_match(&file.path)
                            || crate::generated::diff_content_looks_generated(&file.diff)
                    });
                    session
                },
                Some(Box::new(jj.clone())),
                crate::mcp::GanderMcpParams {
                    overlay_path: workspace_paths.overlay_file(),
                    state_path: state_path.clone(),
                    registry_dir: workspace_paths.registry_dir.clone(),
                    workspace_root: workspace_paths.workspace_root.clone(),
                    target: session.target.clone(),
                    diff_files: session.files.iter().map(|file| file.path.clone()).collect(),
                },
            )?;
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
        Command::Files { command } => match command {
            FilesCommand::List => print_json(&session_files_json(&session))?,
        },
        Command::Hunks { command } => match command {
            HunksCommand::List { file } => {
                print_json(&session_hunks_json(&session, file.as_deref()))?
            }
            HunksCommand::Show { id } => {
                let hunk = session_hunk_json(&session, &id)
                    .ok_or_else(|| eyre!("unknown hunk id `{id}`"))?;
                print_json(&hunk)?;
            }
        },
        Command::Comments { command } => match command {
            CommentsCommand::List => print_json(&session_comments_json(&session))?,
            CommentsCommand::Add {
                path,
                line,
                end_line,
                body,
                kind,
                action,
            } => {
                ensure_diff_file(&session, &path)?;
                let anchor = session
                    .files
                    .iter()
                    .find(|file| file.path == path)
                    .and_then(|file| comment_anchor_for_file_lines(file, line, end_line));
                let spec = session_target_spec(&repo, &session.target);
                let id = review::ensure_session(&mut state, &spec, None).id.clone();
                let idx = state.sessions.iter().position(|s| s.id == id).unwrap();
                let comment = review::add_comment(
                    &mut state.sessions[idx],
                    &mut state.comments,
                    review::NewComment {
                        path,
                        line,
                        end_line,
                        anchor,
                        body,
                        kind: kind.map(Into::into),
                        action: action.map(Into::into),
                    },
                );
                state.save(&state_path)?;
                print_json(&comment)?;
            }
            CommentsCommand::Resolve { id } => {
                let spec = session_target_spec(&repo, &session.target);
                let sid = review::ensure_session(&mut state, &spec, None).id.clone();
                let idx = state.sessions.iter().position(|s| s.id == sid).unwrap();
                let comment =
                    review::resolve_comment(&mut state.sessions[idx], &mut state.comments, &id)?;
                state.save(&state_path)?;
                print_json(&comment)?;
            }
            CommentsCommand::SetState {
                id,
                state: new_state,
            } => {
                let spec = session_target_spec(&repo, &session.target);
                let sid = review::ensure_session(&mut state, &spec, None).id.clone();
                let idx = state.sessions.iter().position(|s| s.id == sid).unwrap();
                let comment = review::set_comment_state(
                    &mut state.sessions[idx],
                    &mut state.comments,
                    &id,
                    new_state.into(),
                )?;
                state.save(&state_path)?;
                print_json(&comment)?;
            }
        },
        Command::Reviews { command } => match command {
            ReviewsCommand::Create { title } => {
                let spec = session_target_spec(&repo, &session.target);
                let review_session =
                    review::ensure_session(&mut state, &spec, title.as_deref()).clone();
                state.save(&state_path)?;
                print_json(&review_session)?;
            }
            ReviewsCommand::List => {
                print_json(&serde_json::json!({ "sessions": review::list_sessions(&state) }))?
            }
            ReviewsCommand::Show { id } => print_json(review::find_session(&state, &id)?)?,
        },
        Command::Tasks { command } => match command {
            TasksCommand::List => {
                let spec = session_target_spec(&repo, &session.target);
                let rs = review::ensure_session(&mut state, &spec, None).clone();
                print_json(
                    &serde_json::json!({ "tasks": review::list_tasks(&rs, &state.comments) }),
                )?;
            }
            TasksCommand::Add {
                title,
                body,
                action,
                comment,
                path,
                line,
            } => {
                if let Some(path) = path.as_deref() {
                    ensure_diff_file(&session, path)?;
                }
                let spec = session_target_spec(&repo, &session.target);
                let rs = review::ensure_session(&mut state, &spec, None);
                let target = path.map(|file| StateReviewTarget {
                    file: Some(file),
                    line,
                    ..StateReviewTarget::default()
                });
                let task = review::add_task(
                    rs,
                    title,
                    body,
                    action.map(Into::into).unwrap_or_default(),
                    comment,
                    target,
                );
                state.save(&state_path)?;
                print_json(&task)?;
            }
            TasksCommand::Complete { id, summary } => {
                let spec = session_target_spec(&repo, &session.target);
                let rs = review::ensure_session(&mut state, &spec, None);
                let task = review::complete_task(rs, &id, summary)?;
                state.save(&state_path)?;
                print_json(&task)?;
            }
            TasksCommand::Reopen { id } => {
                let spec = session_target_spec(&repo, &session.target);
                let rs = review::ensure_session(&mut state, &spec, None);
                let task = review::reopen_task(rs, &id)?;
                state.save(&state_path)?;
                print_json(&task)?;
            }
        },
        Command::Walkthrough { command } => match command {
            WalkthroughCommand::Export => {
                print!("{}", walkthrough::render_walkthroughs_markdown(&state));
            }
            WalkthroughCommand::AddStep {
                title,
                file,
                line,
                end_line,
                symbol,
                why,
                body,
            } => {
                if let Some(file) = file.as_deref() {
                    ensure_diff_file(&session, file)?;
                }
                let spec = session_target_spec(&repo, &session.target);
                let rs = review::ensure_session(&mut state, &spec, None);
                let step = review::add_walkthrough_step(
                    rs,
                    WalkthroughStep {
                        id: String::new(),
                        title: Some(title),
                        body,
                        why,
                        target: StateReviewTarget {
                            file,
                            line,
                            end_line,
                            symbol,
                            ..StateReviewTarget::default()
                        },
                    },
                );
                state.save(&state_path)?;
                print_json(&step)?;
            }
            WalkthroughCommand::RemoveStep { id } => {
                let spec = session_target_spec(&repo, &session.target);
                let rs = review::ensure_session(&mut state, &spec, None);
                let step = review::remove_walkthrough_step(rs, &id)?;
                state.save(&state_path)?;
                print_json(&step)?;
            }
            WalkthroughCommand::MoveStep { id, to } => {
                let spec = session_target_spec(&repo, &session.target);
                let rs = review::ensure_session(&mut state, &spec, None);
                let step = review::move_walkthrough_step(rs, &id, to)?;
                state.save(&state_path)?;
                print_json(&step)?;
            }
            WalkthroughCommand::Show => {
                let spec = session_target_spec(&repo, &session.target);
                let rs = review::ensure_session(&mut state, &spec, None).clone();
                print_json(&serde_json::json!({ "walkthroughs": rs.walkthroughs }))?;
            }
        },
    }

    Ok(())
}

fn print_json(value: &impl Serialize) -> color_eyre::Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    serde_json::to_writer_pretty(&mut stdout, value)?;
    stdout.write_all(b"\n")?;
    Ok(())
}

fn is_broken_pipe_report(error: &color_eyre::Report) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe)
            || cause
                .downcast_ref::<serde_json::Error>()
                .and_then(serde_json::Error::io_error_kind)
                .is_some_and(|kind| kind == std::io::ErrorKind::BrokenPipe)
    })
}

fn session_target_spec(repo: &std::path::Path, target: &ReviewTarget) -> SessionTargetSpec {
    SessionTargetSpec {
        repo: Some(repo.display().to_string()),
        base: Some(target.base.clone()),
        revision: Some(target.rev.clone()),
        revset: Some(target.to_string()),
    }
}

fn ensure_diff_file(session: &ReviewSession, path: &str) -> color_eyre::Result<()> {
    if session.files.iter().any(|file| file.path == path) {
        Ok(())
    } else {
        Err(eyre!("`{path}` is not a file in the current diff"))
    }
}

impl From<CommentKindArg> for CommentKind {
    fn from(value: CommentKindArg) -> Self {
        match value {
            CommentKindArg::Note => Self::Note,
            CommentKindArg::Issue => Self::Issue,
            CommentKindArg::Question => Self::Question,
            CommentKindArg::Praise => Self::Praise,
        }
    }
}
impl From<ActionIntentArg> for ActionIntent {
    fn from(value: ActionIntentArg) -> Self {
        match value {
            ActionIntentArg::Fix => Self::Fix,
            ActionIntentArg::Explain => Self::Explain,
            ActionIntentArg::Test => Self::Test,
            ActionIntentArg::FollowUp => Self::FollowUp,
        }
    }
}
impl From<CommentStateArg> for CommentState {
    fn from(value: CommentStateArg) -> Self {
        match value {
            CommentStateArg::Draft => Self::Draft,
            CommentStateArg::Todo => Self::Todo,
            CommentStateArg::Resolved => Self::Resolved,
        }
    }
}

fn session_files_json(session: &ReviewSession) -> serde_json::Value {
    serde_json::json!({
        "files": session.files.iter().map(|file| serde_json::json!({
            "path": file.path,
            "old_path": file.old_path,
            "status": file.status.to_string(),
            "additions": file.additions,
            "deletions": file.deletions,
            "generated": file.generated,
            "viewed": file.viewed,
            "fingerprint": file.fingerprint,
            "hunk_count": file.diff.hunks.len(),
        })).collect::<Vec<_>>()
    })
}

fn hunk_id(path: &str, index: usize) -> String {
    format!("{path}:{index}")
}

fn session_hunks_json(session: &ReviewSession, file_filter: Option<&str>) -> serde_json::Value {
    let hunks = session
        .files
        .iter()
        .filter(|file| file_filter.is_none_or(|filter| file.path == filter))
        .flat_map(|file| {
            file.diff
                .hunks
                .iter()
                .enumerate()
                .map(move |(index, hunk)| {
                    serde_json::json!({
                        "id": hunk_id(&file.path, index),
                        "file": file.path,
                        "index": index,
                        "old_start": hunk.old_start,
                        "old_len": hunk.old_len,
                        "new_start": hunk.new_start,
                        "new_len": hunk.new_len,
                        "header": hunk.header,
                        "line_count": hunk.lines.len(),
                    })
                })
        })
        .collect::<Vec<_>>();
    serde_json::json!({ "hunks": hunks })
}

fn session_hunk_json(session: &ReviewSession, id: &str) -> Option<serde_json::Value> {
    let (path, index) = id.rsplit_once(':')?;
    let index: usize = index.parse().ok()?;
    let file = session.files.iter().find(|file| file.path == path)?;
    let hunk = file.diff.hunks.get(index)?;
    Some(serde_json::json!({
        "id": id,
        "file": file.path,
        "index": index,
        "old_start": hunk.old_start,
        "old_len": hunk.old_len,
        "new_start": hunk.new_start,
        "new_len": hunk.new_len,
        "header": hunk.header,
        "lines": hunk.lines,
    }))
}

fn session_comments_json(session: &ReviewSession) -> serde_json::Value {
    serde_json::json!({ "comments": session.comments })
}

#[cfg(test)]
fn session_tasks_json(session: &ReviewSession) -> serde_json::Value {
    let tasks = session
        .comments
        .iter()
        .filter(|comment| comment.state == crate::state::CommentState::Todo)
        .map(|comment| {
            serde_json::json!({
                "id": comment.id,
                "source": "comment",
                "comment_id": comment.id,
                "path": comment.path,
                "line": comment.line,
                "end_line": comment.end_line,
                "status": "open",
                "kind": comment.kind,
                "action": comment.action,
                "body": comment.body,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({ "tasks": tasks })
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
            ArtifactFormatConfig::Html => Self::Html,
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
            OutputFormat::Html => Self::Html,
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
    use chrono::TimeZone;

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

    #[test]
    fn file_anchor_flags_accept_path_and_file_aliases() {
        assert!(
            Cli::try_parse_from([
                "gander",
                "comments",
                "add",
                "--file",
                "src/lib.rs",
                "--body",
                "note"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "gander",
                "tasks",
                "add",
                "--title",
                "fix",
                "--file",
                "src/lib.rs"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "gander",
                "walkthrough",
                "add-step",
                "--title",
                "read",
                "--path",
                "src/lib.rs",
            ])
            .is_ok()
        );
    }

    #[test]
    fn handoff_command_parses_format_only_open_output_and_copy() {
        let cli = Cli::try_parse_from([
            "gander",
            "handoff",
            "--format",
            "json",
            "--only-open",
            "--output",
            "/tmp/handoff.json",
            "--copy",
        ])
        .unwrap();

        match cli.command.unwrap() {
            Command::Handoff {
                format,
                only_open,
                output,
                copy,
            } => {
                assert_eq!(format, HandoffFormat::Json);
                assert!(only_open);
                assert_eq!(output, Some(PathBuf::from("/tmp/handoff.json")));
                assert!(copy);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn broken_pipe_reports_are_quiet_success() {
        let error = std::io::Error::from(std::io::ErrorKind::BrokenPipe).into();

        assert!(is_broken_pipe_report(&error));
    }

    #[test]
    fn cli_comment_anchor_exports_agent_excerpt() {
        let mut session = sample_session();
        let file = session
            .files
            .iter()
            .find(|file| file.path == "src/lib.rs")
            .unwrap();
        let anchor = comment_anchor_for_file_lines(file, Some(3), None);
        let spec = session_target_spec(&session.repo, &session.target);
        let mut state = ReviewState::default();
        let sid = review::ensure_session(&mut state, &spec, None).id.clone();
        let idx = state.sessions.iter().position(|s| s.id == sid).unwrap();
        review::add_comment(
            &mut state.sessions[idx],
            &mut state.comments,
            review::NewComment {
                path: "src/lib.rs".to_owned(),
                line: Some(3),
                end_line: None,
                anchor,
                body: "explain this".to_owned(),
                kind: None,
                action: None,
            },
        );
        session.comments = state.comments;

        let json = crate::artifact::render_artifact_with_profile(
            &session,
            crate::artifact::ArtifactFormat::Json,
            crate::artifact::ArtifactProfile::Agent,
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(value["comments"][0]["anchor"]["type"], "line");
        assert!(
            !value["comments"][0]["excerpt"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn cli_comment_anchor_derivation_falls_back_to_removed_line() {
        let session = ReviewSession::new(
            PathBuf::from("/repo"),
            ReviewTarget::new("main".to_owned(), "@".to_owned()),
            DiffSet::parse(
                "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1 @@\n keep\n-remove",
            )
            .unwrap(),
            ReviewState::default(),
        );
        let file = session
            .files
            .iter()
            .find(|file| file.path == "src/lib.rs")
            .unwrap();

        let anchor = comment_anchor_for_file_lines(file, Some(2), None).unwrap();

        assert_eq!(anchor.line(), Some(2));
        assert!(matches!(
            anchor,
            crate::anchor::CommentAnchor::Line {
                side: crate::anchor::DiffSide::Old,
                line_kind,
                ..
            } if line_kind == "removed"
        ));
    }

    #[test]
    fn cli_range_comment_anchor_exports_agent_excerpt() {
        let mut session = sample_session();
        let file = session
            .files
            .iter()
            .find(|file| file.path == "src/lib.rs")
            .unwrap();
        let anchor = comment_anchor_for_file_lines(file, Some(3), Some(4));
        assert!(matches!(
            anchor,
            Some(crate::anchor::CommentAnchor::Range { .. })
        ));
        let spec = session_target_spec(&session.repo, &session.target);
        let mut state = ReviewState::default();
        let sid = review::ensure_session(&mut state, &spec, None).id.clone();
        let idx = state.sessions.iter().position(|s| s.id == sid).unwrap();
        review::add_comment(
            &mut state.sessions[idx],
            &mut state.comments,
            review::NewComment {
                path: "src/lib.rs".to_owned(),
                line: Some(3),
                end_line: Some(4),
                anchor,
                body: "explain this range".to_owned(),
                kind: None,
                action: None,
            },
        );
        session.comments = state.comments;

        let json = crate::artifact::render_artifact_with_profile(
            &session,
            crate::artifact::ArtifactFormat::Json,
            crate::artifact::ArtifactProfile::Agent,
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(value["comments"][0]["anchor"]["type"], "range");
        assert!(
            !value["comments"][0]["excerpt"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    fn sample_session() -> ReviewSession {
        let diff = DiffSet::parse(
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,3 @@\n fn main() {\n-    old();\n+    new();\n+    extra();\n }",
        )
        .unwrap();
        let mut state = ReviewState::default();
        state.comments.push(crate::state::Comment {
            id: "c1".to_owned(),
            path: "src/lib.rs".to_owned(),
            line: Some(2),
            end_line: None,
            anchor: None,
            body: "please fix".to_owned(),
            kind: Some(crate::state::CommentKind::Issue),
            action: Some(crate::state::ActionIntent::Fix),
            state: crate::state::CommentState::Todo,
            created_at: chrono::Utc.with_ymd_and_hms(2026, 7, 4, 0, 0, 0).unwrap(),
        });
        ReviewSession::new(
            PathBuf::from("/repo"),
            ReviewTarget::new("main".to_owned(), "@".to_owned()),
            diff,
            state,
        )
    }

    #[test]
    fn files_json_has_stable_list_shape() {
        let json = session_files_json(&sample_session());

        assert_eq!(json["files"][0]["path"], "src/lib.rs");
        assert_eq!(json["files"][0]["status"], "mod");
        assert_eq!(json["files"][0]["hunk_count"], 1);
    }

    #[test]
    fn hunks_json_can_list_and_show_by_id() {
        let session = sample_session();
        let list = session_hunks_json(&session, None);

        assert_eq!(list["hunks"][0]["id"], "src/lib.rs:0");
        let shown = session_hunk_json(&session, "src/lib.rs:0").unwrap();
        assert_eq!(shown["lines"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn comments_and_tasks_json_use_existing_comment_state() {
        let session = sample_session();

        assert_eq!(session_comments_json(&session)["comments"][0]["id"], "c1");
        assert_eq!(session_tasks_json(&session)["tasks"][0]["comment_id"], "c1");
    }
}
