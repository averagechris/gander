mod acp;
mod agent;
mod anchor;
mod app;
mod artifact;
mod clipboard;
mod config;
mod delegation;
mod diff;
mod file_tree;
mod fuzzy;
mod generated;
mod ids;
mod jj;
mod mcp;
mod paths;
mod provenance;
mod registry;
mod review;
mod skills;
mod state;
mod syntax;
mod tui;
mod walkthrough;
mod web_export;

use std::{
    error::Error,
    fmt,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

use clap::{Parser, Subcommand, ValueEnum};
use color_eyre::eyre::{Context, eyre};
use serde::{Deserialize, Serialize};

use crate::{
    agent::{AgentDraft, AgentOverlay, DraftState, chunk_line_space},
    anchor::comment_anchor_for_file_lines,
    app::ReviewSession,
    artifact::{
        ArtifactBuildOptions, ArtifactFormat, ArtifactProfile, OwnedReviewArtifact,
        import_json_artifact_into_state, render_handoff_json, render_handoff_markdown,
        write_artifact, write_artifact_to,
    },
    clipboard::copy_to_clipboard,
    config::{ArtifactFormatConfig, ArtifactProfileConfig, Config, TuiArtifactOnQuitConfig},
    delegation::{DelegationSpec, build_delegation_packet, render_delegation_markdown},
    diff::DiffSet,
    generated::{GeneratedMatcher, GeneratedPolicy, GeneratedPreset},
    jj::{JjBackend, JjCliBackend, ReviewTarget},
    paths::{PathsEnv, WorkspacePaths},
    review::SessionTargetSpec,
    state::{
        ActionIntent, ActionItemStatus, ClosedDisposition, CommentKind, CommentState, ReviewState,
        ReviewTarget as StateReviewTarget, StepArtifact, StepImportance, StepKind, WalkthroughStep,
    },
};

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum ListFormat {
    Json,
    Text,
}

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Durable guided review sessions over jj-visible work.",
    long_about = "Gander is a local-first review workspace for jj-visible work. It reads code state and writes durable review state: viewed files, comments, action items, walkthroughs, and artifacts. The CLI is the normal automation surface; MCP is optional. Run `gander tui` or omit a subcommand to launch the TUI.",
    after_help = "First review loop:\n  gander reviews create --title 'Parser review'\n  gander files list --format text\n  gander comments add --path src/lib.rs --line 42 --body 'Check this invariant'\n  gander action-items add --title 'Add parser regression' --path src/lib.rs --line 42\n  gander walkthrough add-step --title 'Parser flow' --path src/lib.rs --line 42\n  gander handoff --copy"
)]
struct Cli {
    /// Repository root. Defaults to the current directory.
    #[arg(long, global = true, help_heading = "Target & state (global)")]
    repo: Option<PathBuf>,

    /// jj revision to review. Defaults to the working copy commit.
    #[arg(
        short,
        long,
        default_value = "@",
        global = true,
        help_heading = "Target & state (global)"
    )]
    rev: String,

    /// jj revision/revset to compare from. Defaults to trunk().
    #[arg(
        short,
        long,
        default_value = "trunk()",
        global = true,
        help_heading = "Target & state (global)"
    )]
    base: String,

    /// Hide files matching this glob. Can be repeated.
    #[arg(
        long = "ignore",
        global = true,
        help_heading = "Target & state (global)"
    )]
    ignore: Vec<String>,

    /// Treat files matching this generated/noisy preset as generated. Can be repeated.
    #[arg(
        long = "generated-preset",
        value_enum,
        global = true,
        help_heading = "Target & state (global)"
    )]
    generated_preset: Vec<GeneratedPresetArg>,

    /// Treat files matching this glob as generated/noisy. Can be repeated.
    #[arg(
        long = "generated-glob",
        global = true,
        help_heading = "Target & state (global)"
    )]
    generated_glob: Vec<String>,

    /// Path to the persistent review state file.
    /// Named --state-file so subcommands can use --state for domain state
    /// values (for example `comments set-state --state todo`).
    #[arg(
        long = "state-file",
        global = true,
        help_heading = "Target & state (global)"
    )]
    state: Option<PathBuf>,

    /// Path to a gander config file. Layered over XDG user config and a
    /// committed gander.toml at the repo root.
    #[arg(long, global = true, help_heading = "Target & state (global)")]
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
        /// Start directly in the full-screen tour slide deck.
        #[arg(long)]
        tour: bool,
    },
    /// Render the tour slide deck to plain terminal text.
    Tour {
        #[command(subcommand)]
        command: TourCommand,
    },
    /// Export the current review as JSON, Markdown, or HTML.
    #[command(
        after_help = "Examples:\n  gander export markdown --profile agent --output review.md\n      Complete session artifact with all comments and full raw hunks.\n  gander handoff --copy\n      Compact implementation prompt with action items and trimmed reference hunks."
    )]
    Export {
        /// Artifact format to export (json, markdown, or html). Defaults to configured format or markdown.
        #[arg(value_enum)]
        format: Option<OutputFormat>,
        /// Write output to this file instead of stdout.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Artifact profile; agent adds raw hunks and comment excerpts. Not supported with html.
        #[arg(short, long, value_enum)]
        profile: Option<OutputProfile>,
    },
    /// Print or copy a prompt-style handoff for a coding agent.
    #[command(
        long_about = "Print or copy an actionable handoff for a coding agent. Prompt mode renders compact prompt-ready Markdown or JSON from open action items, walkthrough stops, and relevant hunks. Delegate mode requires --mode delegate and emits a typed work packet selected by action-item/comment flags for external harnesses that understand delegation packets.",
        after_help = "Examples:\n  gander handoff --copy\n      Copy prompt-ready Markdown for an implementer agent.\n  gander handoff --format json\n      Emit structured open action items plus walkthrough and reference hunks.\n  gander handoff --mode delegate --action-item abc123 --to coder --objective 'Address this action item'\n      Emit a typed delegation packet for an external harness."
    )]
    Handoff {
        /// Handoff mode: prompt is the legacy implementation prompt; delegate emits a typed work packet.
        #[arg(long, value_enum, default_value_t = HandoffMode::Prompt)]
        mode: HandoffMode,
        /// Handoff output format. Action items are ordered by action priority (fix, test, follow-up, other), then path and line.
        #[arg(long, value_enum, default_value_t = HandoffFormat::Markdown)]
        format: HandoffFormat,
        /// Delegate a specific action item id. Repeat to include multiple action items.
        #[arg(long = "action-item", value_name = "ID")]
        action_items: Vec<String>,
        /// Delegate a specific comment id. Repeat to include multiple comments.
        #[arg(long = "include-comment", value_name = "ID")]
        include_comments: Vec<String>,
        /// Intended recipient label for delegate mode.
        #[arg(long = "to", value_name = "RECIPIENT")]
        to: Option<String>,
        /// Delegate objective.
        #[arg(long, value_name = "TEXT")]
        objective: Option<String>,
        /// Delegate constraint. Repeat to include multiple constraints.
        #[arg(long = "constraint", value_name = "TEXT")]
        constraints: Vec<String>,
        /// Acceptance criterion. Repeat to include multiple criteria.
        #[arg(long = "accept", value_name = "TEXT")]
        acceptance: Vec<String>,
        /// Requested verification note. Repeat to include multiple checks; recorded as inert text.
        #[arg(long = "verify", value_name = "TEXT")]
        verification: Vec<String>,
        /// Write handoff to this file instead of stdout.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Copy the rendered handoff to the clipboard instead of printing it.
        #[arg(long)]
        copy: bool,
    },
    /// Manage bundled agent skills without requiring a repository.
    #[command(
        after_help = "Examples:\n  gander skills list\n  gander skills show gander-review\n  gander skills install --dir .agents/skills\n  gander skills install gander-address-review --force"
    )]
    Skills {
        #[command(subcommand)]
        command: SkillsCommand,
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
    /// Low-level/internal ACP bridge for debugging live review integrations.
    Acp,
    /// Drive the tour/view in a live TUI via its ACP socket (defaults to status).
    #[command(
        after_help = "Examples:\n  gander present\n  gander present next\n  gander present goto --index 3\n  gander present focus --path src/lib.rs --line 42 --end-line 60 --note 'look here'\n\nRequires a live TUI for this workspace; start one with `gander tui --tour`."
    )]
    Present {
        #[command(subcommand)]
        command: Option<PresentCommand>,
        /// Target a specific live TUI instance by pid when several serve this workspace.
        #[arg(long)]
        pid: Option<u32>,
    },
    /// Optional typed/live MCP harness integration on stdio; CLI remains normal automation.
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
    /// Hunk queries for the current review session.
    Hunks {
        #[command(subcommand)]
        command: HunksCommand,
    },
    /// Machine-readable comment queries for the current review session.
    #[command(
        after_help = "Examples:\n  gander comments list --format text\n  gander comments add --path src/lib.rs --line 42 --kind issue --action fix --body 'Check this invariant'\n  gander comments resolve <id> --reply 'Fixed and verified'"
    )]
    Comments {
        #[command(subcommand)]
        command: CommentsCommand,
    },
    /// Create and inspect durable review sessions.
    Reviews {
        #[command(subcommand)]
        command: ReviewsCommand,
    },
    /// Manage durable action items for the current review session.
    #[command(
        after_help = "Examples:\n  gander action-items list --format text\n  gander action-items add --title 'Add regression coverage' --path src/lib.rs --line 42 --action test --comment abc123\n  gander action-items close <id> --disposition completed --outcome 'Added and ran the regression test'"
    )]
    ActionItems {
        #[command(subcommand)]
        command: ActionItemsCommand,
    },
    /// Walkthrough queries and exports over persisted local review state.
    #[command(
        after_help = "Examples:\n  gander walkthrough add-step --title 'Start here' --path src/lib.rs --line 42\n  gander walkthrough set --file tour.json --dry-run\n  gander walkthrough export"
    )]
    Walkthrough {
        #[command(subcommand)]
        command: WalkthroughCommand,
    },
    /// Author agent draft comments from JSON specs.
    #[command(
        long_about = "Author draft comments. Add specs match review/draft_comment params, either one object like {\"path\":\"src/lib.rs\",\"line\":12,\"body\":\"Consider naming this after the invariant.\"} or {\"drafts\":[...]}. Use --file - (or omit --file) to read stdin."
    )]
    Drafts {
        #[command(subcommand)]
        command: DraftsCommand,
    },
}

#[derive(Debug, Subcommand)]
enum TourCommand {
    /// Render tour slides using the production TUI draw path.
    Render {
        /// Render width in terminal columns.
        #[arg(long, default_value_t = 100, value_name = "COLUMNS")]
        width: u16,
        /// Render height in terminal rows.
        #[arg(long, default_value_t = 30, value_name = "ROWS")]
        height: u16,
        /// One-based slide number to render. Omit to render all slides.
        #[arg(long)]
        slide: Option<usize>,
    },
}

#[derive(Debug, Deserialize, Serialize)]
struct WalkthroughSetSpec {
    title: Option<String>,
    steps: Vec<WalkthroughStep>,
}

#[derive(Debug, Subcommand)]
enum DraftsCommand {
    /// List current overlay drafts as pretty JSON, including state.
    List,
    /// Append pending drafts from a JSON spec file (or stdin with --file - / omitted).
    Add {
        #[arg(short, long)]
        file: Option<PathBuf>,
    },
    /// Remove drafts by id. Repeat --id for multiple drafts.
    Remove {
        #[arg(long = "id", required = true)]
        ids: Vec<String>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DraftsSpec {
    One(DraftCommentSpec),
    Many { drafts: Vec<DraftCommentSpec> },
}

#[derive(Debug, Deserialize)]
struct DraftCommentSpec {
    path: String,
    line: Option<usize>,
    body: String,
}

#[derive(Debug, Subcommand)]
enum PresentCommand {
    /// Print live presentation status.
    Status,
    /// Start the tour, like pressing T in the TUI.
    Start,
    /// End the tour.
    End,
    /// Advance to the next slide.
    Next,
    /// Move to the previous slide.
    Prev,
    /// Jump to a slide by zero-based index or durable step id.
    Goto {
        /// Zero-based slide index (compatibility behavior).
        #[arg(
            long,
            conflicts_with = "step",
            required_unless_present = "step",
            value_name = "INDEX"
        )]
        index: Option<usize>,
        /// Durable walkthrough step id to jump to.
        #[arg(long, required_unless_present = "index", value_name = "STEP_ID")]
        step: Option<String>,
    },
    /// Spotlight a diff location in the normal review view.
    Focus {
        #[arg(long, value_name = "PATH")]
        path: String,
        #[arg(long, value_name = "LINE")]
        line: u32,
        #[arg(long)]
        end_line: Option<u32>,
        #[arg(long)]
        note: Option<String>,
    },
    /// Reload review/walkthrough state and rebuild the active tour.
    Reload,
}

#[derive(Debug, Subcommand)]
enum FilesCommand {
    /// List changed files as JSON or compact text.
    List {
        /// Output format for the changed file list.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
}

#[derive(Debug, Subcommand)]
enum HunksCommand {
    /// List hunks as JSON or compact text. Optionally narrow to one file.
    List {
        /// Changed file path to list hunks for.
        file_arg: Option<String>,
        /// Changed file path to list hunks for. Prefer --path; --file remains a hidden alias.
        #[arg(long = "path", alias = "file", conflicts_with = "file_arg")]
        file: Option<String>,
        /// Output format for the hunk list.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
    /// Show one hunk by id (`<path>:<index>`, as returned by list).
    Show {
        /// Hunk id (`<path>:<index>`) to show.
        id: String,
        /// Output format for the hunk (json or diff; text is an alias for diff).
        #[arg(long, value_enum, default_value_t = HunkShowFormat::Json)]
        format: HunkShowFormat,
    },
}

#[derive(Debug, Subcommand)]
enum CommentsCommand {
    /// List comments as JSON or compact text.
    List {
        /// Output format for the comment list.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
    /// Add a durable anchored or general comment.
    Add {
        /// Changed file path to comment on. Alias: --file.
        #[arg(
            long,
            alias = "file",
            required_unless_present = "general",
            conflicts_with = "general"
        )]
        path: Option<String>,
        /// Create a session-level comment with no file anchor.
        #[arg(long, conflicts_with = "path")]
        general: bool,
        /// 1-indexed new-side line number; old-side fallback is used only for removed-only lines.
        #[arg(long, requires = "path", conflicts_with = "general")]
        line: Option<usize>,
        /// 1-indexed inclusive new-side (post-image) end line for a range anchor.
        #[arg(long = "end-line", requires = "line", conflicts_with = "general")]
        end_line: Option<usize>,
        /// Comment body text.
        #[arg(long)]
        body: String,
        /// Comment classification.
        #[arg(long, value_enum)]
        kind: Option<CommentKindArg>,
        /// Suggested action intent for action-item and handoff output.
        #[arg(long, value_enum)]
        action: Option<ActionIntentArg>,
        /// Initial lifecycle state; overrides [comments] initial-state.
        #[arg(long, value_enum, id = "initial-comment-state")]
        state: Option<InitialCommentStateArg>,
        /// Echo format for the added comment.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
    /// Mark selected comments, or every active-session draft, ready as todos.
    Ready {
        /// Comment ids or unique id prefixes to ready.
        #[arg(required_unless_present = "all_drafts", conflicts_with = "all_drafts")]
        ids: Vec<String>,
        /// Ready all draft comments belonging to the active durable session.
        #[arg(long)]
        all_drafts: bool,
        /// Result format.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
    /// Mark a comment resolved by id or unique id prefix, optionally appending a reply first.
    Resolve {
        /// Comment id or unique id prefix.
        id: String,
        /// Reply body to append before resolving.
        #[arg(long)]
        reply: Option<String>,
        /// Echo format for the resolved comment.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
    /// Append a durable reply to a comment by id or unique id prefix.
    Reply {
        /// Comment id or unique id prefix.
        id: String,
        /// Reply body text (must contain non-whitespace text).
        #[arg(long)]
        body: String,
        /// Also mark the parent comment resolved after appending the reply.
        #[arg(long)]
        resolve: bool,
        /// Echo format for the updated comment.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
    /// Change a comment lifecycle state by id or unique id prefix.
    SetState {
        /// Comment id or unique id prefix.
        id: String,
        /// New comment state. Values: draft, todo, resolved.
        #[arg(long, value_enum, id = "comment-state")]
        state: CommentStateArg,
        /// Echo format for the updated comment.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
    /// Edit a durable comment's anchor and/or body.
    Edit {
        /// Comment id or unique id prefix.
        id: String,
        /// New changed file path. Alias: --file.
        #[arg(long, alias = "file")]
        path: Option<String>,
        /// New 1-indexed new-side (post-image) line number.
        #[arg(long, conflicts_with = "start_line")]
        line: Option<usize>,
        /// New 1-indexed inclusive new-side (post-image) start line for a range.
        #[arg(long = "start-line")]
        start_line: Option<usize>,
        /// New 1-indexed inclusive new-side (post-image) end line for a range.
        #[arg(long = "end-line")]
        end_line: Option<usize>,
        /// Replacement comment body text.
        #[arg(long)]
        body: Option<String>,
        /// Replacement comment classification.
        #[arg(long, value_enum)]
        kind: Option<CommentKindArg>,
        /// Replacement suggested action intent; use none to clear it.
        #[arg(long, value_enum)]
        action: Option<ActionIntentArg>,
        /// Echo format for the edited comment.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
    /// Permanently delete a durable comment by id or unique id prefix.
    Delete {
        /// Comment id or unique id prefix.
        id: String,
        /// Echo format for the deleted comment.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
}

#[derive(Debug, Subcommand)]
enum ReviewsCommand {
    /// Create or return the durable review session for the current repo/base/rev.
    Create {
        /// Optional human-readable title for a newly created review session.
        #[arg(long)]
        title: Option<String>,
    },
    /// List durable review sessions as JSON or compact text.
    List {
        /// Output format for the review session list.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
    /// Show one durable review session by id or unique id prefix as JSON.
    Show {
        /// Review session id or unique id prefix.
        id: String,
    },
}

#[derive(Debug, Subcommand)]
enum ActionItemsCommand {
    /// List durable action items as JSON or compact text.
    List {
        /// Output format for the action item list.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
    /// Show one durable action item by id or unique id prefix.
    Show {
        /// Action item id or unique id prefix.
        id: String,
        /// Output format for the action item.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
    /// Add a durable action item.
    Add {
        /// Action item title.
        #[arg(long)]
        title: String,
        /// Optional details/body text.
        #[arg(long)]
        body: Option<String>,
        /// Action intent.
        #[arg(long, value_enum)]
        action: Option<ActionIntentArg>,
        /// Comment id or unique id prefix to link. Repeat for multiple comments.
        #[arg(long = "comment", value_name = "ID")]
        comments: Vec<String>,
        /// Changed file path this action item targets. Alias: --file.
        #[arg(long, alias = "file")]
        path: Option<String>,
        /// 1-indexed new-side (post-image) line number this action item targets.
        #[arg(long, requires = "path")]
        line: Option<usize>,
        /// Echo format for the added action item.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
    /// Edit a durable action item by id or unique id prefix.
    Edit {
        /// Action item id or unique id prefix.
        id: String,
        /// Replacement title.
        #[arg(long)]
        title: Option<String>,
        /// Replacement details/body text.
        #[arg(long)]
        body: Option<String>,
        /// Replacement action intent; use none to clear it.
        #[arg(long, value_enum)]
        action: Option<ActionIntentArg>,
        /// Replacement changed file path. Alias: --file.
        #[arg(long, alias = "file")]
        path: Option<String>,
        /// Replacement 1-indexed new-side line.
        #[arg(long)]
        line: Option<usize>,
        /// Echo format for the edited action item.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
    /// Link one or more comments to an action item.
    LinkComment {
        /// Action item id or unique id prefix.
        id: String,
        /// Comment id or unique id prefix. Repeat for multiple comments.
        #[arg(long = "comment", required = true, value_name = "ID")]
        comments: Vec<String>,
        /// Echo format for the updated action item.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
    /// Unlink one or more comments from an action item.
    UnlinkComment {
        /// Action item id or unique id prefix.
        id: String,
        /// Comment id or unique id prefix. Repeat for multiple comments.
        #[arg(long = "comment", required = true, value_name = "ID")]
        comments: Vec<String>,
        /// Echo format for the updated action item.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
    /// Add an external ticket reference to an action item.
    AddTicket {
        /// Action item id or unique id prefix.
        id: String,
        /// External tracker name.
        #[arg(long)]
        tracker: String,
        /// Opaque external ticket reference.
        #[arg(long)]
        reference: String,
        /// Optional external ticket URL.
        #[arg(long)]
        url: Option<String>,
        /// Echo format for the updated action item.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
    /// Remove an external ticket by full reference or unique reference prefix.
    RemoveTicket {
        /// Action item id or unique id prefix.
        id: String,
        /// External ticket reference or unique prefix.
        reference: String,
        /// Echo format for the updated action item.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
    /// Close an action item with an explicit disposition.
    Close {
        /// Action item id or unique id prefix.
        id: String,
        /// Why the item is being closed.
        #[arg(long, value_enum)]
        disposition: ClosedDispositionArg,
        /// Optional closure outcome.
        #[arg(long)]
        outcome: Option<String>,
        /// Echo format for the closed action item.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
    /// Reopen a closed action item.
    Reopen {
        /// Action item id or unique id prefix.
        id: String,
        /// Echo format for the reopened action item.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
    /// Permanently delete a durable action item.
    Delete {
        /// Action item id or unique id prefix.
        id: String,
        /// Echo format for the deleted action item.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
}

#[derive(Debug, Subcommand)]
enum WalkthroughCommand {
    /// Export persisted walkthroughs as Markdown.
    Export,
    /// Add a walkthrough step for the current review.
    AddStep {
        /// Step title.
        #[arg(long)]
        title: String,
        /// Changed file path this step targets. Alias: --file.
        #[arg(long = "path", alias = "file")]
        file: Option<String>,
        /// 1-indexed new-side (post-image) line number this step targets.
        #[arg(long, requires = "file")]
        line: Option<usize>,
        /// 1-indexed inclusive new-side (post-image) end line for a range target.
        #[arg(long = "end-line", requires = "line")]
        end_line: Option<usize>,
        /// Symbol/function/class name this step targets.
        #[arg(long)]
        symbol: Option<String>,
        /// Why this step matters.
        #[arg(long)]
        why: Option<String>,
        /// Optional step details/body text.
        #[arg(long)]
        body: Option<String>,
        /// Presentation importance in zen mode.
        #[arg(long, value_enum, default_value_t = StepImportanceArg::Spotlight)]
        importance: StepImportanceArg,
        /// jj change id this step belongs to.
        #[arg(long = "change")]
        change_id: Option<String>,
        /// Artifact JSON object; repeat for multiple artifacts.
        #[arg(long = "artifact")]
        artifacts: Vec<String>,
    },
    /// Add a chapter card for a jj change.
    AddChapter {
        /// jj change id for the chapter.
        #[arg(long = "change")]
        change_id: String,
        /// Narrative chapter summary.
        #[arg(long)]
        summary: String,
    },
    /// Replace the current walkthrough from a JSON spec.
    Set {
        /// Spec file, or -/omitted for stdin.
        #[arg(short, long)]
        file: Option<PathBuf>,
        /// Validate and print the would-be replacement without writing state.
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove a walkthrough step by id or unique id prefix.
    RemoveStep {
        /// Walkthrough step id or unique id prefix.
        id: String,
    },
    /// Move a walkthrough step to a zero-based position.
    MoveStep {
        /// Walkthrough step id or unique id prefix.
        id: String,
        /// Zero-based destination index within the walkthrough.
        #[arg(long = "to")]
        to: usize,
    },
    /// Show persisted walkthroughs as JSON.
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
    None,
    Fix,
    Explain,
    Test,
    #[value(alias = "followup")]
    FollowUp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum ClosedDispositionArg {
    Completed,
    Dismissed,
    Deferred,
}

impl From<ClosedDispositionArg> for ClosedDisposition {
    fn from(value: ClosedDispositionArg) -> Self {
        match value {
            ClosedDispositionArg::Completed => Self::Completed,
            ClosedDispositionArg::Dismissed => Self::Dismissed,
            ClosedDispositionArg::Deferred => Self::Deferred,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum StepImportanceArg {
    Spotlight,
    Glance,
}

impl From<StepImportanceArg> for StepImportance {
    fn from(value: StepImportanceArg) -> Self {
        match value {
            StepImportanceArg::Spotlight => StepImportance::Spotlight,
            StepImportanceArg::Glance => StepImportance::Glance,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum CommentStateArg {
    Draft,
    Todo,
    Resolved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum InitialCommentStateArg {
    Draft,
    Todo,
}

impl From<InitialCommentStateArg> for CommentState {
    fn from(value: InitialCommentStateArg) -> Self {
        match value {
            InitialCommentStateArg::Draft => Self::Draft,
            InitialCommentStateArg::Todo => Self::Todo,
        }
    }
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
enum HandoffMode {
    Prompt,
    Delegate,
}

#[derive(Debug, Subcommand)]
enum SkillsCommand {
    /// List bundled skills.
    List {
        /// Output format for bundled skill metadata.
        #[arg(long, value_enum, default_value_t = ListFormat::Text)]
        format: ListFormat,
    },
    /// Show one bundled skill.
    Show {
        /// Bundled skill name from `gander skills list`.
        #[arg(value_name = "NAME")]
        name: String,
        /// Print exact Markdown or a structured JSON envelope.
        #[arg(long, value_enum, default_value_t = SkillShowFormat::Markdown)]
        format: SkillShowFormat,
    },
    /// Install bundled skills to ~/.agents/skills or --dir.
    Install {
        /// Skill names to install. Omit to install every bundled skill.
        #[arg(value_name = "NAME")]
        names: Vec<String>,
        /// Destination root; each skill is written to NAME/SKILL.md.
        #[arg(long = "dir")]
        dir: Option<PathBuf>,
        /// Replace existing skill files.
        #[arg(long)]
        force: bool,
        /// Output format for installed paths.
        #[arg(long, value_enum, default_value_t = ListFormat::Text)]
        format: ListFormat,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum SkillShowFormat {
    Markdown,
    Json,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum HunkShowFormat {
    Json,
    #[value(alias = "text")]
    Diff,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct UserError(String);

impl UserError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for UserError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for UserError {}

fn user_error(message: impl Into<String>) -> color_eyre::Report {
    eyre!(UserError::new(message))
}

fn into_user_error(error: color_eyre::Report) -> color_eyre::Report {
    user_error(error.to_string())
}

fn format_user_error(error: &UserError) -> String {
    format!("error: {}\n", error)
}

fn main() -> color_eyre::Result<()> {
    color_eyre::config::HookBuilder::default()
        .display_location_section(false)
        .install()?;
    if let Err(error) = run() {
        if is_broken_pipe_report(&error) {
            return Ok(());
        }
        if let Some(user_error) = error.downcast_ref::<UserError>() {
            eprint!("{}", format_user_error(user_error));
            std::process::exit(1);
        }
        return Err(error);
    }
    Ok(())
}

fn run() -> color_eyre::Result<()> {
    let mut cli = Cli::parse();
    if let Some(command) = take_skills_command(&mut cli.command) {
        return handle_skills(command);
    }
    let repo = cli
        .repo
        .unwrap_or(std::env::current_dir()?)
        .canonicalize()
        .wrap_err("failed to canonicalize repository path")?;
    let config = Config::load(&repo, cli.config.as_deref())?;
    warn_deprecated_config_layer(&repo);
    let workspace_paths = WorkspacePaths::resolve(&repo, &PathsEnv::from_env())?;
    let command = cli.command.unwrap_or(Command::Tui {
        artifact_on_quit: None,
        artifact_format: None,
        artifact_profile: None,
        artifact_output: None,
        tour: false,
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
        .map_err(|error| user_error(format!("failed to read jj diff for {target}: {error}")))?;
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
        ReviewSession::new_with_config(repo.clone(), target, diff.clone(), state.clone(), &config);
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
            tour,
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
                config.theme,
                tour,
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
        Command::Tour { command } => match command {
            TourCommand::Render {
                width,
                height,
                slide,
            } => {
                if slide == Some(0) {
                    return Err(user_error(
                        "tour render --slide is one-based; pass 1 or greater",
                    ));
                }
                let rendered = tui::render_tour_text(
                    &mut session,
                    &config.keybindings,
                    config.theme,
                    &jj,
                    width,
                    height,
                    slide.map(|n| n - 1),
                )?;
                print!("{rendered}");
            }
        },
        Command::Export {
            format,
            output,
            profile,
        } => {
            if matches!(format, Some(OutputFormat::Html)) && profile.is_some() {
                return Err(user_error(
                    "export --profile is not supported with html output",
                ));
            }
            let (format, destination, profile) =
                resolve_export_options(&repo, &config, format, output, profile);
            let spec = session_target_spec(&repo, &session.target);
            warn_session_target_mismatch(&state, &spec);
            note_if_no_session_for_artifact(&state, &spec);
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
            mode,
            format,
            action_items,
            include_comments,
            to,
            objective,
            constraints,
            acceptance,
            verification,
            output,
            copy,
        } => {
            let spec = session_target_spec(&repo, &session.target);
            warn_session_target_mismatch(&state, &spec);
            note_if_no_session_for_artifact(&state, &spec);
            let delegate_flags = !action_items.is_empty()
                || !include_comments.is_empty()
                || to.is_some()
                || objective.is_some()
                || !constraints.is_empty()
                || !acceptance.is_empty()
                || !verification.is_empty();
            let body = match mode {
                HandoffMode::Prompt => {
                    if delegate_flags {
                        return Err(user_error(
                            "delegate-only flags require `gander handoff --mode delegate`",
                        ));
                    }
                    match format {
                        HandoffFormat::Json => {
                            render_handoff_json(&session, ArtifactBuildOptions { only_open: true })?
                        }
                        HandoffFormat::Markdown => render_handoff_markdown(
                            &session,
                            ArtifactBuildOptions { only_open: true },
                        )?,
                    }
                }
                HandoffMode::Delegate => {
                    let durable = find_current_action_items_session(&state, &spec)?;
                    let objective = objective.unwrap_or_else(|| {
                        "Address the selected Gander review action items and comments.".to_owned()
                    });
                    let spec = DelegationSpec {
                        recipient: to,
                        objective,
                        repeated_constraints: constraints,
                        acceptance_criteria: acceptance,
                        requested_verification: verification,
                        action_item_selectors: action_items,
                        comment_selectors: include_comments,
                        hunk_context_lines: 3,
                    };
                    let packet = build_delegation_packet(&state, durable, &diff, &spec)
                        .map_err(into_user_error)?;
                    if packet.action_items.is_empty() {
                        return Err(user_error(
                            "delegation selected no open action item or comment items",
                        ));
                    }
                    match format {
                        HandoffFormat::Json => serde_json::to_string_pretty(&packet)?,
                        HandoffFormat::Markdown => render_delegation_markdown(&packet),
                    }
                }
            };
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
                if instance.base != session.target.base || instance.rev != session.target.rev {
                    eprintln!(
                        "warning: bridging to live TUI session reviewing {}..{}; requested {} ignored",
                        instance.base, instance.rev, session.target
                    );
                }
                eprintln!(
                    "gander acp: bridged to live TUI session (target {})",
                    session.target
                );
                return crate::acp::socket::bridge_stdio(&instance.socket_path);
            }
            eprintln!("gander acp: serving snapshot (no live TUI for this workspace)");
            let overlay_path = workspace_paths.overlay_file();
            let mut server =
                crate::acp::AcpServer::new(session, overlay_path)?.with_jj(Box::new(jj.clone()));
            let stdin = std::io::stdin();
            let stdout = std::io::stdout();
            server.serve(stdin.lock(), stdout.lock())?;
        }
        Command::Present { command, pid } => {
            #[cfg(not(unix))]
            {
                color_eyre::eyre::bail!("gander present requires Unix sockets and a live TUI");
            }
            #[cfg(unix)]
            {
                let command = command.unwrap_or(PresentCommand::Status);
                if let PresentCommand::Focus {
                    line,
                    end_line: Some(end_line),
                    ..
                } = &command
                    && end_line < line
                {
                    return Err(user_error(
                        "present focus --end-line must be greater than or equal to --line",
                    ));
                }
                let instance = select_present_instance(
                    &workspace_paths.registry_dir,
                    &workspace_paths.workspace_root,
                    pid,
                )?;
                let request = present_request_json(command);
                let response = send_present_request(&instance.socket_path, &request)?;
                println!("{response}");
            }
        }
        Command::Mcp => {
            let (target, diff, state, config, generated_matcher) =
                mcp_ingredients.expect("captured above for the mcp command");
            let session_repo = repo.clone();
            let initial_comment_state = config.comments.initial_state.into();
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
                    initial_comment_state,
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
                    status = file.status.to_string(),
                    generated = if file.generated { "gen" } else { "" },
                    path = file.path,
                    additions = file.additions,
                    deletions = file.deletions
                );
            }
        }
        Command::Drafts { command } => {
            warn_if_live_session_target_differs(&workspace_paths, &session);
            handle_drafts_command(command, &workspace_paths.overlay_file())?
        }
        Command::Files { command } => match command {
            FilesCommand::List { format } => match format {
                ListFormat::Json => print_json(&session_files_json(&session))?,
                ListFormat::Text => print!("{}", session_files_text(&session)),
            },
        },
        Command::Hunks { command } => match command {
            HunksCommand::List {
                file_arg,
                file,
                format,
            } => {
                let file = file.or(file_arg);
                match format {
                    ListFormat::Json => print_json(&session_hunks_json(&session, file.as_deref()))?,
                    ListFormat::Text => print!("{}", session_hunks_text(&session, file.as_deref())),
                }
            }
            HunksCommand::Show { id, format } => match format {
                HunkShowFormat::Json => {
                    let hunk = session_hunk_json(&session, &id)
                        .ok_or_else(|| user_error(format!("unknown hunk id `{id}`")))?;
                    print_json(&hunk)?;
                }
                HunkShowFormat::Diff => {
                    let diff = session_hunk_diff(&session, &id)
                        .ok_or_else(|| user_error(format!("unknown hunk id `{id}`")))?;
                    print!("{diff}");
                }
            },
        },
        Command::Comments { command } => match command {
            CommentsCommand::List { format } => {
                let spec = session_target_spec(&repo, &session.target);
                warn_session_target_mismatch(&state, &spec);
                let active_session_id = review::find_session_for_target(&state, &spec)
                    .map(|review_session| review_session.id.as_str());
                let mut listed_session = session.clone();
                listed_session
                    .comments
                    .retain(|comment| match active_session_id {
                        Some(id) => comment.is_in_session(id),
                        None => comment.session_id.is_none(),
                    });
                match format {
                    ListFormat::Json => print_json(&session_comments_json(&listed_session))?,
                    ListFormat::Text => print!("{}", session_comments_text(&listed_session)),
                }
            }
            CommentsCommand::Add {
                path,
                general: _,
                line,
                end_line,
                body,
                kind,
                action,
                state: initial_state,
                format,
            } => {
                if let (Some(start), Some(end)) = (line, end_line)
                    && end < start
                {
                    return Err(user_error(
                        "comments add --end-line must be greater than or equal to --line",
                    ));
                }
                if let Some(path) = path.as_deref() {
                    ensure_diff_file(&session, path)?;
                }
                let anchor = path.as_deref().and_then(|path| {
                    session
                        .files
                        .iter()
                        .find(|file| file.path == path)
                        .and_then(|file| comment_anchor_for_file_lines(file, line, end_line))
                });
                if let Some(path) = path.as_deref() {
                    warn_if_anchorless_line(path, line, anchor.is_some());
                }
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
                let id = review::ensure_session(&mut state, &spec, None).id.clone();
                let idx = state.sessions.iter().position(|s| s.id == id).unwrap();
                let observation = crate::provenance::CommentObservation::new(
                    provenance_snapshot(&session, &state.sessions[idx]),
                    anchor.clone(),
                );
                let comment = review::add_comment(
                    &mut state.sessions[idx],
                    &mut state.comments,
                    review::NewComment {
                        session_id: id,
                        path,
                        line,
                        end_line,
                        anchor,
                        observation: Some(observation),
                        body,
                        kind: kind.map(Into::into),
                        action: action.and_then(action_intent_arg_to_option),
                        state: initial_state
                            .map(Into::into)
                            .unwrap_or_else(|| config.comments.initial_state.into()),
                    },
                )
                .map_err(into_user_error)?;
                state.save(&state_path)?;
                match format {
                    ListFormat::Json => print_json(&comment)?,
                    ListFormat::Text => print!("{}", comment_echo_text(&comment)),
                }
            }
            CommentsCommand::Ready {
                ids,
                all_drafts,
                format,
            } => {
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
                let sid = review::ensure_session(&mut state, &spec, None).id.clone();
                let idx = state.sessions.iter().position(|s| s.id == sid).unwrap();
                let result = if all_drafts {
                    review::ready_all_drafts(&mut state.sessions[idx], &mut state.comments)
                } else {
                    review::ready_selected_comments(
                        &mut state.sessions[idx],
                        &mut state.comments,
                        &ids,
                    )
                }
                .map_err(into_user_error)?;
                state.save(&state_path)?;
                match format {
                    ListFormat::Json => print_json(&result)?,
                    ListFormat::Text => println!(
                        "readied: {}\nalready ready: {}",
                        result.readied, result.already_ready
                    ),
                }
            }
            CommentsCommand::Resolve { id, reply, format } => {
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
                let sid = review::ensure_session(&mut state, &spec, None).id.clone();
                let idx = state.sessions.iter().position(|s| s.id == sid).unwrap();
                let comment = if let Some(reply) = reply {
                    let snapshot = provenance_snapshot(&session, &state.sessions[idx]);
                    review::reply_and_maybe_resolve_comment(
                        &mut state.sessions[idx],
                        &mut state.comments,
                        &id,
                        reply,
                        true,
                        snapshot,
                    )
                } else {
                    review::resolve_comment(&mut state.sessions[idx], &mut state.comments, &id)
                }
                .map_err(|error| user_error(error.to_string()))?;
                state.save(&state_path)?;
                match format {
                    ListFormat::Json => print_json(&comment)?,
                    ListFormat::Text => print!("{}", comment_echo_text(&comment)),
                }
            }
            CommentsCommand::Reply {
                id,
                body,
                resolve,
                format,
            } => {
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
                let sid = review::ensure_session(&mut state, &spec, None).id.clone();
                let idx = state.sessions.iter().position(|s| s.id == sid).unwrap();
                let snapshot = provenance_snapshot(&session, &state.sessions[idx]);
                let comment = if resolve {
                    review::reply_and_maybe_resolve_comment(
                        &mut state.sessions[idx],
                        &mut state.comments,
                        &id,
                        body,
                        true,
                        snapshot,
                    )
                } else {
                    review::reply_to_comment(
                        &mut state.sessions[idx],
                        &mut state.comments,
                        &id,
                        body,
                        snapshot,
                    )
                }
                .map_err(into_user_error)?;
                state.save(&state_path)?;
                match format {
                    ListFormat::Json => print_json(&comment)?,
                    ListFormat::Text => print!("{}", comment_echo_text(&comment)),
                }
            }
            CommentsCommand::SetState {
                id,
                state: new_state,
                format,
            } => {
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
                let sid = review::ensure_session(&mut state, &spec, None).id.clone();
                let idx = state.sessions.iter().position(|s| s.id == sid).unwrap();
                let comment = review::set_comment_state(
                    &mut state.sessions[idx],
                    &mut state.comments,
                    &id,
                    new_state.into(),
                )
                .map_err(into_user_error)?;
                state.save(&state_path)?;
                match format {
                    ListFormat::Json => print_json(&comment)?,
                    ListFormat::Text => print!("{}", comment_echo_text(&comment)),
                }
            }
            CommentsCommand::Edit {
                id,
                path,
                line,
                start_line,
                end_line,
                body,
                kind,
                action,
                format,
            } => {
                let canonical_id =
                    review::resolve_comment_id(&state.comments, &id).map_err(into_user_error)?;
                let existing = state
                    .comments
                    .iter()
                    .find(|comment| comment.id == canonical_id)
                    .cloned()
                    .expect("resolved comment id must exist");
                let new_line = start_line.or(line);
                if end_line.is_some() && new_line.or(existing.line).is_none() {
                    return Err(user_error(
                        "comments edit --end-line requires an existing or supplied start line",
                    ));
                }
                if let (Some(start), Some(end)) =
                    (new_line.or(existing.line), end_line.or(existing.end_line))
                    && end < start
                {
                    return Err(user_error(
                        "comments edit end line must be greater than or equal to start line",
                    ));
                }
                let effective_path = if let Some(path) = path.as_deref() {
                    ensure_diff_file(&session, path)?;
                    Some(path.to_owned())
                } else {
                    existing.path.clone()
                };
                let anchor_changed =
                    path.is_some() || line.is_some() || start_line.is_some() || end_line.is_some();
                let effective_line = new_line.or(existing.line);
                let effective_end_line = end_line.or(existing.end_line);
                let anchor = anchor_changed.then(|| {
                    effective_path.as_deref().and_then(|effective_path| {
                        session
                            .files
                            .iter()
                            .find(|file| file.path == effective_path)
                            .and_then(|file| {
                                comment_anchor_for_file_lines(
                                    file,
                                    effective_line,
                                    effective_end_line,
                                )
                            })
                    })
                });
                if anchor_changed && let Some(effective_path) = effective_path.as_deref() {
                    warn_if_anchorless_line(
                        effective_path,
                        effective_line,
                        anchor.as_ref().and_then(|anchor| anchor.as_ref()).is_some(),
                    );
                }
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
                let sid = review::ensure_session(&mut state, &spec, None).id.clone();
                let idx = state.sessions.iter().position(|s| s.id == sid).unwrap();
                let comment = review::edit_comment(
                    &mut state.sessions[idx],
                    &mut state.comments,
                    &canonical_id,
                    review::CommentEdits {
                        path: path.or(effective_path),
                        line: (start_line.is_some() || line.is_some()).then_some(new_line),
                        end_line: end_line.map(Some),
                        anchor,
                        body,
                        kind: kind.map(|k| Some(k.into())),
                        action: action.map(action_intent_arg_to_option),
                    },
                )
                .map_err(into_user_error)?;
                state.save(&state_path)?;
                match format {
                    ListFormat::Json => print_json(&comment)?,
                    ListFormat::Text => print!("{}", comment_echo_text(&comment)),
                }
            }
            CommentsCommand::Delete { id, format } => {
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
                let sid = review::ensure_session(&mut state, &spec, None).id.clone();
                let idx = state.sessions.iter().position(|s| s.id == sid).unwrap();
                let comment =
                    review::delete_comment(&mut state.sessions[idx], &mut state.comments, &id)
                        .map_err(into_user_error)?;
                state.save(&state_path)?;
                match format {
                    ListFormat::Json => print_json(&comment)?,
                    ListFormat::Text => print!("{}", comment_echo_text(&comment)),
                }
            }
        },
        Command::Reviews { command } => match command {
            ReviewsCommand::Create { title } => {
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
                let review_session =
                    review::ensure_session(&mut state, &spec, title.as_deref()).clone();
                state.save(&state_path)?;
                print_json(&review_session)?;
            }
            ReviewsCommand::List { format } => match format {
                ListFormat::Json => {
                    print_json(&serde_json::json!({ "sessions": review::list_sessions(&state) }))?
                }
                ListFormat::Text => print!("{}", reviews_text(&state)),
            },
            ReviewsCommand::Show { id } => {
                print_json(review::find_session(&state, &id).map_err(into_user_error)?)?
            }
        },
        Command::ActionItems { command } => match command {
            ActionItemsCommand::List { format } => {
                let spec = session_target_spec(&repo, &session.target);
                warn_session_target_mismatch(&state, &spec);
                let action_items = review::find_session_for_target(&state, &spec)
                    .map(review::list_action_items)
                    .unwrap_or_default();
                match format {
                    ListFormat::Json => {
                        print_json(&serde_json::json!({ "action_items": action_items }))?
                    }
                    ListFormat::Text => print!("{}", action_items_text(&action_items)),
                }
            }
            ActionItemsCommand::Show { id, format } => {
                let spec = session_target_spec(&repo, &session.target);
                warn_session_target_mismatch(&state, &spec);
                let rs = find_current_action_items_session(&state, &spec)?;
                let id = review::resolve_action_item_id(rs, &id).map_err(into_user_error)?;
                print_listed_action_item(listed_action_item(rs, &id), format)?;
            }
            ActionItemsCommand::Add {
                title,
                body,
                action,
                comments,
                path,
                line,
                format,
            } => {
                if let Some(path) = path.as_deref() {
                    ensure_diff_file(&session, path)?;
                }
                let target = path.map(|file| StateReviewTarget {
                    file: Some(file),
                    line,
                    ..StateReviewTarget::default()
                });
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
                let sid = review::ensure_session(&mut state, &spec, None).id.clone();
                let idx = state.sessions.iter().position(|s| s.id == sid).unwrap();
                let item = review::add_action_item(
                    &mut state.sessions[idx],
                    &state.comments,
                    review::NewActionItem {
                        title,
                        body,
                        target,
                        action: action.and_then(action_intent_arg_to_option),
                        comment_selectors: comments,
                        external_tickets: Vec::new(),
                    },
                )
                .map_err(into_user_error)?;
                state.save(&state_path)?;
                print_listed_action_item(
                    listed_action_item(&state.sessions[idx], &item.id),
                    format,
                )?;
            }
            ActionItemsCommand::Edit {
                id,
                title,
                body,
                action,
                path,
                line,
                format,
            } => {
                if let Some(path) = path.as_deref() {
                    ensure_diff_file(&session, path)?;
                }
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
                let sid = review::ensure_session(&mut state, &spec, None).id.clone();
                let idx = state.sessions.iter().position(|s| s.id == sid).unwrap();
                let canonical_id = review::resolve_action_item_id(&state.sessions[idx], &id)
                    .map_err(into_user_error)?;
                let existing_target = state.sessions[idx]
                    .action_items
                    .iter()
                    .find(|item| item.id == canonical_id)
                    .and_then(|item| item.target.clone());
                if line.is_some()
                    && path.is_none()
                    && existing_target
                        .as_ref()
                        .and_then(|target| target.file.as_ref())
                        .is_none()
                {
                    return Err(user_error(
                        "action-items edit --line requires --path or an existing target path",
                    ));
                }
                let target = (path.is_some() || line.is_some()).then(|| {
                    let mut patched = existing_target.unwrap_or_default();
                    if let Some(path) = path {
                        patched.file = Some(path);
                    }
                    if line.is_some() {
                        patched.line = line;
                    }
                    patched
                });
                let item = review::edit_action_item(
                    &mut state.sessions[idx],
                    &canonical_id,
                    review::ActionItemEdits {
                        title,
                        body: body.map(Some),
                        target: target.map(Some),
                        action: action.map(action_intent_arg_to_option),
                    },
                )
                .map_err(into_user_error)?;
                state.save(&state_path)?;
                print_listed_action_item(
                    listed_action_item(&state.sessions[idx], &item.id),
                    format,
                )?;
            }
            ActionItemsCommand::LinkComment {
                id,
                comments,
                format,
            } => {
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
                let sid = review::ensure_session(&mut state, &spec, None).id.clone();
                let idx = state.sessions.iter().position(|s| s.id == sid).unwrap();
                let item =
                    review::link_comment(&mut state.sessions[idx], &state.comments, &id, &comments)
                        .map_err(into_user_error)?;
                state.save(&state_path)?;
                print_listed_action_item(
                    listed_action_item(&state.sessions[idx], &item.id),
                    format,
                )?;
            }
            ActionItemsCommand::UnlinkComment {
                id,
                comments,
                format,
            } => {
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
                let sid = review::ensure_session(&mut state, &spec, None).id.clone();
                let idx = state.sessions.iter().position(|s| s.id == sid).unwrap();
                let item = review::unlink_comment(
                    &mut state.sessions[idx],
                    &state.comments,
                    &id,
                    &comments,
                )
                .map_err(into_user_error)?;
                state.save(&state_path)?;
                print_listed_action_item(
                    listed_action_item(&state.sessions[idx], &item.id),
                    format,
                )?;
            }
            ActionItemsCommand::AddTicket {
                id,
                tracker,
                reference,
                url,
                format,
            } => {
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
                let sid = review::ensure_session(&mut state, &spec, None).id.clone();
                let idx = state.sessions.iter().position(|s| s.id == sid).unwrap();
                let item = review::add_ticket(
                    &mut state.sessions[idx],
                    &id,
                    review::NewExternalTicket {
                        tracker,
                        reference,
                        url,
                    },
                )
                .map_err(into_user_error)?;
                state.save(&state_path)?;
                print_listed_action_item(
                    listed_action_item(&state.sessions[idx], &item.id),
                    format,
                )?;
            }
            ActionItemsCommand::RemoveTicket {
                id,
                reference,
                format,
            } => {
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
                let sid = review::ensure_session(&mut state, &spec, None).id.clone();
                let idx = state.sessions.iter().position(|s| s.id == sid).unwrap();
                let item = review::remove_ticket(&mut state.sessions[idx], &id, &reference)
                    .map_err(into_user_error)?;
                state.save(&state_path)?;
                print_listed_action_item(
                    listed_action_item(&state.sessions[idx], &item.id),
                    format,
                )?;
            }
            ActionItemsCommand::Close {
                id,
                disposition,
                outcome,
                format,
            } => {
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
                let sid = review::ensure_session(&mut state, &spec, None).id.clone();
                let idx = state.sessions.iter().position(|s| s.id == sid).unwrap();
                let item = review::close_action_item(
                    &mut state.sessions[idx],
                    &id,
                    disposition.into(),
                    outcome,
                )
                .map_err(into_user_error)?;
                state.save(&state_path)?;
                print_listed_action_item(
                    listed_action_item(&state.sessions[idx], &item.id),
                    format,
                )?;
            }
            ActionItemsCommand::Reopen { id, format } => {
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
                let sid = review::ensure_session(&mut state, &spec, None).id.clone();
                let idx = state.sessions.iter().position(|s| s.id == sid).unwrap();
                let item = review::reopen_action_item(&mut state.sessions[idx], &id)
                    .map_err(into_user_error)?;
                state.save(&state_path)?;
                print_listed_action_item(
                    listed_action_item(&state.sessions[idx], &item.id),
                    format,
                )?;
            }
            ActionItemsCommand::Delete { id, format } => {
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
                let sid = review::ensure_session(&mut state, &spec, None).id.clone();
                let idx = state.sessions.iter().position(|s| s.id == sid).unwrap();
                let canonical_id = review::resolve_action_item_id(&state.sessions[idx], &id)
                    .map_err(into_user_error)?;
                let selector = listed_action_item(&state.sessions[idx], &canonical_id).selector;
                let item = review::delete_action_item(&mut state.sessions[idx], &canonical_id)
                    .map_err(into_user_error)?;
                let listed = listed_action_item_from(item, selector);
                state.save(&state_path)?;
                print_listed_action_item(listed, format)?;
            }
        },
        Command::Walkthrough { command } => match command {
            WalkthroughCommand::Export => {
                let spec = session_target_spec(&repo, &session.target);
                warn_session_target_mismatch(&state, &spec);
                let mut scoped = state.clone();
                scoped.sessions = review::find_session_for_target(&state, &spec)
                    .cloned()
                    .into_iter()
                    .collect();
                print!("{}", walkthrough::render_walkthroughs_markdown(&scoped));
            }
            WalkthroughCommand::AddStep {
                title,
                file,
                line,
                end_line,
                symbol,
                why,
                body,
                importance,
                change_id,
                artifacts,
            } => {
                if let (Some(start), Some(end)) = (line, end_line)
                    && end < start
                {
                    return Err(user_error(
                        "walkthrough add-step --end-line must be greater than or equal to --line",
                    ));
                }
                if let Some(file) = file.as_deref() {
                    ensure_diff_file(&session, file)?;
                    warn_target_line_space(&session, "new step", file, line)?;
                }
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
                let rs = review::ensure_session(&mut state, &spec, None);
                let step = review::add_walkthrough_step(
                    rs,
                    WalkthroughStep {
                        id: String::new(),
                        title: Some(title),
                        body,
                        why,
                        importance: importance.into(),
                        change_id,
                        artifacts: parse_step_artifacts(&artifacts)?,
                        target: StateReviewTarget {
                            file,
                            line,
                            end_line,
                            symbol,
                            ..StateReviewTarget::default()
                        },
                        ..WalkthroughStep::default()
                    },
                );
                state.save(&state_path)?;
                print_json(&step)?;
            }
            WalkthroughCommand::AddChapter { change_id, summary } => {
                validate_change_ids_for_cli(&session, &jj, std::slice::from_ref(&change_id))?;
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
                let rs = review::ensure_session(&mut state, &spec, None);
                let step = review::add_chapter(rs, change_id, summary, None);
                state.save(&state_path)?;
                print_json(&step)?;
            }
            WalkthroughCommand::Set { file, dry_run } => {
                let mut spec: WalkthroughSetSpec = read_json_spec(file.as_ref(), "walkthrough")?;
                let target_spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &target_spec);
                let rs = review::ensure_session(&mut state, &target_spec, None);
                let replaced = rs
                    .walkthroughs
                    .first()
                    .map(|walkthrough| walkthrough.steps.len())
                    .unwrap_or(0);
                let prior = rs
                    .walkthroughs
                    .first()
                    .map(|walkthrough| walkthrough.steps.as_slice())
                    .unwrap_or(&[]);
                spec.steps = review::preserve_walkthrough_step_ids(prior, spec.steps);
                warn_walkthrough_set_issues(&session, &jj, &spec)?;
                let new_count = spec.steps.len();
                if dry_run {
                    eprintln!(
                        "would replace walkthrough ({replaced} steps) with {new_count} steps"
                    );
                    print_json(&spec)?;
                    return Ok(());
                }
                let walkthrough = review::set_walkthrough(rs, spec.title, spec.steps);
                state.save(&state_path)?;
                eprintln!("replaced walkthrough ({replaced} steps) with {new_count} steps");
                print_json(&walkthrough)?;
            }
            WalkthroughCommand::RemoveStep { id } => {
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
                let rs = review::ensure_session(&mut state, &spec, None);
                let step = review::remove_walkthrough_step(rs, &id).map_err(into_user_error)?;
                state.save(&state_path)?;
                print_json(&step)?;
            }
            WalkthroughCommand::MoveStep { id, to } => {
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
                let rs = review::ensure_session(&mut state, &spec, None);
                let step = review::move_walkthrough_step(rs, &id, to).map_err(into_user_error)?;
                state.save(&state_path)?;
                print_json(&step)?;
            }
            WalkthroughCommand::Show => {
                let spec = session_target_spec(&repo, &session.target);
                warn_session_target_mismatch(&state, &spec);
                let walkthroughs = review::find_session_for_target(&state, &spec)
                    .map(|rs| rs.walkthroughs.clone())
                    .unwrap_or_default();
                print_json(&serde_json::json!({ "walkthroughs": walkthroughs }))?;
            }
        },
        Command::Skills { .. } => {
            unreachable!("skills dispatches before repository initialization")
        }
    }

    Ok(())
}

fn take_skills_command(command: &mut Option<Command>) -> Option<SkillsCommand> {
    if !matches!(command.as_ref(), Some(Command::Skills { .. })) {
        return None;
    }
    match command.take() {
        Some(Command::Skills { command }) => Some(command),
        _ => unreachable!("skills command was checked before taking it"),
    }
}

fn warn_if_live_session_target_differs(paths: &WorkspacePaths, session: &ReviewSession) {
    #[cfg(unix)]
    if let Some(instance) =
        crate::registry::find_live_for_workspace(&paths.registry_dir, &paths.workspace_root)
        && (instance.base != session.target.base || instance.rev != session.target.rev)
    {
        eprintln!(
            "warning: live TUI session is reviewing {}..{}; this command is using {}",
            instance.base, instance.rev, session.target
        );
    }

    #[cfg(not(unix))]
    let _ = (paths, session);
}

fn durable_target_label(target: &StateReviewTarget) -> String {
    match (target.base.as_deref(), target.revision.as_deref()) {
        (Some(base), Some(rev)) => format!("{base}..{rev}"),
        _ => target
            .revset
            .as_deref()
            .unwrap_or("(unspecified)")
            .to_owned(),
    }
}

fn target_spec_label(spec: &review::SessionTargetSpec) -> String {
    match (spec.base.as_deref(), spec.revision.as_deref()) {
        (Some(base), Some(rev)) => format!("{base}..{rev}"),
        _ => spec.revset.as_deref().unwrap_or("(unspecified)").to_owned(),
    }
}

fn warn_session_target_mismatch(state: &ReviewState, spec: &review::SessionTargetSpec) {
    if review::find_session_for_target(state, spec).is_some() {
        return;
    }
    if let Some(other) = review::open_session_for_other_target(state, spec) {
        eprintln!(
            "warning: no open review session matches '{}'; open session \"{}\" targets '{}' (pass -b {} -r {} or gander reviews …)",
            target_spec_label(spec),
            other.title.as_deref().unwrap_or("(untitled)"),
            durable_target_label(&other.target),
            other.target.base.as_deref().unwrap_or("<base>"),
            other.target.revision.as_deref().unwrap_or("<rev>")
        );
    }
}

fn note_if_creating_mismatched_session(state: &ReviewState, spec: &review::SessionTargetSpec) {
    if review::find_session_for_target(state, spec).is_none()
        && let Some(other) = review::open_session_for_other_target(state, spec)
    {
        eprintln!(
            "note: created new session for '{}'; another open session targets '{}'",
            target_spec_label(spec),
            durable_target_label(&other.target)
        );
    }
}

fn note_if_no_session_for_artifact(state: &ReviewState, spec: &review::SessionTargetSpec) {
    if review::find_session_for_target(state, spec).is_none()
        && review::open_session_for_other_target(state, spec).is_none()
    {
        eprintln!(
            "note: no review session for '{}' — action-item/walkthrough sections will be empty",
            target_spec_label(spec)
        );
    }
}

fn print_json(value: &impl Serialize) -> color_eyre::Result<()> {
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    serde_json::to_writer_pretty(&mut stdout, value)?;
    stdout.write_all(b"\n")?;
    Ok(())
}

fn handle_skills(command: SkillsCommand) -> color_eyre::Result<()> {
    match command {
        SkillsCommand::List { format } => {
            let skills = skills::list()?;
            match format {
                ListFormat::Json => print_json(
                    &serde_json::json!({"kind":"gander.skills.list","schema_version":1,"skills":skills}),
                )?,
                ListFormat::Text => {
                    for skill in skills {
                        println!("{}\t{}", skill.name, skill.description);
                    }
                }
            }
        }
        SkillsCommand::Show { name, format } => match format {
            SkillShowFormat::Markdown => {
                print!("{}", skills::show(&name).map_err(into_user_error)?)
            }
            SkillShowFormat::Json => {
                let skill = skills::find_skill(&name).map_err(into_user_error)?;
                print_json(
                    &serde_json::json!({"kind":"gander.skills.show","schema_version":1,"skill":skill.metadata,"markdown":skill.markdown}),
                )?;
            }
        },
        SkillsCommand::Install {
            names,
            dir,
            force,
            format,
        } => {
            let dir = match dir {
                Some(path) => path,
                None => std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|home| home.join(".agents/skills"))
                    .ok_or_else(|| user_error("HOME is unavailable; pass --dir PATH"))?,
            };
            let installed = skills::install(&dir, &names, force).map_err(into_user_error)?;
            match format {
                ListFormat::Json => print_json(
                    &serde_json::json!({"kind":"gander.skills.install","schema_version":1,"installed":installed}),
                )?,
                ListFormat::Text => {
                    for item in installed {
                        println!("installed {} -> {}", item.name, item.path.display());
                    }
                }
            }
        }
    }
    Ok(())
}

fn find_current_action_items_session<'a>(
    state: &'a ReviewState,
    spec: &SessionTargetSpec,
) -> color_eyre::Result<&'a state::ReviewSession> {
    review::find_session_for_target(state, spec).ok_or_else(|| {
        user_error(
            "no existing review session for this target; create action items or comments first",
        )
    })
}

const ARTIFACT_KEYS: &[&str] = &["title", "kind", "body"];
const DRAFT_KEYS: &[&str] = &["path", "line", "body"];
const WALKTHROUGH_KEYS: &[&str] = &["title", "steps"];
const WALKTHROUGH_STEP_KEYS: &[&str] = &[
    "id",
    "target",
    "importance",
    "kind",
    "change_id",
    "title",
    "body",
    "why",
    "artifacts",
    "extra_targets",
    "updated_at",
];
const WALKTHROUGH_TARGET_KEYS: &[&str] = &[
    "repo", "base", "revision", "revset", "file", "line", "end_line", "symbol",
];

fn push_unknown_field_warnings(
    warnings: &mut Vec<String>,
    value: &serde_json::Value,
    at: &str,
    allowed: &[&str],
) {
    if let Some(map) = value.as_object() {
        for key in map.keys() {
            if !allowed.contains(&key.as_str()) {
                warnings.push(format!(
                    "unknown field '{key}' at {at} is ignored — allowed fields: {}",
                    allowed.join(", ")
                ));
            }
        }
    }
}

fn artifact_unknown_field_warnings(
    warnings: &mut Vec<String>,
    value: &serde_json::Value,
    at: &str,
) {
    for (index, artifact) in value
        .get("artifacts")
        .and_then(|artifacts| artifacts.as_array())
        .into_iter()
        .flatten()
        .enumerate()
    {
        push_unknown_field_warnings(
            warnings,
            artifact,
            &format!("{at}.artifacts[{index}]"),
            ARTIFACT_KEYS,
        );
    }
}

fn walkthrough_spec_unknown_fields(value: &serde_json::Value) -> Vec<String> {
    let mut warnings = Vec::new();
    push_unknown_field_warnings(&mut warnings, value, "spec root", WALKTHROUGH_KEYS);
    for (index, step) in value
        .get("steps")
        .and_then(|steps| steps.as_array())
        .into_iter()
        .flatten()
        .enumerate()
    {
        let at = format!("steps[{index}]");
        push_unknown_field_warnings(&mut warnings, step, &at, WALKTHROUGH_STEP_KEYS);
        artifact_unknown_field_warnings(&mut warnings, step, &at);
        if let Some(target) = step.get("target") {
            push_unknown_field_warnings(
                &mut warnings,
                target,
                &format!("{at}.target"),
                WALKTHROUGH_TARGET_KEYS,
            );
        }
        for (target_index, target) in step
            .get("extra_targets")
            .and_then(|targets| targets.as_array())
            .into_iter()
            .flatten()
            .enumerate()
        {
            push_unknown_field_warnings(
                &mut warnings,
                target,
                &format!("{at}.extra_targets[{target_index}]"),
                WALKTHROUGH_TARGET_KEYS,
            );
        }
    }
    warnings
}

fn drafts_spec_unknown_fields(value: &serde_json::Value) -> Vec<String> {
    let mut warnings = Vec::new();
    if value.get("drafts").is_some() {
        push_unknown_field_warnings(&mut warnings, value, "spec root", &["drafts"]);
        for (index, draft) in value
            .get("drafts")
            .and_then(|drafts| drafts.as_array())
            .into_iter()
            .flatten()
            .enumerate()
        {
            push_unknown_field_warnings(
                &mut warnings,
                draft,
                &format!("drafts[{index}]"),
                DRAFT_KEYS,
            );
        }
    } else {
        push_unknown_field_warnings(&mut warnings, value, "spec root", DRAFT_KEYS);
    }
    warnings
}

fn handle_drafts_command(
    command: DraftsCommand,
    overlay_path: &std::path::Path,
) -> color_eyre::Result<()> {
    let mut overlay = AgentOverlay::load_or_default(overlay_path)?;
    match command {
        DraftsCommand::List => print_json(&overlay.drafts)?,
        DraftsCommand::Add { file } => {
            let spec: DraftsSpec = read_json_spec(file.as_ref(), "draft")?;
            let drafts = match spec {
                DraftsSpec::One(draft) => vec![draft],
                DraftsSpec::Many { drafts } => drafts,
            };
            let mut ids = Vec::new();
            for draft in drafts {
                if draft.path.trim().is_empty() {
                    return Err(user_error("path must not be empty"));
                }
                if draft.body.trim().is_empty() {
                    return Err(user_error("body must not be empty"));
                }
                let id = uuid::Uuid::new_v4().to_string();
                overlay.drafts.push(AgentDraft {
                    id: id.clone(),
                    path: draft.path,
                    line: draft.line,
                    body: draft.body,
                    state: DraftState::Pending,
                    accepted_comment_id: None,
                });
                ids.push(id);
            }
            merge_draft_dispositions_from_disk(&mut overlay, overlay_path);
            overlay.save(overlay_path)?;
            print_json(&serde_json::json!({ "ids": ids }))?;
        }
        DraftsCommand::Remove { ids } => {
            let unknown = ids
                .iter()
                .filter(|id| !overlay.drafts.iter().any(|draft| &draft.id == *id))
                .cloned()
                .collect::<Vec<_>>();
            if !unknown.is_empty() {
                return Err(user_error(format!(
                    "unknown draft id(s): {}",
                    unknown.join(", ")
                )));
            }
            overlay.drafts.retain(|draft| !ids.contains(&draft.id));
            overlay.save(overlay_path)?;
            println!("Removed {}; remaining {}", ids.len(), overlay.drafts.len());
        }
    }
    Ok(())
}

fn read_spec_contents(file: Option<&PathBuf>, spec_name: &str) -> color_eyre::Result<String> {
    let mut contents = String::new();
    match file {
        Some(path) if path != std::path::Path::new("-") => {
            contents = std::fs::read_to_string(path).map_err(|error| {
                user_error(format!(
                    "failed to read {spec_name} spec {}: {error}",
                    path.display()
                ))
            })?;
        }
        _ => {
            std::io::stdin().read_to_string(&mut contents)?;
        }
    }
    Ok(contents)
}

fn read_json_spec<T: for<'de> Deserialize<'de>>(
    file: Option<&PathBuf>,
    spec_name: &str,
) -> color_eyre::Result<T> {
    let contents = read_spec_contents(file, spec_name)?;
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) {
        let warnings = match spec_name {
            "draft" => drafts_spec_unknown_fields(&value),
            "walkthrough" => walkthrough_spec_unknown_fields(&value),
            _ => Vec::new(),
        };
        for warning in warnings {
            eprintln!("warning: {warning}");
        }
    }
    serde_json::from_str(&contents)
        .map_err(|error| user_error(format!("failed to parse {spec_name} spec JSON: {error}")))
}

fn validate_change_ids_for_cli(
    session: &ReviewSession,
    jj: &dyn JjBackend,
    change_ids: &[String],
) -> color_eyre::Result<()> {
    let changes = jj.stack_changes(&session.repo, &session.target)?;
    let invalid: Vec<_> = change_ids
        .iter()
        .filter(|id| !changes.iter().any(|change| change.change_id == id.trim()))
        .cloned()
        .collect();
    if !invalid.is_empty() {
        return Err(user_error(format!(
            "unknown change id(s): {}",
            invalid.join(", ")
        )));
    }
    Ok(())
}

fn parse_step_artifacts(values: &[String]) -> color_eyre::Result<Vec<StepArtifact>> {
    values
        .iter()
        .map(|value| {
            serde_json::from_str(value)
                .map_err(|error| user_error(format!("invalid artifact JSON: {error}")))
        })
        .collect()
}

fn warn_walkthrough_set_issues(
    session: &ReviewSession,
    jj: &dyn JjBackend,
    spec: &WalkthroughSetSpec,
) -> color_eyre::Result<()> {
    let chapter_ids: Vec<String> = spec
        .steps
        .iter()
        .filter(|step| step.kind == StepKind::Chapter)
        .map(|step| {
            step.change_id
                .clone()
                .filter(|id| !id.trim().is_empty())
                .ok_or_else(|| {
                    user_error(format!(
                        "chapter step {} missing change_id",
                        step_label(step)
                    ))
                })
        })
        .collect::<color_eyre::Result<Vec<_>>>()?;
    if !chapter_ids.is_empty() {
        validate_change_ids_for_cli(session, jj, &chapter_ids)?;
    }
    let line_space = chunk_line_space(
        &session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>(),
        None,
    );
    for step in &spec.steps {
        for target in std::iter::once(&step.target).chain(step.extra_targets.iter()) {
            if let Some(file) = target.file.as_deref()
                && !session.files.iter().any(|f| f.path == file)
            {
                eprintln!(
                    "warning: walkthrough step {} targets file not in diff: {file}",
                    step_label(step)
                );
            } else if let (Some(file), Some(line)) = (target.file.as_deref(), target.line) {
                let in_range = line_space
                    .iter()
                    .find(|entry| entry.path == file)
                    .is_some_and(|entry| {
                        entry
                            .hunks
                            .iter()
                            .any(|hunk| line >= hunk.start_line && line <= hunk.end_line)
                    });
                if !in_range {
                    let ranges = line_space
                        .iter()
                        .find(|entry| entry.path == file)
                        .map(|entry| {
                            entry
                                .hunks
                                .iter()
                                .map(|hunk| format!("{}-{}", hunk.start_line, hunk.end_line))
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .filter(|ranges| !ranges.is_empty())
                        .unwrap_or_else(|| "none".to_owned());
                    eprintln!(
                        "warning: walkthrough step {} targets {file}:{line} outside diff line space; valid ranges: {ranges}",
                        step_label(step)
                    );
                }
            }
        }
    }
    Ok(())
}

fn step_label(step: &WalkthroughStep) -> &str {
    if !step.id.is_empty() {
        &step.id
    } else {
        step.title.as_deref().unwrap_or("<untitled>")
    }
}

fn warn_target_line_space(
    session: &ReviewSession,
    label: &str,
    file: &str,
    line: Option<usize>,
) -> color_eyre::Result<()> {
    let Some(line) = line else { return Ok(()) };
    let files = session
        .files
        .iter()
        .map(|file| file.diff.clone())
        .collect::<Vec<_>>();
    let line_space = chunk_line_space(&files, Some(file));
    let in_range = line_space.first().is_some_and(|entry| {
        entry
            .hunks
            .iter()
            .any(|hunk| line >= hunk.start_line && line <= hunk.end_line)
    });
    if !in_range {
        let ranges = line_space
            .first()
            .map(|entry| {
                entry
                    .hunks
                    .iter()
                    .map(|hunk| format!("{}-{}", hunk.start_line, hunk.end_line))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .filter(|ranges| !ranges.is_empty())
            .unwrap_or_else(|| "none".to_owned());
        eprintln!(
            "warning: walkthrough step {label} targets {file}:{line} outside diff line space; valid ranges: {ranges}"
        );
    }
    Ok(())
}

fn merge_draft_dispositions_from_disk(overlay: &mut AgentOverlay, overlay_path: &std::path::Path) {
    if let Ok(on_disk) = AgentOverlay::load_or_default(overlay_path) {
        for draft in &mut overlay.drafts {
            if let Some(disk_draft) = on_disk.drafts.iter().find(|disk| disk.id == draft.id)
                && draft.state == DraftState::Pending
            {
                draft.state = disk_draft.state;
                draft.accepted_comment_id = disk_draft.accepted_comment_id.clone();
            }
        }
    }
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

fn provenance_snapshot(
    session: &ReviewSession,
    durable: &crate::state::ReviewSession,
) -> crate::provenance::SnapshotEvidence {
    crate::provenance::SnapshotEvidence::capture(
        chrono::Utc::now(),
        durable.id.clone(),
        durable.target.clone(),
        session.files.iter().map(|file| &file.diff),
    )
}

fn ensure_diff_file(session: &ReviewSession, path: &str) -> color_eyre::Result<()> {
    if session.files.iter().any(|file| file.path == path) {
        Ok(())
    } else {
        Err(user_error(format!(
            "`{path}` is not a file in the current diff"
        )))
    }
}

fn warn_if_anchorless_line(path: &str, line: Option<usize>, anchored: bool) {
    if let Some(line) = line
        && !anchored
    {
        eprintln!(
            "warning: line {line} is not in the current diff for {path}; comment stored without an excerpt anchor — use 'comments edit' to fix"
        );
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
fn action_intent_arg_to_option(value: ActionIntentArg) -> Option<ActionIntent> {
    match value {
        ActionIntentArg::None => None,
        ActionIntentArg::Fix => Some(ActionIntent::Fix),
        ActionIntentArg::Explain => Some(ActionIntent::Explain),
        ActionIntentArg::Test => Some(ActionIntent::Test),
        ActionIntentArg::FollowUp => Some(ActionIntent::FollowUp),
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

fn ellipsize(input: &str, max: usize) -> String {
    let text = input.trim().lines().next().unwrap_or("").trim();
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let keep = max.saturating_sub(1);
    format!("{}…", text.chars().take(keep).collect::<String>())
}

fn action_label_opt(action: Option<ActionIntent>) -> &'static str {
    action_label(action.unwrap_or(ActionIntent::None))
}
fn action_label(action: ActionIntent) -> &'static str {
    match action {
        ActionIntent::Fix => "fix",
        ActionIntent::Explain => "explain",
        ActionIntent::Test => "test",
        ActionIntent::FollowUp => "follow-up",
        ActionIntent::None => "none",
    }
}
fn kind_label(kind: Option<CommentKind>) -> &'static str {
    match kind.unwrap_or(CommentKind::Note) {
        CommentKind::Note => "note",
        CommentKind::Issue => "issue",
        CommentKind::Question => "question",
        CommentKind::Praise => "praise",
    }
}
fn action_item_status_label(status: ActionItemStatus) -> &'static str {
    match status {
        ActionItemStatus::Open => "open",
        ActionItemStatus::Closed => "closed",
    }
}

fn disposition_label(disposition: Option<ClosedDisposition>) -> &'static str {
    match disposition {
        Some(ClosedDisposition::Completed) => "completed",
        Some(ClosedDisposition::Dismissed) => "dismissed",
        Some(ClosedDisposition::Deferred) => "deferred",
        None => "",
    }
}

fn loc(path: &str, line: Option<usize>, end_line: Option<usize>) -> String {
    match (line, end_line) {
        (Some(a), Some(b)) if b != a => format!("{path}:{a}-{b}"),
        (Some(a), _) => format!("{path}:{a}"),
        _ => path.to_owned(),
    }
}

fn comment_loc(path: Option<&str>, line: Option<usize>, end_line: Option<usize>) -> String {
    path.map_or_else(
        || "general/session".to_owned(),
        |path| loc(path, line, end_line),
    )
}

fn session_files_text(session: &ReviewSession) -> String {
    let mut out = String::new();
    for f in &session.files {
        out.push_str(&format!(
            "{} {:<8} {:<48} +{:<4} -{:<4} {:>3} hunk{}{}\n",
            if f.viewed { "✓" } else { "•" },
            f.status.to_string(),
            ellipsize(&f.path, 48),
            f.additions,
            f.deletions,
            f.diff.hunks.len(),
            if f.diff.hunks.len() == 1 { " " } else { "s" },
            if f.generated { " gen" } else { "" }
        ));
    }
    out
}

fn session_hunks_text(session: &ReviewSession, file_filter: Option<&str>) -> String {
    let mut out = String::new();
    for f in session
        .files
        .iter()
        .filter(|f| file_filter.is_none_or(|p| f.path == p))
    {
        for (i, h) in f.diff.hunks.iter().enumerate() {
            let adds = h
                .lines
                .iter()
                .filter(|l| l.kind == crate::diff::DiffLineKind::Added)
                .count();
            let dels = h
                .lines
                .iter()
                .filter(|l| l.kind == crate::diff::DiffLineKind::Removed)
                .count();
            let first = h
                .lines
                .iter()
                .find(|l| l.kind != crate::diff::DiffLineKind::Meta)
                .map(|l| l.text.as_str())
                .unwrap_or("");
            out.push_str(&format!(
                "{:<48} +{:<3} -{:<3} {:<32} {}\n",
                ellipsize(&hunk_id(&f.path, i), 48),
                adds,
                dels,
                ellipsize(&h.header, 32),
                ellipsize(first, 60)
            ));
        }
    }
    out
}

fn session_comments_text(session: &ReviewSession) -> String {
    let mut out = String::new();
    for c in &session.comments {
        out.push_str(&format!(
            "{:<8} [{:<8}] {:<20} {:<36} r{:<2} {}\n",
            &c.id[..c.id.len().min(8)],
            c.state.label(),
            format!("[{}/{}]", kind_label(c.kind), action_label_opt(c.action)),
            ellipsize(&comment_loc(c.path.as_deref(), c.line, c.end_line), 36),
            c.replies.len(),
            ellipsize(&c.body, 80)
        ));
    }
    out
}

fn action_items_text(action_items: &[review::ListedActionItem]) -> String {
    let mut out = String::new();
    for item in action_items {
        let location = item
            .target
            .as_ref()
            .and_then(|x| x.file.as_ref().map(|p| loc(p, x.line, x.end_line)))
            .unwrap_or_default();
        let linked = item
            .comment_ids
            .iter()
            .map(|id| &id[..id.len().min(8)])
            .collect::<Vec<_>>()
            .join(",");
        out.push_str(&format!(
            "{:<12} {:<6} {:<10} [{:<9}] {:<48} {:<28} {}\n",
            item.selector,
            action_item_status_label(item.status),
            disposition_label(item.disposition),
            action_label_opt(item.action),
            ellipsize(&item.title, 48),
            ellipsize(&location, 32),
            linked
        ));
    }
    out
}

fn reviews_text(state: &ReviewState) -> String {
    let mut out = String::new();
    for s in review::list_sessions(state) {
        out.push_str(&format!(
            "{:<12} {:<9} {:<36} {:<28} {} action item(s), {} walkthrough(s)\n",
            s.selector,
            format!("{:?}", s.status).to_lowercase(),
            ellipsize(s.title.as_deref().unwrap_or("(untitled)"), 36),
            ellipsize(
                s.target
                    .revset
                    .as_deref()
                    .or(s.target.revision.as_deref())
                    .unwrap_or(""),
                28
            ),
            s.action_item_count,
            s.walkthrough_count
        ));
    }
    out
}

fn comment_echo_text(c: &crate::state::Comment) -> String {
    if c.is_general() {
        return format!(
            "id: {}\nstate/kind/action: {}/{}/{}\nlocation: general/session\n",
            c.id,
            c.state.label(),
            kind_label(c.kind),
            action_label_opt(c.action),
        );
    }
    let anchored = c
        .anchor
        .as_ref()
        .map(|a| {
            let first = match a {
                crate::anchor::CommentAnchor::File { .. } => "",
                crate::anchor::CommentAnchor::Line { line_text, .. } => line_text.as_str(),
                crate::anchor::CommentAnchor::Range { lines, .. } => {
                    lines.first().map(|l| l.line_text.as_str()).unwrap_or("")
                }
            };
            format!("anchored: {}", ellipsize(first, 80))
        })
        .unwrap_or_else(|| "no anchor ⚠".to_owned());
    format!(
        "id: {}\nstate/kind/action: {}/{}/{}\nanchor: {} ({})\n",
        c.id,
        c.state.label(),
        kind_label(c.kind),
        action_label_opt(c.action),
        comment_loc(c.path.as_deref(), c.line, c.end_line),
        anchored
    )
}

fn listed_action_item(session: &state::ReviewSession, id: &str) -> review::ListedActionItem {
    review::list_action_items(session)
        .into_iter()
        .find(|item| item.id == id)
        .expect("listed action item must exist")
}

fn listed_action_item_from(
    item: crate::state::ActionItem,
    selector: String,
) -> review::ListedActionItem {
    review::ListedActionItem {
        id: item.id,
        selector,
        title: item.title,
        body: item.body,
        status: item.status,
        action: item.action,
        target: item.target,
        comment_ids: item.comment_ids,
        external_tickets: item.external_tickets,
        disposition: item.disposition,
        outcome: item.outcome,
        closed_at: item.closed_at,
        created_at: item.created_at,
        updated_at: item.updated_at,
    }
}

fn print_listed_action_item(
    item: review::ListedActionItem,
    format: ListFormat,
) -> color_eyre::Result<()> {
    match format {
        ListFormat::Json => print_json(&item),
        ListFormat::Text => {
            print!("{}", action_item_echo_text(&item));
            Ok(())
        }
    }
}

fn action_item_echo_text(item: &review::ListedActionItem) -> String {
    let location = item
        .target
        .as_ref()
        .and_then(|x| x.file.as_ref().map(|p| loc(p, x.line, x.end_line)))
        .unwrap_or_else(|| "(no anchor)".to_owned());
    let body_line = item
        .body
        .as_deref()
        .and_then(|body| body.lines().next())
        .filter(|line| !line.trim().is_empty())
        .map(|line| format!("body: {}\n", ellipsize(line, 100)))
        .unwrap_or_default();
    format!(
        "id: {}\nselector: {}\nstatus/disposition/action: {}/{}/{}\ntitle: {} ({})\n{}",
        item.id,
        item.selector,
        action_item_status_label(item.status),
        disposition_label(item.disposition),
        action_label_opt(item.action),
        item.title,
        location,
        body_line
    )
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

fn session_hunk_diff(session: &ReviewSession, id: &str) -> Option<String> {
    let (path, index) = id.rsplit_once(':')?;
    let index: usize = index.parse().ok()?;
    let file = session.files.iter().find(|file| file.path == path)?;
    let hunk = file.diff.hunks.get(index)?;
    let old_path = file.old_path.as_deref().unwrap_or(&file.path);
    let mut out = format!("--- a/{old_path}\n+++ b/{}\n{}\n", file.path, hunk.header);
    for line in &hunk.lines {
        let prefix = match line.kind {
            crate::diff::DiffLineKind::Added => '+',
            crate::diff::DiffLineKind::Removed => '-',
            crate::diff::DiffLineKind::Context => ' ',
            crate::diff::DiffLineKind::Meta => '\\',
        };
        out.push(prefix);
        out.push_str(&line.text);
        out.push('\n');
    }
    Some(out)
}

fn session_comments_json(session: &ReviewSession) -> serde_json::Value {
    let ids = session
        .comments
        .iter()
        .map(|comment| comment.id.as_str())
        .collect::<Vec<_>>();
    let comments = session
        .comments
        .iter()
        .map(|comment| {
            let mut value = serde_json::to_value(comment).expect("comments serialize");
            if let serde_json::Value::Object(object) = &mut value {
                let selector = ids::shortest_unique_prefix(&comment.id, &ids);
                object.insert("selector".into(), selector.clone().into());
                object.insert("short_id".into(), selector.into());
            }
            value
        })
        .collect::<Vec<_>>();
    serde_json::json!({ "comments": comments })
}

#[cfg(unix)]
fn select_present_instance(
    registry_dir: &Path,
    workspace_root: &Path,
    pid: Option<u32>,
) -> color_eyre::Result<crate::registry::InstanceInfo> {
    let workspace_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    let matches: Vec<_> = crate::registry::live_instances(registry_dir)
        .into_iter()
        .filter(|instance| instance.workspace_root == workspace_root)
        .collect();
    if let Some(pid) = pid {
        return matches
            .into_iter()
            .find(|instance| instance.pid == pid)
            .ok_or_else(|| user_error(format!("no live TUI with pid {pid} for this workspace")));
    }
    match matches.as_slice() {
        [] => Err(user_error(
            "no live TUI for this workspace; start one with `gander tui --tour` and retry",
        )),
        [one] => Ok(one.clone()),
        many => {
            let list = many
                .iter()
                .map(|instance| {
                    format!(
                        "  pid {}: {}..{} ({})",
                        instance.pid, instance.base, instance.rev, instance.summary
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            Err(user_error(format!(
                "multiple live TUIs serve this workspace; pass --pid:\n{list}"
            )))
        }
    }
}

fn present_request_json(command: PresentCommand) -> serde_json::Value {
    match command {
        PresentCommand::Status => {
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"present/status"})
        }
        PresentCommand::Start => {
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"present/start"})
        }
        PresentCommand::End => serde_json::json!({"jsonrpc":"2.0","id":1,"method":"present/end"}),
        PresentCommand::Next => serde_json::json!({"jsonrpc":"2.0","id":1,"method":"present/next"}),
        PresentCommand::Prev => serde_json::json!({"jsonrpc":"2.0","id":1,"method":"present/prev"}),
        PresentCommand::Reload => {
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"present/reload"})
        }
        PresentCommand::Goto { index, step } => {
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"present/goto","params":{"index":index,"step_id":step}})
        }
        PresentCommand::Focus {
            path,
            line,
            end_line,
            note,
        } => {
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"present/focus","params":{"path":path,"line":line,"end_line":end_line,"note":note}})
        }
    }
}

#[cfg(unix)]
fn send_present_request(
    socket_path: &Path,
    request: &serde_json::Value,
) -> color_eyre::Result<String> {
    use std::io::{BufRead, Write};
    use std::os::unix::net::UnixStream;
    let mut stream = UnixStream::connect(socket_path)
        .with_context(|| format!("failed to connect to {}", socket_path.display()))?;
    writeln!(stream, "{request}")?;
    stream.flush()?;
    let mut response = String::new();
    std::io::BufReader::new(stream).read_line(&mut response)?;
    Ok(response.trim_end().to_owned())
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
    if matches!(format, OutputFormat::Html) && cli_profile.is_some() {
        return Err(user_error(
            "tui --artifact-profile is not supported with html artifacts",
        ));
    }
    let profile = cli_profile.unwrap_or_else(|| config.artifact.profile.into());
    Ok(match mode {
        TuiArtifactOnQuitConfig::Never => None,
        TuiArtifactOnQuitConfig::Write => {
            let output = cli_output
                .or_else(|| config.artifact.output_path(repo, format.into()))
                .ok_or_else(|| {
                    user_error(
                        "artifact on-tui-quit `write` needs a destination: \
                         pass --artifact-output or set [artifact] output-dir",
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
    use clap::CommandFactory;

    #[test]
    fn draft_spec_unknown_fields_are_warned() {
        let draft_value: serde_json::Value = serde_json::from_str(
            r#"{"drafts":[{"path":"src/lib.rs","line":3,"body":"b","severity":"major"}]}"#,
        )
        .unwrap();
        let warnings = drafts_spec_unknown_fields(&draft_value);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("unknown field 'severity' at drafts[0]"));
    }

    #[test]
    fn user_error_formatting_is_plain_and_preserves_multiline_details() {
        let error = UserError::new("invalid chunk part(s):\n- src/lib.rs:99-100 outside diff");

        assert_eq!(
            format_user_error(&error),
            "error: invalid chunk part(s):\n- src/lib.rs:99-100 outside diff\n"
        );
    }

    #[test]
    fn user_error_reports_can_be_downcast_by_main() {
        let report = user_error("unknown draft id(s): nope");

        assert_eq!(
            report.downcast_ref::<UserError>().map(ToString::to_string),
            Some("unknown draft id(s): nope".to_owned())
        );
    }

    #[test]
    fn drafts_cli_adds_and_strictly_removes() {
        let dir = tempfile::tempdir().unwrap();
        let overlay_path = dir.path().join("agent.json");
        let spec_path = dir.path().join("draft.json");
        std::fs::write(
            &spec_path,
            r#"{"path":"src/lib.rs","line":12,"body":"Please check this."}"#,
        )
        .unwrap();

        handle_drafts_command(
            DraftsCommand::Add {
                file: Some(spec_path),
            },
            &overlay_path,
        )
        .unwrap();
        let overlay = AgentOverlay::load_or_default(&overlay_path).unwrap();
        assert_eq!(overlay.drafts.len(), 1);
        assert_eq!(overlay.drafts[0].state, DraftState::Pending);
        let id = overlay.drafts[0].id.clone();

        let error = handle_drafts_command(
            DraftsCommand::Remove {
                ids: vec!["missing".to_owned()],
            },
            &overlay_path,
        )
        .unwrap_err();
        assert!(error.to_string().contains("unknown draft id(s): missing"));

        handle_drafts_command(DraftsCommand::Remove { ids: vec![id] }, &overlay_path).unwrap();
        assert!(
            AgentOverlay::load_or_default(&overlay_path)
                .unwrap()
                .drafts
                .is_empty()
        );
    }

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
                "action-items",
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
    fn full_cli_debug_asserts() {
        Cli::command().debug_assert();
    }

    #[test]
    fn early_skills_dispatch_does_not_consume_other_commands() {
        let mut ordinary = Some(Command::Summary);
        assert!(take_skills_command(&mut ordinary).is_none());
        assert!(matches!(ordinary, Some(Command::Summary)));

        let mut skills = Some(Command::Skills {
            command: SkillsCommand::List {
                format: ListFormat::Text,
            },
        });
        assert!(matches!(
            take_skills_command(&mut skills),
            Some(SkillsCommand::List {
                format: ListFormat::Text
            })
        ));
        assert!(skills.is_none());
    }

    #[test]
    fn help_contains_stable_cli_consistency_text() {
        fn help_for(path: &[&str]) -> String {
            let mut cmd = Cli::command();
            let mut current = &mut cmd;
            for name in path {
                current = current.find_subcommand_mut(name).unwrap();
            }
            current.render_long_help().to_string()
        }

        let top = help_for(&[]);
        assert!(top.contains("local-first review workspace for jj-visible work"));
        assert!(top.contains("Run `gander tui` or omit a subcommand to launch the TUI"));
        assert!(top.contains("First review loop:"));
        assert!(!top.contains("chunks"));
        assert!(!top.contains("briefs"));
        assert!(!top.to_lowercase().contains("task"));

        for (path, needles) in [
            (
                &["handoff"][..],
                &["Prompt mode", "Delegate mode"] as &[&str],
            ),
            (
                &["skills"][..],
                &["List bundled skills", "Show one bundled skill"],
            ),
            (&["present", "goto"][..], &["zero-based", "STEP_ID"]),
            (
                &["comments", "add"][..],
                &["--path", "--end-line", "new-side"],
            ),
            (
                &["action-items", "add"][..],
                &["--path", "--line", "Action intent"],
            ),
            (
                &["walkthrough", "add-step"][..],
                &["--path", "--end-line", "Symbol"],
            ),
            (
                &["tour", "render"][..],
                &["COLUMNS", "ROWS", "One-based slide"],
            ),
        ] {
            let help = help_for(path);
            for needle in needles {
                assert!(
                    help.contains(needle),
                    "{path:?} help missing {needle:?}\n{help}"
                );
            }
        }
    }

    #[test]
    fn stale_skill_literals_are_not_canonical_or_parseable() {
        let skills = [
            include_str!("../skills/gander-review/SKILL.md"),
            include_str!("../skills/gander-address-review/SKILL.md"),
        ]
        .join("\n");
        for stale in [
            "comments list --json",
            "tasks list --json",
            "walkthrough show --json",
            "gander review ",
            "--revset",
            "gander chunks",
            "gander briefs",
            "--body -",
        ] {
            assert!(
                !skills.contains(stale),
                "stale literal still present: {stale}"
            );
        }
        for command in [
            ["gander", "comments", "list", "--json"].as_slice(),
            ["gander", "tasks", "list", "--json"].as_slice(),
            ["gander", "walkthrough", "show", "--json"].as_slice(),
        ] {
            assert!(Cli::try_parse_from(command).is_err());
        }
    }

    #[test]
    fn comment_add_and_ready_clap_constraints_are_enforced() {
        assert!(
            Cli::try_parse_from([
                "gander",
                "comments",
                "add",
                "--general",
                "--body",
                "overall note"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "gander",
                "comments",
                "add",
                "--path",
                "a.rs",
                "--state",
                "draft",
                "--body",
                "line note"
            ])
            .is_ok()
        );
        for args in [
            vec!["gander", "comments", "add", "--body", "missing scope"],
            vec![
                "gander",
                "comments",
                "add",
                "--general",
                "--path",
                "a.rs",
                "--body",
                "bad",
            ],
            vec![
                "gander",
                "comments",
                "add",
                "--general",
                "--line",
                "1",
                "--body",
                "bad",
            ],
            vec![
                "gander",
                "comments",
                "add",
                "--general",
                "--state",
                "resolved",
                "--body",
                "bad",
            ],
            vec!["gander", "comments", "ready"],
            vec!["gander", "comments", "ready", "abcd", "--all-drafts"],
        ] {
            assert!(
                Cli::try_parse_from(&args).is_err(),
                "unexpectedly parsed {args:?}"
            );
        }
        assert!(Cli::try_parse_from(["gander", "comments", "ready", "abcd", "efgh"]).is_ok());
        assert!(Cli::try_parse_from(["gander", "comments", "ready", "--all-drafts"]).is_ok());
    }

    #[test]
    fn none_action_clears_optional_action_intent() {
        assert_eq!(action_intent_arg_to_option(ActionIntentArg::None), None);
        assert_eq!(
            action_intent_arg_to_option(ActionIntentArg::Fix),
            Some(ActionIntent::Fix)
        );
    }

    #[test]
    fn cli_requires_conflicts_and_legacy_aliases_parse() {
        for command in [
            [
                "gander",
                "comments",
                "add",
                "--path",
                "p",
                "--end-line",
                "2",
                "--body",
                "b",
            ]
            .as_slice(),
            [
                "gander",
                "comments",
                "edit",
                "id",
                "--line",
                "1",
                "--start-line",
                "1",
            ]
            .as_slice(),
            [
                "gander",
                "action-items",
                "add",
                "--title",
                "t",
                "--line",
                "1",
            ]
            .as_slice(),
            [
                "gander",
                "walkthrough",
                "add-step",
                "--title",
                "t",
                "--line",
                "1",
            ]
            .as_slice(),
            [
                "gander",
                "walkthrough",
                "add-step",
                "--title",
                "t",
                "--path",
                "p",
                "--end-line",
                "2",
            ]
            .as_slice(),
            ["gander", "present", "goto"].as_slice(),
            ["gander", "present", "goto", "--index", "0", "--step", "abc"].as_slice(),
        ] {
            assert!(
                Cli::try_parse_from(command).is_err(),
                "expected reject: {command:?}"
            );
        }
        for command in [
            ["gander", "comments", "add", "--file", "p", "--body", "b"].as_slice(),
            [
                "gander",
                "action-items",
                "add",
                "--title",
                "t",
                "--file",
                "p",
                "--line",
                "1",
            ]
            .as_slice(),
            ["gander", "hunks", "list", "--file", "p"].as_slice(),
            ["gander", "present", "goto", "--index", "0"].as_slice(),
            ["gander", "present", "goto", "--step", "abc"].as_slice(),
            ["gander", "comments", "edit", "id", "--action", "none"].as_slice(),
        ] {
            assert!(
                Cli::try_parse_from(command).is_ok(),
                "expected accept: {command:?}"
            );
        }
    }

    #[test]
    fn mutating_subcommands_parse() {
        let commands: &[&[&str]] = &[
            &[
                "gander",
                "comments",
                "add",
                "--path",
                "src/lib.rs",
                "--line",
                "1",
                "--body",
                "note",
            ],
            &["gander", "comments", "resolve", "abc"],
            &["gander", "comments", "set-state", "abc", "--state", "todo"],
            &[
                "gander",
                "comments",
                "edit",
                "abc",
                "--path",
                "src/lib.rs",
                "--line",
                "2",
                "--body",
                "updated",
            ],
            &["gander", "comments", "delete", "abc"],
            &[
                "gander",
                "action-items",
                "add",
                "--title",
                "fix",
                "--action",
                "follow-up",
            ],
            &["gander", "action-items", "reopen", "abc"],
            &[
                "gander",
                "walkthrough",
                "add-step",
                "--title",
                "read",
                "--path",
                "src/lib.rs",
                "--line",
                "1",
            ],
            &["gander", "walkthrough", "remove-step", "abc"],
            &["gander", "walkthrough", "move-step", "abc", "--to", "0"],
            &["gander", "reviews", "create", "--title", "Review"],
            &["gander", "drafts", "add", "--file", "-"],
            &["gander", "drafts", "remove", "--id", "abc"],
        ];

        for command in commands {
            Cli::try_parse_from(*command)
                .unwrap_or_else(|error| panic!("failed to parse {command:?}: {error}"));
        }
    }

    #[test]
    fn action_items_cli_accepts_complete_public_surface() {
        let commands: &[&[&str]] = &[
            &["gander", "action-items", "list", "--format", "text"],
            &["gander", "action-items", "show", "abc", "--format", "json"],
            &[
                "gander",
                "action-items",
                "add",
                "--title",
                "Coordinate fixes",
                "--body",
                "Handle both comments",
                "--action",
                "follow-up",
                "--comment",
                "comment-a",
                "--comment",
                "comment-b",
                "--path",
                "src/lib.rs",
                "--line",
                "12",
            ],
            &[
                "gander",
                "action-items",
                "edit",
                "item-a",
                "--title",
                "Updated",
                "--body",
                "Updated body",
                "--action",
                "none",
                "--path",
                "src/main.rs",
                "--line",
                "20",
            ],
            &[
                "gander",
                "action-items",
                "link-comment",
                "item-a",
                "--comment",
                "comment-a",
                "--comment",
                "comment-b",
            ],
            &[
                "gander",
                "action-items",
                "unlink-comment",
                "item-a",
                "--comment",
                "comment-a",
                "--comment",
                "comment-b",
            ],
            &[
                "gander",
                "action-items",
                "add-ticket",
                "item-a",
                "--tracker",
                "linear",
                "--reference",
                "ENG-123",
                "--url",
                "https://linear.example/ENG-123",
            ],
            &["gander", "action-items", "remove-ticket", "item-a", "ENG-1"],
            &[
                "gander",
                "action-items",
                "close",
                "item-a",
                "--disposition",
                "completed",
                "--outcome",
                "Implemented and checked",
            ],
            &[
                "gander",
                "action-items",
                "close",
                "item-a",
                "--disposition",
                "dismissed",
            ],
            &[
                "gander",
                "action-items",
                "close",
                "item-a",
                "--disposition",
                "deferred",
            ],
            &["gander", "action-items", "reopen", "item-a"],
            &["gander", "action-items", "delete", "item-a"],
        ];
        for command in commands {
            Cli::try_parse_from(*command)
                .unwrap_or_else(|error| panic!("failed to parse {command:?}: {error}"));
        }

        let mut command = Cli::command();
        let names = command
            .find_subcommand_mut("action-items")
            .unwrap()
            .get_subcommands()
            .map(clap::Command::get_name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "list",
                "show",
                "add",
                "edit",
                "link-comment",
                "unlink-comment",
                "add-ticket",
                "remove-ticket",
                "close",
                "reopen",
                "delete",
            ]
        );
    }

    #[test]
    fn action_item_required_flags_are_enforced() {
        for command in [
            &["gander", "action-items", "add"][..],
            &["gander", "action-items", "link-comment", "item-a"][..],
            &["gander", "action-items", "unlink-comment", "item-a"][..],
            &[
                "gander",
                "action-items",
                "add-ticket",
                "item-a",
                "--tracker",
                "linear",
            ][..],
            &["gander", "action-items", "remove-ticket", "item-a"][..],
            &["gander", "action-items", "close", "item-a"][..],
        ] {
            assert!(
                Cli::try_parse_from(command).is_err(),
                "unexpectedly parsed {command:?}"
            );
        }
    }

    #[test]
    fn retired_public_commands_are_unknown() {
        for command in [
            &["gander", "chunks"][..],
            &["gander", "briefs"][..],
            &["gander", "tasks"][..],
        ] {
            let error = Cli::try_parse_from(command).unwrap_err();
            assert_eq!(error.kind(), clap::error::ErrorKind::InvalidSubcommand);
        }
    }

    #[test]
    fn handoff_command_parses_format_output_and_copy() {
        let cli = Cli::try_parse_from([
            "gander",
            "handoff",
            "--format",
            "json",
            "--output",
            "/tmp/handoff.json",
            "--copy",
        ])
        .unwrap();

        match cli.command.unwrap() {
            Command::Handoff {
                format,
                output,
                copy,
                ..
            } => {
                assert_eq!(format, HandoffFormat::Json);
                assert_eq!(output, Some(PathBuf::from("/tmp/handoff.json")));
                assert!(copy);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn handoff_only_open_flag_is_unknown() {
        let error = Cli::try_parse_from(["gander", "handoff", "--only-open"]).unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn handoff_uses_repeatable_action_item_selectors_and_rejects_task_flag() {
        let cli = Cli::try_parse_from([
            "gander",
            "handoff",
            "--mode",
            "delegate",
            "--action-item",
            "item-a",
            "--action-item",
            "item-b",
        ])
        .unwrap();
        match cli.command.unwrap() {
            Command::Handoff { action_items, .. } => {
                assert_eq!(action_items, ["item-a", "item-b"]);
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let error = Cli::try_parse_from([
            "gander", "handoff", "--mode", "delegate", "--task", "item-a",
        ])
        .unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn hunks_show_parses_diff_format_and_renders_unified_diff() {
        let cli = Cli::try_parse_from([
            "gander",
            "hunks",
            "show",
            "src/lib.rs:0",
            "--format",
            "diff",
        ])
        .unwrap();
        match cli.command.unwrap() {
            Command::Hunks {
                command: HunksCommand::Show { format, .. },
            } => assert_eq!(format, HunkShowFormat::Diff),
            other => panic!("unexpected command: {other:?}"),
        }

        let diff = session_hunk_diff(&sample_session(), "src/lib.rs:0").unwrap();
        assert!(diff.starts_with("--- a/src/lib.rs\n+++ b/src/lib.rs\n@@"));
        assert!(diff.contains("-"));
        assert!(diff.contains("+"));
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
                session_id: sid.clone(),
                path: Some("src/lib.rs".to_owned()),
                line: Some(3),
                end_line: None,
                anchor,
                observation: None,
                body: "explain this".to_owned(),
                kind: None,
                action: None,
                state: CommentState::Todo,
            },
        )
        .unwrap();
        session.comments = state.comments;
        session.sessions = state.sessions;

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
    fn cli_provenance_uses_loaded_session_without_backend_work() {
        let session = sample_session();
        let spec = session_target_spec(&session.repo, &session.target);
        let mut state = ReviewState::default();
        let id = review::ensure_session(&mut state, &spec, None).id.clone();
        let durable = state
            .sessions
            .iter()
            .find(|session| session.id == id)
            .unwrap();

        let snapshot = provenance_snapshot(&session, durable);
        let observation = crate::provenance::CommentObservation::new(snapshot.clone(), None);

        assert_eq!(snapshot.identity.session_id, id);
        assert_eq!(snapshot.files.len(), session.files.len());
        assert_eq!(
            snapshot.files[0].diff_fingerprint,
            session.files[0].fingerprint
        );
        assert_eq!(observation.snapshot, snapshot);
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
                session_id: sid.clone(),
                path: Some("src/lib.rs".to_owned()),
                line: Some(3),
                end_line: Some(4),
                anchor,
                observation: None,
                body: "explain this range".to_owned(),
                kind: None,
                action: None,
                state: CommentState::Todo,
            },
        )
        .unwrap();
        session.comments = state.comments;
        session.sessions = state.sessions;

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
            path: Some("src/lib.rs".to_owned()),
            line: Some(2),
            end_line: None,
            anchor: None,
            body: "please fix".to_owned(),
            kind: Some(crate::state::CommentKind::Issue),
            action: Some(crate::state::ActionIntent::Fix),
            state: crate::state::CommentState::Todo,
            created_at: chrono::Utc.with_ymd_and_hms(2026, 7, 4, 0, 0, 0).unwrap(),
            ..Default::default()
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
    fn comments_text_is_compact_one_line_per_comment() {
        let text = session_comments_text(&sample_session());
        assert_eq!(
            text,
            "c1       [todo    ] [issue/fix]          src/lib.rs:2                         r0  please fix\n"
        );
    }

    #[test]
    fn action_item_text_uses_compact_unique_selectors() {
        let session = state::ReviewSession {
            action_items: vec![
                crate::state::ActionItem {
                    id: "abcdef12-0000".into(),
                    title: "First durable item".into(),
                    ..Default::default()
                },
                crate::state::ActionItem {
                    id: "abcdef12-ffff".into(),
                    title: "Second durable item".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let listed = review::list_action_items(&session);

        assert_eq!(listed[0].selector, "abcdef12-0");
        assert_eq!(listed[1].selector, "abcdef12-f");
        let text = action_items_text(&listed);
        assert!(text.starts_with("abcdef12-0"));
        assert!(text.contains("abcdef12-f"));
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
    fn comments_json_uses_existing_comment_state() {
        let session = sample_session();

        assert_eq!(session_comments_json(&session)["comments"][0]["id"], "c1");
    }
}
