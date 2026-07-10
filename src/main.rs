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
mod jj;
mod mcp;
mod paths;
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
    agent::{
        AgentDraft, AgentOverlay, ChangeBrief, ChangeDiffContext, ChunkImportance,
        ChunkValidationContext, DraftState, ReviewChunk, brief_without_spotlight_warnings,
        chunk_line_space, invalid_chunk_parts_message, remove_review_chunks, replace_review_chunks,
        update_review_chunks,
    },
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
        ActionIntent, CommentKind, CommentState, ReviewState, ReviewTarget as StateReviewTarget,
        StepArtifact, StepArtifactKind, StepImportance, StepKind, WalkthroughStep,
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
    long_about = "Gander is a local-first review workspace for jj-visible work. It reads code state and writes durable review state: viewed files, comments, tasks, walkthroughs, and artifacts. The CLI is the normal automation surface; MCP is optional. Run `gander tui` or omit a subcommand to launch the TUI.",
    after_help = "First review loop:\n  gander reviews create --title 'Parser review'\n  gander files list --format text\n  gander comments add --path src/lib.rs --line 42 --body 'Check this invariant'\n  gander tasks add --title 'Add parser regression' --path src/lib.rs --line 42\n  gander walkthrough add-step --title 'Parser flow' --path src/lib.rs --line 42\n  gander handoff --copy"
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
        long_about = "Print or copy an actionable handoff for a coding agent. Prompt mode renders compact prompt-ready Markdown or JSON from open action items, walkthrough stops, and relevant hunks. Delegate mode requires --mode delegate and emits a typed work packet selected by task/comment flags for external harnesses that understand delegation packets.",
        after_help = "Examples:\n  gander handoff --copy\n      Copy prompt-ready Markdown for an implementer agent.\n  gander handoff --format json\n      Emit structured open action items plus walkthrough and reference hunks.\n  gander handoff --mode delegate --task abc123 --to coder --objective 'Fix this task'\n      Emit a typed delegation packet for an external harness."
    )]
    Handoff {
        /// Handoff mode: prompt is the legacy implementation prompt; delegate emits a typed work packet.
        #[arg(long, value_enum, default_value_t = HandoffMode::Prompt)]
        mode: HandoffMode,
        /// Handoff output format. Action items are ordered by action priority (fix, test, follow-up, other), then path and line.
        #[arg(long, value_enum, default_value_t = HandoffFormat::Markdown)]
        format: HandoffFormat,
        /// Include only unresolved comments and open tasks (the default for handoff formats).
        #[arg(long, hide = true)]
        only_open: bool,
        /// Delegate a specific task id. Repeat to include multiple tasks.
        #[arg(long = "task", value_name = "ID")]
        tasks: Vec<String>,
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
    /// Machine-readable task queries for the current review session.
    #[command(
        after_help = "Examples:\n  gander tasks list --format text\n  gander tasks add --title 'Add regression coverage' --path src/lib.rs --line 42 --action test\n  gander tasks complete <id> --summary 'Added and ran the regression test'"
    )]
    Tasks {
        #[command(subcommand)]
        command: TasksCommand,
    },
    /// Walkthrough queries and exports over persisted local review state.
    #[command(
        after_help = "Examples:\n  gander walkthrough add-step --title 'Start here' --path src/lib.rs --line 42\n  gander walkthrough set --file tour.json --dry-run\n  gander walkthrough export"
    )]
    Walkthrough {
        #[command(subcommand)]
        command: WalkthroughCommand,
    },
    /// Deprecated compatibility: author agent-curated review chunks from JSON specs.
    #[command(hide = true)]
    #[command(
        long_about = "Author agent-curated review chunks. Specs are JSON objects like {\"chunks\":[{\"title\":\"Parser flow\",\"importance\":\"spotlight\",\"parts\":[{\"path\":\"src/lib.rs\",\"start_line\":10,\"end_line\":20}]}]}. id is optional for set/update and generated when omitted. Use --file - (or omit --file) to read stdin."
    )]
    Chunks {
        #[command(subcommand)]
        command: ChunksCommand,
    },
    /// Deprecated compatibility: author per-change briefs from JSON specs.
    #[command(hide = true)]
    #[command(
        long_about = "Author per-change briefs. Specs are JSON objects like {\"briefs\":[{\"change_id\":\"abc\",\"summary\":\"Explains the parser groundwork.\"}]}. Use --file - (or omit --file) to read stdin."
    )]
    Briefs {
        #[command(subcommand)]
        command: BriefsCommand,
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

#[derive(Debug, Subcommand)]
enum ChunksCommand {
    /// List current overlay chunks as pretty JSON.
    List,
    /// List valid chunk line ranges for the current diff (or one change diff) as pretty JSON.
    Lines {
        /// jj change id whose change-scoped diff line space should be listed.
        #[arg(long)]
        change: Option<String>,
        /// Restrict output to one file path.
        #[arg(long)]
        path: Option<String>,
    },
    /// Replace all chunks from a JSON spec file (or stdin with --file - / omitted).
    Set {
        #[arg(short, long)]
        file: Option<PathBuf>,
        /// Replace non-chunk authored walkthrough steps too.
        #[arg(long)]
        replace: bool,
    },
    /// Upsert chunks from a JSON spec file (or stdin with --file - / omitted).
    Update {
        #[arg(short, long)]
        file: Option<PathBuf>,
    },
    /// Remove chunks by id. Repeat --id for multiple chunks.
    Remove {
        #[arg(long = "id", required = true)]
        ids: Vec<String>,
    },
    /// Empty the chunk list.
    Clear,
}

#[derive(Debug, Deserialize)]
struct ChunksSpec {
    chunks: Vec<ReviewChunk>,
}

#[derive(Debug, Subcommand)]
enum BriefsCommand {
    /// List current overlay briefs as pretty JSON.
    List,
    /// Replace all briefs from a JSON spec file (or stdin with --file - / omitted).
    Set {
        #[arg(short, long)]
        file: Option<PathBuf>,
        /// Replace existing walkthrough chapters not present in this spec.
        #[arg(long)]
        replace: bool,
    },
    /// Empty the brief list.
    Clear,
}

#[derive(Debug, Deserialize)]
struct BriefsSpec {
    briefs: Vec<ChangeBrief>,
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
    /// Add a durable comment anchored to a changed file.
    Add {
        /// Changed file path to comment on. Alias: --file.
        #[arg(long, alias = "file")]
        path: String,
        /// 1-indexed new-side line number; old-side fallback is used only for removed-only lines.
        #[arg(long)]
        line: Option<usize>,
        /// 1-indexed inclusive new-side (post-image) end line for a range anchor.
        #[arg(long = "end-line", requires = "line")]
        end_line: Option<usize>,
        /// Comment body text.
        #[arg(long)]
        body: String,
        /// Comment classification.
        #[arg(long, value_enum)]
        kind: Option<CommentKindArg>,
        /// Suggested action intent for task/handoff output.
        #[arg(long, value_enum)]
        action: Option<ActionIntentArg>,
        /// Echo format for the added comment.
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
enum TasksCommand {
    /// List tasks as JSON or compact text, including comment-backed todo items.
    List {
        /// Output format for the task list.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
    /// Add a durable review task.
    Add {
        /// Task title.
        #[arg(long)]
        title: String,
        /// Optional task details/body text.
        #[arg(long)]
        body: Option<String>,
        /// Action intent. Accepts none, fix, explain, test, follow-up, and legacy followup.
        #[arg(long, value_enum)]
        action: Option<ActionIntentArg>,
        /// Source comment id or unique id prefix to link.
        #[arg(long = "comment")]
        comment: Option<String>,
        /// Changed file path this task targets. Alias: --file.
        #[arg(long, alias = "file")]
        path: Option<String>,
        /// 1-indexed new-side (post-image) line number this task targets.
        #[arg(long, requires = "path")]
        line: Option<usize>,
        /// Echo format for the added task.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
    /// Mark a task done by id or unique id prefix.
    Complete {
        /// Task id or unique id prefix.
        id: String,
        /// Optional completion summary.
        #[arg(long)]
        summary: Option<String>,
        /// Echo format for the completed task.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
    /// Reopen a done task by id or unique id prefix.
    Reopen {
        /// Task id or unique id prefix.
        id: String,
        /// Echo format for the reopened task.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
    /// Edit a durable review task by id or unique id prefix.
    Edit {
        /// Task id or unique id prefix.
        id: String,
        /// Replacement task title.
        #[arg(long)]
        title: Option<String>,
        /// Replacement task details/body text.
        #[arg(long)]
        body: Option<String>,
        /// Replacement action intent.
        #[arg(long, value_enum)]
        action: Option<ActionIntentArg>,
        /// Replacement changed file path. Alias: --file.
        #[arg(long, alias = "file")]
        path: Option<String>,
        /// Replacement 1-indexed new-side line.
        #[arg(long)]
        line: Option<usize>,
        /// Replacement linked comment id or unique prefix.
        #[arg(long = "comment")]
        comment: Option<String>,
        /// Echo format for the edited task.
        #[arg(long, value_enum, default_value_t = ListFormat::Json)]
        format: ListFormat,
    },
    /// Permanently delete a durable review task by id or unique id prefix.
    Delete {
        /// Task id or unique id prefix.
        id: String,
        /// Echo format for the deleted task.
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
    let repo = cli.repo.unwrap_or(std::env::current_dir()?);
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
            only_open,
            tasks,
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
            let delegate_flags = !tasks.is_empty()
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
                            render_handoff_json(&session, ArtifactBuildOptions { only_open })?
                        }
                        HandoffFormat::Markdown => {
                            render_handoff_markdown(&session, ArtifactBuildOptions { only_open })?
                        }
                    }
                }
                HandoffMode::Delegate => {
                    let durable = find_current_durable_session(&state, &spec)?;
                    let objective = objective.unwrap_or_else(|| {
                        "Address the selected Gander review tasks and comments.".to_owned()
                    });
                    let spec = DelegationSpec {
                        recipient: to,
                        objective,
                        repeated_constraints: constraints,
                        acceptance_criteria: acceptance,
                        requested_verification: verification,
                        task_selectors: tasks,
                        comment_selectors: include_comments,
                        hunk_context_lines: 3,
                    };
                    let packet = build_delegation_packet(&state, durable, &diff, &spec)
                        .map_err(into_user_error)?;
                    if packet.action_items.is_empty() {
                        return Err(user_error(
                            "delegation selected no open task or comment items",
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
                    status = file.status.to_string(),
                    generated = if file.generated { "gen" } else { "" },
                    path = file.path,
                    additions = file.additions,
                    deletions = file.deletions
                );
            }
        }
        Command::Chunks { command } => {
            warn_if_live_session_target_differs(&workspace_paths, &session);
            handle_chunks_command(
                command,
                &session,
                &jj,
                &workspace_paths.overlay_file(),
                &state_path,
            )?
        }
        Command::Briefs { command } => {
            warn_if_live_session_target_differs(&workspace_paths, &session);
            handle_briefs_command(
                command,
                &session,
                &jj,
                &workspace_paths.overlay_file(),
                &state_path,
            )?
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
                match format {
                    ListFormat::Json => print_json(&session_comments_json(&session))?,
                    ListFormat::Text => print!("{}", session_comments_text(&session)),
                }
            }
            CommentsCommand::Add {
                path,
                line,
                end_line,
                body,
                kind,
                action,
                format,
            } => {
                if let (Some(start), Some(end)) = (line, end_line)
                    && end < start
                {
                    return Err(user_error(
                        "comments add --end-line must be greater than or equal to --line",
                    ));
                }
                ensure_diff_file(&session, &path)?;
                let anchor = session
                    .files
                    .iter()
                    .find(|file| file.path == path)
                    .and_then(|file| comment_anchor_for_file_lines(file, line, end_line));
                warn_if_anchorless_line(&path, line, anchor.is_some());
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
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
                        action: action.and_then(action_intent_arg_to_option),
                    },
                );
                state.save(&state_path)?;
                match format {
                    ListFormat::Json => print_json(&comment)?,
                    ListFormat::Text => print!("{}", comment_echo_text(&comment)),
                }
            }
            CommentsCommand::Resolve { id, reply, format } => {
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
                let sid = review::ensure_session(&mut state, &spec, None).id.clone();
                let idx = state.sessions.iter().position(|s| s.id == sid).unwrap();
                let comment = if let Some(reply) = reply {
                    review::reply_and_maybe_resolve_comment(
                        &mut state.sessions[idx],
                        &mut state.comments,
                        &id,
                        reply,
                        true,
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
                let comment = if resolve {
                    review::reply_and_maybe_resolve_comment(
                        &mut state.sessions[idx],
                        &mut state.comments,
                        &id,
                        body,
                        true,
                    )
                } else {
                    review::reply_to_comment(
                        &mut state.sessions[idx],
                        &mut state.comments,
                        &id,
                        body,
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
                    path.to_owned()
                } else {
                    existing.path.clone()
                };
                let anchor_changed =
                    path.is_some() || line.is_some() || start_line.is_some() || end_line.is_some();
                let effective_line = new_line.or(existing.line);
                let effective_end_line = end_line.or(existing.end_line);
                let anchor = anchor_changed.then(|| {
                    session
                        .files
                        .iter()
                        .find(|file| file.path == effective_path)
                        .and_then(|file| {
                            comment_anchor_for_file_lines(file, effective_line, effective_end_line)
                        })
                });
                if anchor_changed {
                    warn_if_anchorless_line(
                        &effective_path,
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
                        path: path.or(Some(effective_path)),
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
        Command::Tasks { command } => match command {
            TasksCommand::List { format } => {
                let spec = session_target_spec(&repo, &session.target);
                warn_session_target_mismatch(&state, &spec);
                let tasks = review::find_session_for_target(&state, &spec)
                    .map(|rs| review::list_tasks(rs, &state.comments))
                    .unwrap_or_default();
                match format {
                    ListFormat::Json => print_json(&serde_json::json!({ "tasks": tasks }))?,
                    ListFormat::Text => print!("{}", tasks_text(&tasks)),
                }
            }
            TasksCommand::Add {
                title,
                body,
                action,
                comment,
                path,
                line,
                format,
            } => {
                if line.is_some() && path.is_none() {
                    return Err(user_error("tasks add --line requires --path"));
                }
                if let Some(path) = path.as_deref() {
                    ensure_diff_file(&session, path)?;
                }
                let comment = comment
                    .as_deref()
                    .map(|id| review::resolve_comment_id(&state.comments, id))
                    .transpose()
                    .map_err(into_user_error)?;
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
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
                    action.map(action_intent_arg).unwrap_or_default(),
                    comment,
                    target,
                );
                state.save(&state_path)?;
                match format {
                    ListFormat::Json => print_json(&task)?,
                    ListFormat::Text => print!("{}", task_echo_text(&task)),
                }
            }
            TasksCommand::Complete {
                id,
                summary,
                format,
            } => {
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
                let rs = review::ensure_session(&mut state, &spec, None);
                let task = review::complete_task(rs, &id, summary).map_err(into_user_error)?;
                state.save(&state_path)?;
                match format {
                    ListFormat::Json => print_json(&task)?,
                    ListFormat::Text => print!("{}", task_echo_text(&task)),
                }
            }
            TasksCommand::Reopen { id, format } => {
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
                let rs = review::ensure_session(&mut state, &spec, None);
                let task = review::reopen_task(rs, &id).map_err(into_user_error)?;
                state.save(&state_path)?;
                match format {
                    ListFormat::Json => print_json(&task)?,
                    ListFormat::Text => print!("{}", task_echo_text(&task)),
                }
            }
            TasksCommand::Edit {
                id,
                title,
                body,
                action,
                path,
                line,
                comment,
                format,
            } => {
                if let Some(path) = path.as_deref() {
                    ensure_diff_file(&session, path)?;
                }
                let comment = comment
                    .as_deref()
                    .map(|id| review::resolve_comment_id(&state.comments, id))
                    .transpose()
                    .map_err(into_user_error)?;
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
                let rs = review::ensure_session(&mut state, &spec, None);
                let existing_task_id = review::resolve_task_id(rs, &id).map_err(into_user_error)?;
                let existing_target = rs
                    .tasks
                    .iter()
                    .find(|task| task.id == existing_task_id)
                    .and_then(|task| task.target.clone());
                if line.is_some()
                    && path.is_none()
                    && existing_target
                        .as_ref()
                        .and_then(|t| t.file.as_ref())
                        .is_none()
                {
                    return Err(user_error(
                        "tasks edit --line requires --path or an existing target path",
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
                let task = review::edit_task(
                    rs,
                    &id,
                    review::TaskEdits {
                        title,
                        body,
                        action: action.map(action_intent_arg),
                        source_comment_id: comment,
                        target: target.map(Some),
                    },
                )
                .map_err(into_user_error)?;
                state.save(&state_path)?;
                match format {
                    ListFormat::Json => print_json(&task)?,
                    ListFormat::Text => print!("{}", task_echo_text(&task)),
                }
            }
            TasksCommand::Delete { id, format } => {
                let spec = session_target_spec(&repo, &session.target);
                note_if_creating_mismatched_session(&state, &spec);
                let rs = review::ensure_session(&mut state, &spec, None);
                let task = review::delete_task(rs, &id).map_err(into_user_error)?;
                state.save(&state_path)?;
                match format {
                    ListFormat::Json => print_json(&task)?,
                    ListFormat::Text => print!("{}", task_echo_text(&task)),
                }
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
            "note: no review session for '{}' — tasks/walkthrough sections will be empty",
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

fn find_current_durable_session<'a>(
    state: &'a ReviewState,
    spec: &SessionTargetSpec,
) -> color_eyre::Result<&'a state::ReviewSession> {
    review::find_session_for_target(state, spec).ok_or_else(|| {
        user_error("no existing review session for this target; create tasks/comments first")
    })
}

fn handle_chunks_command(
    command: ChunksCommand,
    session: &ReviewSession,
    jj: &dyn JjBackend,
    overlay_path: &std::path::Path,
    state_path: &std::path::Path,
) -> color_eyre::Result<()> {
    let mut overlay = AgentOverlay::load_or_default(overlay_path)?;
    match command {
        ChunksCommand::List => print_json(&overlay.chunks)?,
        ChunksCommand::Lines { change, path } => {
            let files = if let Some(change_id) = change {
                let target = ReviewTarget::new(format!("{change_id}-"), change_id.clone());
                let raw = jj.diff(&session.repo, &target).map_err(|error| {
                    user_error(format!(
                        "failed to read change diff for {change_id}: {error}"
                    ))
                })?;
                DiffSet::parse(&raw)
                    .map_err(|error| user_error(format!("failed to parse change diff: {error}")))?
                    .files
            } else {
                session
                    .files
                    .iter()
                    .map(|file| file.diff.clone())
                    .collect::<Vec<_>>()
            };
            print_json(&chunk_line_space(&files, path.as_deref()))?;
        }
        ChunksCommand::Set { file, replace } => {
            eprintln!("warning: chunks commands are deprecated; writing durable walkthrough steps");
            let spec = read_chunks_spec(file.as_ref())?;
            let context = chunk_validation_context_for_cli(session, jj, &spec.chunks)?;
            let chunks = spec.chunks;
            replace_review_chunks(&mut overlay.chunks, chunks.clone(), &context).map_err(
                |invalid| {
                    user_error(format!(
                        "invalid chunk part(s): {}",
                        invalid_chunk_parts_message(&invalid)
                    ))
                },
            )?;
            write_chunk_steps_to_state(state_path, session, chunks, false, replace)?;
            overlay.chunks.clear();
            overlay.save(overlay_path)?;
            println!("Set walkthrough steps from chunks");
        }
        ChunksCommand::Update { file } => {
            eprintln!("warning: chunks commands are deprecated; writing durable walkthrough steps");
            let spec = read_chunks_spec(file.as_ref())?;
            let context = chunk_validation_context_for_cli(session, jj, &spec.chunks)?;
            let chunks = spec.chunks;
            let summary = update_review_chunks(&mut overlay.chunks, chunks.clone(), &context)
                .map_err(|invalid| {
                    user_error(format!(
                        "invalid chunk part(s): {}",
                        invalid_chunk_parts_message(&invalid)
                    ))
                })?;
            write_chunk_steps_to_state(state_path, session, chunks, true, false)?;
            overlay.chunks.clear();
            overlay.save(overlay_path)?;
            println!(
                "Updated {} chunks, added {}; total {}",
                summary.updated, summary.added, summary.chunks
            );
        }
        ChunksCommand::Remove { ids } => {
            eprintln!(
                "warning: chunks commands are deprecated; removing durable walkthrough steps"
            );
            let summary = remove_review_chunks(&mut overlay.chunks, &ids).map_err(|unknown| {
                user_error(format!("unknown chunk id(s): {}", unknown.join(", ")))
            })?;
            remove_walkthrough_steps_from_state(state_path, session, &ids)?;
            overlay.save(overlay_path)?;
            println!("Removed {}; remaining {}", summary.removed, summary.chunks);
        }
        ChunksCommand::Clear => {
            eprintln!(
                "warning: chunks commands are deprecated; clearing durable walkthrough steps"
            );
            overlay.chunks.clear();
            clear_walkthrough_kind_from_state(state_path, session, StepKind::Step)?;
            overlay.save(overlay_path)?;
            println!("Cleared chunks");
        }
    }
    Ok(())
}

fn read_chunks_spec(file: Option<&PathBuf>) -> color_eyre::Result<ChunksSpec> {
    let contents = read_spec_contents(file, "chunk")?;
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) {
        for warning in chunks_spec_unknown_fields(&value) {
            eprintln!("warning: {warning}");
        }
    }
    serde_json::from_str(&contents)
        .map_err(|error| user_error(format!("failed to parse chunk spec JSON: {error}")))
}

fn walkthrough_session_mut<'a>(
    state: &'a mut ReviewState,
    session: &ReviewSession,
) -> &'a mut crate::state::ReviewSession {
    let spec = session_target_spec(&session.repo, &session.target);
    review::ensure_session(state, &spec, None)
}

fn write_chunk_steps_to_state(
    state_path: &std::path::Path,
    session: &ReviewSession,
    chunks: Vec<ReviewChunk>,
    append: bool,
    replace: bool,
) -> color_eyre::Result<()> {
    let mut state = ReviewState::load_or_default(state_path)?;
    let rs = walkthrough_session_mut(&mut state, session);
    let new_steps: Vec<_> = chunks.into_iter().map(chunk_to_walkthrough_step).collect();
    if append {
        for step in new_steps {
            review::add_walkthrough_step(rs, step);
        }
    } else {
        let existing_steps = rs
            .walkthroughs
            .first()
            .map(|walkthrough| walkthrough.steps.len())
            .unwrap_or(0);
        if existing_steps > 0 && !replace {
            return Err(user_error(format!(
                "walkthrough has {existing_steps} steps; use walkthrough set, or pass --replace"
            )));
        }
        review::set_walkthrough_preserve_ids(rs, Some("Walkthrough".to_owned()), new_steps);
    }
    state.save(state_path)?;
    Ok(())
}

fn remove_walkthrough_steps_from_state(
    state_path: &std::path::Path,
    session: &ReviewSession,
    ids: &[String],
) -> color_eyre::Result<()> {
    let mut state = ReviewState::load_or_default(state_path)?;
    let rs = walkthrough_session_mut(&mut state, session);
    for id in ids {
        let _ = review::remove_walkthrough_step(rs, id);
    }
    state.save(state_path)?;
    Ok(())
}

fn clear_walkthrough_kind_from_state(
    state_path: &std::path::Path,
    session: &ReviewSession,
    kind: StepKind,
) -> color_eyre::Result<()> {
    let mut state = ReviewState::load_or_default(state_path)?;
    let rs = walkthrough_session_mut(&mut state, session);
    for walkthrough in &mut rs.walkthroughs {
        walkthrough.steps.retain(|step| step.kind != kind);
    }
    state.save(state_path)?;
    Ok(())
}

fn write_brief_chapters_to_state(
    state_path: &std::path::Path,
    session: &ReviewSession,
    briefs: Vec<ChangeBrief>,
    replace: bool,
) -> color_eyre::Result<()> {
    let mut state = ReviewState::load_or_default(state_path)?;
    let rs = walkthrough_session_mut(&mut state, session);
    if rs.walkthroughs.is_empty() {
        review::set_walkthrough(rs, Some("Walkthrough".to_owned()), Vec::new());
    }
    if replace {
        rs.walkthroughs[0]
            .steps
            .retain(|step| step.kind != StepKind::Chapter);
    }
    for brief in briefs {
        let step = brief_to_walkthrough_step(brief);
        if let Some(change_id) = step.change_id.as_deref()
            && let Some(existing) = rs.walkthroughs[0].steps.iter_mut().find(|existing| {
                existing.kind == StepKind::Chapter
                    && existing.change_id.as_deref() == Some(change_id)
            })
        {
            *existing = step;
            continue;
        }
        review::add_walkthrough_step(rs, step);
    }
    state.save(state_path)?;
    Ok(())
}

const CHUNK_SPEC_KEYS: &[&str] = &[
    "id",
    "title",
    "importance",
    "change_id",
    "rationale",
    "explanation",
    "artifacts",
    "parts",
];
const CHUNK_PART_KEYS: &[&str] = &["path", "start_line", "end_line"];
const ARTIFACT_KEYS: &[&str] = &["title", "kind", "body"];
const BRIEF_KEYS: &[&str] = &["change_id", "summary", "artifacts"];
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

fn chunks_spec_unknown_fields(value: &serde_json::Value) -> Vec<String> {
    let mut warnings = Vec::new();
    push_unknown_field_warnings(&mut warnings, value, "spec root", &["chunks"]);
    for (index, chunk) in value
        .get("chunks")
        .and_then(|chunks| chunks.as_array())
        .into_iter()
        .flatten()
        .enumerate()
    {
        let at = format!("chunks[{index}]");
        push_unknown_field_warnings(&mut warnings, chunk, &at, CHUNK_SPEC_KEYS);
        artifact_unknown_field_warnings(&mut warnings, chunk, &at);
        for (part_index, part) in chunk
            .get("parts")
            .and_then(|parts| parts.as_array())
            .into_iter()
            .flatten()
            .enumerate()
        {
            push_unknown_field_warnings(
                &mut warnings,
                part,
                &format!("{at}.parts[{part_index}]"),
                CHUNK_PART_KEYS,
            );
        }
    }
    warnings
}

fn briefs_spec_unknown_fields(value: &serde_json::Value) -> Vec<String> {
    let mut warnings = Vec::new();
    push_unknown_field_warnings(&mut warnings, value, "spec root", &["briefs"]);
    for (index, brief) in value
        .get("briefs")
        .and_then(|briefs| briefs.as_array())
        .into_iter()
        .flatten()
        .enumerate()
    {
        let at = format!("briefs[{index}]");
        push_unknown_field_warnings(&mut warnings, brief, &at, BRIEF_KEYS);
        artifact_unknown_field_warnings(&mut warnings, brief, &at);
    }
    warnings
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

fn handle_briefs_command(
    command: BriefsCommand,
    session: &ReviewSession,
    jj: &dyn JjBackend,
    overlay_path: &std::path::Path,
    state_path: &std::path::Path,
) -> color_eyre::Result<()> {
    let mut overlay = AgentOverlay::load_or_default(overlay_path)?;
    match command {
        BriefsCommand::List => print_json(&overlay.briefs)?,
        BriefsCommand::Set { file, replace } => {
            eprintln!(
                "warning: briefs commands are deprecated; writing durable walkthrough chapters"
            );
            let spec: BriefsSpec = read_json_spec(file.as_ref(), "brief")?;
            validate_briefs_for_cli(session, jj, &spec.briefs)?;
            write_brief_chapters_to_state(state_path, session, spec.briefs.clone(), replace)?;
            overlay.briefs.clear();
            let warnings = brief_without_spotlight_warnings(&overlay.briefs, &overlay.chunks);
            overlay.save(overlay_path)?;
            for warning in &warnings {
                eprintln!("warning: {warning}");
            }
            println!("Set {} briefs", overlay.briefs.len());
        }
        BriefsCommand::Clear => {
            eprintln!(
                "warning: briefs commands are deprecated; clearing durable walkthrough chapters"
            );
            overlay.briefs.clear();
            clear_walkthrough_kind_from_state(state_path, session, StepKind::Chapter)?;
            overlay.save(overlay_path)?;
            println!("Cleared briefs");
        }
    }
    Ok(())
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
            "brief" => briefs_spec_unknown_fields(&value),
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

fn validate_briefs_for_cli(
    session: &ReviewSession,
    jj: &dyn JjBackend,
    briefs: &[ChangeBrief],
) -> color_eyre::Result<()> {
    let changes = jj.stack_changes(&session.repo, &session.target)?;
    let invalid = briefs
        .iter()
        .filter_map(|brief| {
            if brief.change_id.trim().is_empty() {
                Some("change_id must not be empty".to_owned())
            } else if brief.summary.trim().is_empty() {
                Some(format!("{}: summary must not be empty", brief.change_id))
            } else if !changes
                .iter()
                .any(|change| change.change_id == brief.change_id.trim())
            {
                Some(format!("{}: unknown change id", brief.change_id))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    if !invalid.is_empty() {
        return Err(user_error(format!(
            "invalid brief(s): {}",
            invalid.join("; ")
        )));
    }
    Ok(())
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

fn chunk_to_walkthrough_step(chunk: ReviewChunk) -> WalkthroughStep {
    let mut targets = chunk.parts.into_iter().map(|part| StateReviewTarget {
        file: Some(part.path),
        line: part.start_line,
        end_line: part.end_line,
        ..Default::default()
    });
    WalkthroughStep {
        id: chunk.id,
        title: Some(chunk.title),
        importance: match chunk.importance {
            ChunkImportance::Spotlight => StepImportance::Spotlight,
            ChunkImportance::Glance => StepImportance::Glance,
        },
        kind: StepKind::Step,
        change_id: chunk.change_id,
        why: chunk.rationale,
        body: chunk.explanation,
        artifacts: chunk
            .artifacts
            .into_iter()
            .map(agent_artifact_to_step)
            .collect(),
        target: targets.next().unwrap_or_default(),
        extra_targets: targets.collect(),
        ..Default::default()
    }
}

fn brief_to_walkthrough_step(brief: ChangeBrief) -> WalkthroughStep {
    WalkthroughStep {
        id: format!("chapter-{}", brief.change_id),
        title: Some(brief.change_id.clone()),
        kind: StepKind::Chapter,
        change_id: Some(brief.change_id),
        body: Some(brief.summary),
        artifacts: brief
            .artifacts
            .into_iter()
            .map(agent_artifact_to_step)
            .collect(),
        ..Default::default()
    }
}

fn agent_artifact_to_step(artifact: crate::agent::Artifact) -> StepArtifact {
    StepArtifact {
        title: artifact.title,
        kind: match artifact.kind {
            crate::agent::ArtifactKind::Example => StepArtifactKind::Example,
            crate::agent::ArtifactKind::Output => StepArtifactKind::Output,
            crate::agent::ArtifactKind::Diagram => StepArtifactKind::Diagram,
            crate::agent::ArtifactKind::Note => StepArtifactKind::Note,
        },
        body: artifact.body,
    }
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

fn chunk_validation_context_for_cli(
    session: &ReviewSession,
    jj: &dyn JjBackend,
    chunks: &[ReviewChunk],
) -> color_eyre::Result<ChunkValidationContext<'static>> {
    let session_files = session
        .files
        .iter()
        .map(|file| file.diff.clone())
        .collect::<Vec<_>>();
    let mut parsed_changes = Vec::new();
    for change_id in chunks.iter().filter_map(|chunk| chunk.change_id.as_ref()) {
        if parsed_changes
            .iter()
            .any(|(existing, _): &(String, DiffSet)| existing == change_id)
        {
            continue;
        }
        let target = ReviewTarget::new(format!("{change_id}-"), change_id.clone());
        if let Ok(raw) = jj.diff(&session.repo, &target)
            && let Ok(diff) = DiffSet::parse(&raw)
        {
            parsed_changes.push((change_id.clone(), diff));
        }
    }
    let leaked_session = Box::leak(session_files.into_boxed_slice());
    let leaked_changes: &'static [(String, DiffSet)] = Box::leak(parsed_changes.into_boxed_slice());
    Ok(ChunkValidationContext {
        session_files: leaked_session,
        change_diffs: leaked_changes
            .iter()
            .map(|(change_id, diff)| ChangeDiffContext {
                change_id: change_id.clone(),
                files: &diff.files,
            })
            .collect(),
    })
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

fn action_intent_arg(value: ActionIntentArg) -> ActionIntent {
    match value {
        ActionIntentArg::None => ActionIntent::None,
        ActionIntentArg::Fix => ActionIntent::Fix,
        ActionIntentArg::Explain => ActionIntent::Explain,
        ActionIntentArg::Test => ActionIntent::Test,
        ActionIntentArg::FollowUp => ActionIntent::FollowUp,
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
fn task_status_label(status: crate::state::ReviewTaskStatus) -> &'static str {
    match status {
        crate::state::ReviewTaskStatus::Open => "open",
        crate::state::ReviewTaskStatus::Done => "done",
        crate::state::ReviewTaskStatus::Dismissed => "dismissed",
    }
}

fn loc(path: &str, line: Option<usize>, end_line: Option<usize>) -> String {
    match (line, end_line) {
        (Some(a), Some(b)) if b != a => format!("{path}:{a}-{b}"),
        (Some(a), _) => format!("{path}:{a}"),
        _ => path.to_owned(),
    }
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
            ellipsize(&loc(&c.path, c.line, c.end_line), 36),
            c.replies.len(),
            ellipsize(&c.body, 80)
        ));
    }
    out
}

fn tasks_text(tasks: &[review::ListedTask]) -> String {
    let mut out = String::new();
    for t in tasks {
        let location = t
            .target
            .as_ref()
            .and_then(|x| x.file.as_ref().map(|p| loc(p, x.line, x.end_line)))
            .unwrap_or_default();
        let linked = t
            .source_comment_id
            .as_ref()
            .map(|id| id[..id.len().min(8)].to_owned())
            .unwrap_or_default();
        out.push_str(&format!(
            "{:<8} {:<9} [{:<9}] {:<54} {:<32} {}\n",
            &t.id[..t.id.len().min(8)],
            task_status_label(t.status),
            action_label_opt(t.action),
            ellipsize(&t.title, 54),
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
            "{:<8} {:<9} {:<36} {:<28} {} task(s), {} walkthrough(s)\n",
            &s.id[..s.id.len().min(8)],
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
            s.task_count,
            s.walkthrough_count
        ));
    }
    out
}

fn comment_echo_text(c: &crate::state::Comment) -> String {
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
        loc(&c.path, c.line, c.end_line),
        anchored
    )
}

fn task_echo_text(t: &crate::state::ReviewTask) -> String {
    let location = t
        .target
        .as_ref()
        .and_then(|x| x.file.as_ref().map(|p| loc(p, x.line, x.end_line)))
        .unwrap_or_else(|| "(no anchor)".to_owned());
    let body_line = t
        .body
        .as_deref()
        .and_then(|body| body.lines().next())
        .filter(|line| !line.trim().is_empty())
        .map(|line| format!("body: {}\n", ellipsize(line, 100)))
        .unwrap_or_default();
    format!(
        "id: {}\nstatus/action: {}/{}\ntitle: {} ({})\n{}",
        t.id,
        task_status_label(t.status),
        action_label_opt(Some(t.action)),
        t.title,
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
    serde_json::json!({ "comments": session.comments })
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
    fn chunk_spec_allowed_keys_match_struct_serialization() {
        let chunk = ReviewChunk {
            id: "c1".into(),
            title: "t".into(),
            importance: crate::agent::ChunkImportance::Glance,
            change_id: Some("abc".into()),
            rationale: Some("r".into()),
            explanation: Some("e".into()),
            artifacts: vec![crate::agent::Artifact {
                title: "a".into(),
                kind: Default::default(),
                body: "b".into(),
            }],
            parts: vec![crate::agent::ChunkPart {
                path: "src/lib.rs".into(),
                start_line: Some(1),
                end_line: Some(2),
            }],
        };
        let value = serde_json::to_value(&chunk).unwrap();
        for key in value.as_object().unwrap().keys() {
            assert!(
                CHUNK_SPEC_KEYS.contains(&key.as_str()),
                "ReviewChunk gained field '{key}' — update CHUNK_SPEC_KEYS"
            );
        }
        let part = serde_json::to_value(&chunk.parts[0]).unwrap();
        for key in part.as_object().unwrap().keys() {
            assert!(
                CHUNK_PART_KEYS.contains(&key.as_str()),
                "ChunkPart gained field '{key}' — update CHUNK_PART_KEYS"
            );
        }
        let artifact = serde_json::to_value(&chunk.artifacts[0]).unwrap();
        for key in artifact.as_object().unwrap().keys() {
            assert!(
                ARTIFACT_KEYS.contains(&key.as_str()),
                "Artifact gained field '{key}' — update ARTIFACT_KEYS"
            );
        }
        let brief = serde_json::to_value(crate::agent::ChangeBrief {
            change_id: "abc".into(),
            summary: "s".into(),
            artifacts: vec![chunk.artifacts[0].clone()],
        })
        .unwrap();
        for key in brief.as_object().unwrap().keys() {
            assert!(
                BRIEF_KEYS.contains(&key.as_str()),
                "ChangeBrief gained field '{key}' — update BRIEF_KEYS"
            );
        }
    }

    #[test]
    fn chunk_spec_typo_field_is_warned_not_silent() {
        let value: serde_json::Value = serde_json::from_str(
            r#"{"chunks":[{"id":"c1","title":"t","role":"glance","parts":[{"path":"src/lib.rs","start_line":1,"end_line":2}]}]}"#,
        )
        .unwrap();
        let warnings = chunks_spec_unknown_fields(&value);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("unknown field 'role' at chunks[0]"));
        assert!(warnings[0].contains("importance"));
    }

    #[test]
    fn brief_and_draft_spec_unknown_fields_are_warned() {
        let brief_value: serde_json::Value =
            serde_json::from_str(r#"{"briefs":[{"change_id":"x","summary":"s","risk":"high"}]}"#)
                .unwrap();
        let warnings = briefs_spec_unknown_fields(&brief_value);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("unknown field 'risk' at briefs[0]"));

        let draft_value: serde_json::Value = serde_json::from_str(
            r#"{"drafts":[{"path":"src/lib.rs","line":3,"body":"b","severity":"major"}]}"#,
        )
        .unwrap();
        let warnings = drafts_spec_unknown_fields(&draft_value);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("unknown field 'severity' at drafts[0]"));

        let valid: serde_json::Value = serde_json::from_str(
            r#"{"chunks":[{"title":"t","importance":"spotlight","parts":[{"path":"a","start_line":1,"end_line":2}]}]}"#,
        )
        .unwrap();
        assert!(chunks_spec_unknown_fields(&valid).is_empty());
    }

    struct BriefsTestJj;

    impl JjBackend for BriefsTestJj {
        fn snapshot_working_copy(&self, _: &std::path::Path) -> color_eyre::Result<()> {
            Ok(())
        }

        fn diff(&self, _: &std::path::Path, _: &ReviewTarget) -> color_eyre::Result<String> {
            Ok(String::new())
        }

        fn change_summaries(
            &self,
            _: &std::path::Path,
        ) -> color_eyre::Result<Vec<crate::jj::JjChangeSummary>> {
            Ok(Vec::new())
        }

        fn stack_changes(
            &self,
            _: &std::path::Path,
            _: &crate::jj::ReviewTarget,
        ) -> color_eyre::Result<Vec<crate::jj::JjChangeSummary>> {
            Ok(vec![crate::jj::JjChangeSummary {
                change_id: "abc".to_owned(),
                bookmarks: String::new(),
                description: "test".to_owned(),
            }])
        }

        fn change_fingerprint(
            &self,
            _: &std::path::Path,
            _: &ReviewTarget,
        ) -> color_eyre::Result<String> {
            Ok(String::new())
        }

        fn operations(
            &self,
            _: &std::path::Path,
        ) -> color_eyre::Result<Vec<crate::jj::JjOperationSummary>> {
            Ok(Vec::new())
        }

        fn diff_at_operation(
            &self,
            _: &std::path::Path,
            _: &ReviewTarget,
            _: &str,
        ) -> color_eyre::Result<String> {
            Ok(String::new())
        }

        fn file_contents(
            &self,
            _: &std::path::Path,
            _: &str,
            _: &str,
        ) -> color_eyre::Result<String> {
            Ok(String::new())
        }

        fn run_command(&self, _: &std::path::Path, _: &[String]) -> color_eyre::Result<String> {
            Ok(String::new())
        }
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
    fn briefs_cli_validates_change_ids() {
        let repo = tempfile::tempdir().unwrap();
        let session = ReviewSession::new(
            repo.path().to_path_buf(),
            ReviewTarget::trunk_to_current(),
            DiffSet {
                raw_header: Vec::new(),
                files: Vec::new(),
            },
            ReviewState::default(),
        );
        let valid = vec![ChangeBrief {
            change_id: "abc".to_owned(),
            summary: "Summary".to_owned(),
            artifacts: Vec::new(),
        }];
        assert!(validate_briefs_for_cli(&session, &BriefsTestJj, &valid).is_ok());

        let invalid = vec![ChangeBrief {
            change_id: "missing".to_owned(),
            summary: "Summary".to_owned(),
            artifacts: Vec::new(),
        }];
        let error = validate_briefs_for_cli(&session, &BriefsTestJj, &invalid).unwrap_err();
        assert!(error.to_string().contains("missing: unknown change id"));
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
                &["tasks", "add"][..],
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
            "--state ",
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
    fn none_action_maps_to_a_real_task_action_but_clears_optional_comment_action() {
        assert_eq!(action_intent_arg(ActionIntentArg::None), ActionIntent::None);
        assert_eq!(action_intent_arg_to_option(ActionIntentArg::None), None);
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
            ["gander", "tasks", "add", "--title", "t", "--line", "1"].as_slice(),
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
                "gander", "tasks", "add", "--title", "t", "--file", "p", "--line", "1",
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
                "gander", "tasks", "add", "--title", "fix", "--action", "followup",
            ],
            &[
                "gander",
                "tasks",
                "add",
                "--title",
                "fix",
                "--action",
                "follow-up",
            ],
            &["gander", "tasks", "complete", "abc", "--summary", "done"],
            &["gander", "tasks", "reopen", "abc"],
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
            &["gander", "chunks", "set", "--file", "-"],
            &["gander", "chunks", "update", "--file", "-"],
            &["gander", "chunks", "remove", "--id", "abc"],
            &["gander", "chunks", "clear"],
            &["gander", "briefs", "set", "--file", "-"],
            &["gander", "briefs", "clear"],
            &["gander", "drafts", "add", "--file", "-"],
            &["gander", "drafts", "remove", "--id", "abc"],
        ];

        for command in commands {
            Cli::try_parse_from(*command)
                .unwrap_or_else(|error| panic!("failed to parse {command:?}: {error}"));
        }
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
                ..
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
