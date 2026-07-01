mod app;
mod artifact;
mod diff;
mod jj;
mod state;
mod syntax;
mod tui;

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use color_eyre::eyre::WrapErr;

use crate::{
    app::ReviewSession,
    artifact::{ArtifactFormat, write_artifact},
    diff::DiffSet,
    jj::JjCommand,
    state::ReviewState,
};

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Repository root. Defaults to the current directory.
    #[arg(long, global = true)]
    repo: Option<PathBuf>,

    /// jj revision to review.
    #[arg(short, long, default_value = "@", global = true)]
    rev: String,

    /// Hide files matching this glob. Can be repeated.
    #[arg(long = "ignore", global = true)]
    ignore: Vec<String>,

    /// Path to the persistent review state file.
    #[arg(long, global = true)]
    state: Option<PathBuf>,

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
        format: OutputFormat,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Mark all currently visible files as viewed without opening the TUI.
    MarkViewed,
    /// Print a terse summary of the current change.
    Summary,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum OutputFormat {
    Json,
    Markdown,
}

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();
    let repo = cli.repo.unwrap_or(std::env::current_dir()?);
    let jj = JjCommand::new(repo.clone(), cli.rev.clone());

    let diff_text = jj
        .show()
        .wrap_err("failed to read jj change with `jj show --git`")?;
    let mut diff = DiffSet::parse(&diff_text).wrap_err("failed to parse jj git diff")?;
    diff.apply_ignores(&cli.ignore)?;

    let state_path = cli
        .state
        .unwrap_or_else(|| repo.join(".jj-change-viewer").join("state.json"));
    let mut state = ReviewState::load_or_default(&state_path)?;
    let mut session = ReviewSession::new(repo, cli.rev, diff, state.clone());
    session.apply_viewed_state();

    match cli.command.unwrap_or(Command::Tui) {
        Command::Tui => {
            tui::run(&mut session)?;
            state = session.into_state();
            state.save(&state_path)?;
        }
        Command::Export { format, output } => {
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
        Command::Summary => {
            println!("{}", session.summary_line());
            for file in &session.files {
                println!(
                    "{mark} {status:>7} {path} (+{additions}/-{deletions})",
                    mark = if file.viewed { "✓" } else { "•" },
                    status = file.status,
                    path = file.path,
                    additions = file.additions,
                    deletions = file.deletions
                );
            }
        }
    }

    Ok(())
}
