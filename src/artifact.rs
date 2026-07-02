use std::{fs, io::Write, path::Path};

use chrono::Utc;
use color_eyre::eyre::Result;
use serde::{Deserialize, Serialize};

use crate::{
    anchor::CommentAnchor,
    app::{ReviewFile, ReviewSession},
    diff::{DiffLineKind, Hunk},
    state::{Comment, ReviewState},
};

#[derive(Clone, Copy, Debug)]
pub enum ArtifactFormat {
    Json,
    Markdown,
}

/// Who the artifact is for. The agent profile adds raw diff hunks per file
/// and raw excerpts around each comment so tools can reason about the change
/// without re-running jj.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ArtifactProfile {
    #[default]
    Human,
    Agent,
}

/// Diff context lines included on each side of a comment excerpt.
const EXCERPT_CONTEXT_LINES: usize = 3;

#[derive(Debug, Serialize)]
pub struct ReviewArtifact<'a> {
    pub version: u8,
    pub generated_at: chrono::DateTime<chrono::Utc>,
    pub repo: &'a Path,
    pub base: &'a str,
    pub revision: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<&'static str>,
    pub summary: String,
    pub files: Vec<FileArtifact<'a>>,
    pub comments: Vec<CommentArtifact<'a>>,
}

#[derive(Debug, Serialize)]
pub struct FileArtifact<'a> {
    pub path: &'a str,
    pub old_path: Option<&'a str>,
    pub status: String,
    pub viewed: bool,
    pub generated: bool,
    pub additions: usize,
    pub deletions: usize,
    pub fingerprint: &'a str,
    /// Raw hunks, agent profile only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hunks: Option<Vec<HunkArtifact<'a>>>,
}

#[derive(Debug, Serialize)]
pub struct HunkArtifact<'a> {
    pub header: &'a str,
    pub old_start: usize,
    pub old_len: usize,
    pub new_start: usize,
    pub new_len: usize,
    pub lines: Vec<ExcerptLine<'a>>,
}

#[derive(Debug, Serialize)]
pub struct ExcerptLine<'a> {
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_line: Option<usize>,
    pub text: &'a str,
}

#[derive(Debug, Serialize)]
pub struct CommentArtifact<'a> {
    #[serde(flatten)]
    pub comment: &'a Comment,
    /// Raw diff lines around the comment anchor, agent profile only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<Vec<ExcerptLine<'a>>>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct OwnedReviewArtifact {
    pub version: u8,
    pub base: String,
    pub revision: String,
    pub files: Vec<OwnedFileArtifact>,
    pub comments: Vec<Comment>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct OwnedFileArtifact {
    pub path: String,
    pub viewed: bool,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportSummary {
    pub viewed_files_imported: usize,
    pub comments_imported: usize,
    pub duplicate_comments_skipped: usize,
}

impl Default for OwnedReviewArtifact {
    fn default() -> Self {
        Self {
            version: 4,
            base: String::new(),
            revision: String::new(),
            files: Vec::new(),
            comments: Vec::new(),
        }
    }
}

impl<'a> From<&'a ReviewSession> for ReviewArtifact<'a> {
    fn from(session: &'a ReviewSession) -> Self {
        Self::build(session, ArtifactProfile::Human)
    }
}

impl<'a> ReviewArtifact<'a> {
    pub fn build(session: &'a ReviewSession, profile: ArtifactProfile) -> Self {
        let agent = profile == ArtifactProfile::Agent;
        Self {
            version: 4,
            generated_at: Utc::now(),
            repo: &session.repo,
            base: &session.target.base,
            revision: &session.target.rev,
            profile: agent.then_some("agent"),
            summary: session.summary_line(),
            files: session
                .files
                .iter()
                .map(|file| FileArtifact {
                    path: &file.path,
                    old_path: file.old_path.as_deref(),
                    status: file.status.to_string(),
                    viewed: file.viewed,
                    generated: file.generated,
                    additions: file.additions,
                    deletions: file.deletions,
                    fingerprint: &file.fingerprint,
                    hunks: agent.then(|| file.diff.hunks.iter().map(hunk_artifact).collect()),
                })
                .collect(),
            comments: session
                .comments
                .iter()
                .map(|comment| CommentArtifact {
                    comment,
                    excerpt: agent.then(|| comment_excerpt(session, comment)).flatten(),
                })
                .collect(),
        }
    }
}

fn hunk_artifact(hunk: &Hunk) -> HunkArtifact<'_> {
    HunkArtifact {
        header: &hunk.header,
        old_start: hunk.old_start,
        old_len: hunk.old_len,
        new_start: hunk.new_start,
        new_len: hunk.new_len,
        lines: hunk.lines.iter().map(excerpt_line).collect(),
    }
}

fn excerpt_line(line: &crate::diff::DiffLine) -> ExcerptLine<'_> {
    ExcerptLine {
        kind: match line.kind {
            DiffLineKind::Context => "context",
            DiffLineKind::Added => "added",
            DiffLineKind::Removed => "removed",
            DiffLineKind::Meta => "meta",
        },
        old_line: line.old_lineno,
        new_line: line.new_lineno,
        text: &line.text,
    }
}

/// Raw diff lines around a comment anchor: the anchored line(s) plus
/// [`EXCERPT_CONTEXT_LINES`] context lines on each side within the hunk.
fn comment_excerpt<'a>(
    session: &'a ReviewSession,
    comment: &Comment,
) -> Option<Vec<ExcerptLine<'a>>> {
    let anchor = comment.anchor.as_ref()?;
    let file = session
        .files
        .iter()
        .find(|file| file.path == anchor.path())?;
    let (hunk_index, first_line, last_line) = match anchor {
        CommentAnchor::Line {
            hunk_index,
            line_index,
            ..
        } => (*hunk_index, *line_index, *line_index),
        CommentAnchor::Range { lines, .. } => {
            let first = lines.first()?;
            let in_first_hunk = lines
                .iter()
                .filter(|line| line.hunk_index == first.hunk_index);
            let last = in_first_hunk
                .map(|line| line.line_index)
                .max()
                .unwrap_or(first.line_index);
            (first.hunk_index, first.line_index, last)
        }
        CommentAnchor::File { .. } => return excerpt_for_file(file),
    };
    let hunk = file.diff.hunks.get(hunk_index)?;
    let start = first_line.saturating_sub(EXCERPT_CONTEXT_LINES);
    let end = (last_line + EXCERPT_CONTEXT_LINES + 1).min(hunk.lines.len());
    Some(hunk.lines[start..end].iter().map(excerpt_line).collect())
}

/// File-level comments excerpt the first hunk, which is usually enough for a
/// tool to orient itself.
fn excerpt_for_file(file: &ReviewFile) -> Option<Vec<ExcerptLine<'_>>> {
    let hunk = file.diff.hunks.first()?;
    let end = (EXCERPT_CONTEXT_LINES * 2 + 1).min(hunk.lines.len());
    Some(hunk.lines[..end].iter().map(excerpt_line).collect())
}

pub fn write_artifact(
    session: &ReviewSession,
    format: ArtifactFormat,
    profile: ArtifactProfile,
    path: &Path,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(
        path,
        render_artifact_with_profile(session, format, profile)?,
    )?;
    Ok(())
}

pub fn import_json_artifact_into_state(
    state: &mut ReviewState,
    artifact: &OwnedReviewArtifact,
) -> ImportSummary {
    let mut summary = ImportSummary::default();

    for file in &artifact.files {
        let Some(existing) = state.files.get_mut(&file.path) else {
            continue;
        };
        if existing.fingerprint == file.fingerprint {
            let was_viewed = existing.viewed;
            existing.viewed = file.viewed;
            if file.viewed && !was_viewed {
                summary.viewed_files_imported += 1;
            }
        }
    }

    for comment in &artifact.comments {
        if state
            .comments
            .iter()
            .any(|existing| existing.id == comment.id)
        {
            summary.duplicate_comments_skipped += 1;
        } else {
            state.comments.push(comment.clone());
            summary.comments_imported += 1;
        }
    }

    summary
}

#[cfg(test)]
pub fn render_artifact(session: &ReviewSession, format: ArtifactFormat) -> Result<String> {
    render_artifact_with_profile(session, format, ArtifactProfile::Human)
}

pub fn render_artifact_with_profile(
    session: &ReviewSession,
    format: ArtifactFormat,
    profile: ArtifactProfile,
) -> Result<String> {
    let artifact = ReviewArtifact::build(session, profile);
    match format {
        ArtifactFormat::Json => Ok(serde_json::to_string_pretty(&artifact)?),
        ArtifactFormat::Markdown => Ok(to_markdown(&artifact)),
    }
}

pub fn write_artifact_to(
    session: &ReviewSession,
    format: ArtifactFormat,
    profile: ArtifactProfile,
    mut writer: impl Write,
) -> Result<()> {
    let body = render_artifact_with_profile(session, format, profile)?;
    writer.write_all(body.as_bytes())?;
    if !body.ends_with('\n') {
        writer.write_all(b"\n")?;
    }
    Ok(())
}

fn to_markdown(artifact: &ReviewArtifact<'_>) -> String {
    let mut out = String::new();
    out.push_str("# jj change review\n\n");
    out.push_str(&format!("- Revision: `{}`\n", artifact.revision));
    out.push_str(&format!("- Base: `{}`\n", artifact.base));
    out.push_str(&format!("- Repository: `{}`\n", artifact.repo.display()));
    out.push_str(&format!("- Generated: `{}`\n", artifact.generated_at));
    out.push_str(&format!("- Summary: {}\n\n", artifact.summary));

    out.push_str("## Files\n\n");
    for file in &artifact.files {
        out.push_str(&format!(
            "- [{}] `{}` — {}{} (+{}/-{})\n",
            if file.viewed { "x" } else { " " },
            file.path,
            file.status,
            if file.generated {
                " [generated/noisy]"
            } else {
                ""
            },
            file.additions,
            file.deletions
        ));
    }

    out.push_str("\n## Comments\n\n");
    if artifact.comments.is_empty() {
        out.push_str("No comments recorded.\n");
    } else {
        for comment in &artifact.comments {
            write_comment_heading(&mut out, comment.comment);
            out.push_str(&format!("Status: {}\n\n", comment.comment.state.label()));
            out.push_str(comment.comment.body.trim());
            out.push_str("\n\n");
        }
    }

    out
}

fn write_comment_heading(out: &mut String, comment: &crate::state::Comment) {
    match comment.anchor.as_ref() {
        Some(CommentAnchor::Line {
            path,
            side,
            line,
            hunk_header,
            line_text,
            line_kind,
            diff_fingerprint,
            ..
        }) => {
            out.push_str(&format!("### `{path}`:{}:{line}\n\n", side.label()));
            out.push_str(&format!("Anchor: `{hunk_header}`  \n"));
            out.push_str(&format!("Diff fingerprint: `{diff_fingerprint}`\n\n"));
            out.push_str("```diff\n");
            out.push_str(match line_kind.as_str() {
                "added" => "+",
                "removed" => "-",
                _ => " ",
            });
            out.push_str(line_text);
            out.push_str("\n```\n\n");
        }
        Some(CommentAnchor::Range {
            path,
            start_line,
            end_line,
            lines,
            diff_fingerprint,
            range_fingerprint,
            ..
        }) => {
            out.push_str(&format!("### `{path}`:range:{start_line}-{end_line}\n\n"));
            out.push_str(&format!("Diff fingerprint: `{diff_fingerprint}`\n"));
            out.push_str(&format!("Range fingerprint: `{range_fingerprint}`\n\n"));
            out.push_str("```diff\n");
            for line in lines {
                out.push_str(match line.line_kind.as_str() {
                    "added" => "+",
                    "removed" => "-",
                    _ => " ",
                });
                out.push_str(&format!("{:>4} ", line.line));
                out.push_str(&line.line_text);
                out.push('\n');
            }
            out.push_str("```\n\n");
        }
        Some(CommentAnchor::File { path, .. }) => out.push_str(&format!("### `{path}`\n\n")),
        None => match comment.line {
            Some(line) => out.push_str(&format!("### `{}`:{}\n\n", comment.path, line)),
            None => out.push_str(&format!("### `{}`\n\n", comment.path)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::ReviewSession,
        diff::DiffSet,
        jj::ReviewTarget,
        state::{FileState, ReviewState},
    };

    #[test]
    fn markdown_contains_files_and_comments() {
        let diff = DiffSet::parse(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.add_comment("Looks good".into());
        let artifact = ReviewArtifact::from(&session);
        let markdown = to_markdown(&artifact);
        assert!(markdown.contains("`a.txt`"));
        assert!(markdown.contains("Looks good"));
    }

    #[test]
    fn render_artifact_outputs_json_comments() {
        let diff = DiffSet::parse(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.add_comment("Looks good".into());

        let json = render_artifact(&session, ArtifactFormat::Json).unwrap();

        assert!(json.contains("\"comments\""));
        assert!(json.contains("Looks good"));
    }

    #[test]
    fn write_artifact_to_appends_newline() {
        let diff = DiffSet::parse(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        let session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        let mut out = Vec::new();

        write_artifact_to(
            &session,
            ArtifactFormat::Json,
            ArtifactProfile::Human,
            &mut out,
        )
        .unwrap();

        assert!(out.ends_with(b"\n"));
    }

    #[test]
    fn markdown_includes_comment_state() {
        let diff = DiffSet::parse(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.add_comment("Needs work".into());
        let id = session.comments[0].id.clone();
        session.cycle_comment_state(&id);

        let markdown = render_artifact(&session, ArtifactFormat::Markdown).unwrap();
        let json = render_artifact(&session, ArtifactFormat::Json).unwrap();

        assert!(markdown.contains("Status: todo"));
        assert!(json.contains("\"state\": \"todo\""));
    }

    #[test]
    fn agent_profile_includes_raw_hunks_and_comment_excerpts() {
        let diff = DiffSet::parse(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1,5 +1,5 @@
 first
 second
-old
+new
 fourth
 fifth
"#,
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.toggle_focus();
        session.move_diff_cursor(2);
        session.add_comment("Line note".into());

        let json =
            render_artifact_with_profile(&session, ArtifactFormat::Json, ArtifactProfile::Agent)
                .unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(value["profile"], "agent");
        let hunk = &value["files"][0]["hunks"][0];
        assert_eq!(hunk["header"], "@@ -1,5 +1,5 @@");
        assert_eq!(hunk["lines"].as_array().unwrap().len(), 6);
        assert_eq!(hunk["lines"][2]["kind"], "removed");
        assert_eq!(hunk["lines"][2]["text"], "old");

        let excerpt = value["comments"][0]["excerpt"].as_array().unwrap();
        assert!(!excerpt.is_empty());
        assert!(
            excerpt
                .iter()
                .any(|line| line["kind"] == "added" && line["text"] == "new")
        );
        // Comment fields stay flattened alongside the excerpt.
        assert_eq!(value["comments"][0]["body"], "Line note");
    }

    #[test]
    fn human_profile_omits_hunks_and_excerpts() {
        let diff = DiffSet::parse(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.add_comment("File note".into());

        let json = render_artifact(&session, ArtifactFormat::Json).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert!(value.get("profile").is_none());
        assert!(value["files"][0].get("hunks").is_none());
        assert!(value["comments"][0].get("excerpt").is_none());
    }

    #[test]
    fn artifact_version_is_4() {
        let diff = DiffSet::parse(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        let session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );

        let artifact = ReviewArtifact::from(&session);

        assert_eq!(artifact.version, 4);
        assert_eq!(artifact.profile, None);
    }

    #[test]
    fn markdown_contains_line_anchor() {
        let diff = DiffSet::parse(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.toggle_focus();
        session.add_comment("Line note".into());

        let artifact = ReviewArtifact::from(&session);
        let markdown = to_markdown(&artifact);

        assert!(markdown.contains("`a.txt`:old:1") || markdown.contains("`a.txt`:new:1"));
        assert!(markdown.contains("Anchor: `@@ -1 +1 @@`"));
        assert!(markdown.contains("Line note"));
    }

    #[test]
    fn artifact_includes_generated_metadata() {
        let diff = DiffSet::parse(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.annotate_generated_where(|file| file.path == "a.txt");

        let artifact = ReviewArtifact::from(&session);
        let json = serde_json::to_value(&artifact).unwrap();

        assert!(artifact.files[0].generated);
        assert_eq!(json["files"][0]["generated"], true);
    }

    #[test]
    fn markdown_marks_generated_files() {
        let diff = DiffSet::parse(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.annotate_generated_where(|file| file.path == "a.txt");

        let artifact = ReviewArtifact::from(&session);
        let markdown = to_markdown(&artifact);

        assert!(markdown.contains("`a.txt` — mod [generated/noisy]"));
    }

    #[test]
    fn markdown_contains_range_anchor() {
        let diff = DiffSet::parse(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1,2 +1,3 @@
-old
+new
+extra
 same
"#,
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.toggle_focus();
        session.toggle_diff_range_selection();
        session.move_diff_cursor(2);
        session.add_comment("Range note".into());

        let artifact = ReviewArtifact::from(&session);
        let markdown = to_markdown(&artifact);

        assert!(markdown.contains(":range:"));
        assert!(markdown.contains("Range fingerprint"));
        assert!(markdown.contains("Range note"));
    }

    #[test]
    fn import_json_artifact_merges_matching_viewed_files_and_new_comments() {
        let mut state = ReviewState::default();
        state.files.insert(
            "src/main.rs".to_owned(),
            FileState {
                fingerprint: "abc".to_owned(),
                viewed: false,
            },
        );
        state.files.insert(
            "src/stale.rs".to_owned(),
            FileState {
                fingerprint: "current".to_owned(),
                viewed: false,
            },
        );
        state.comments.push(crate::state::Comment {
            id: "existing".to_owned(),
            path: "src/main.rs".to_owned(),
            line: None,
            end_line: None,
            anchor: None,
            body: "already here".to_owned(),
            state: crate::state::CommentState::default(),
            created_at: chrono::DateTime::parse_from_rfc3339("2026-06-30T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        });
        let artifact: OwnedReviewArtifact = serde_json::from_str(
            r#"{
  "version": 3,
  "base": "trunk()",
  "revision": "@",
  "files": [
    { "path": "src/main.rs", "viewed": true, "fingerprint": "abc" },
    { "path": "src/stale.rs", "viewed": true, "fingerprint": "old" }
  ],
  "comments": [
    {
      "id": "existing",
      "path": "src/main.rs",
      "body": "duplicate",
      "created_at": "2026-06-30T00:00:00Z"
    },
    {
      "id": "new",
      "path": "src/main.rs",
      "body": "new comment",
      "created_at": "2026-06-30T00:00:00Z"
    }
  ]
}"#,
        )
        .unwrap();

        let summary = import_json_artifact_into_state(&mut state, &artifact);

        assert_eq!(summary.viewed_files_imported, 1);
        assert_eq!(summary.comments_imported, 1);
        assert_eq!(summary.duplicate_comments_skipped, 1);
        assert!(state.files["src/main.rs"].viewed);
        assert!(!state.files["src/stale.rs"].viewed);
        assert_eq!(state.comments.len(), 2);
    }
}
