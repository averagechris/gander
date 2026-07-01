use std::{fs, path::Path};

use chrono::Utc;
use color_eyre::eyre::Result;
use serde::Serialize;

use crate::app::ReviewSession;

#[derive(Clone, Copy, Debug)]
pub enum ArtifactFormat {
    Json,
    Markdown,
}

#[derive(Debug, Serialize)]
pub struct ReviewArtifact<'a> {
    pub version: u8,
    pub generated_at: chrono::DateTime<chrono::Utc>,
    pub repo: &'a Path,
    pub revision: &'a str,
    pub summary: String,
    pub files: Vec<FileArtifact<'a>>,
    pub comments: &'a [crate::state::Comment],
}

#[derive(Debug, Serialize)]
pub struct FileArtifact<'a> {
    pub path: &'a str,
    pub old_path: Option<&'a str>,
    pub status: String,
    pub viewed: bool,
    pub additions: usize,
    pub deletions: usize,
    pub fingerprint: &'a str,
}

impl<'a> From<&'a ReviewSession> for ReviewArtifact<'a> {
    fn from(session: &'a ReviewSession) -> Self {
        Self {
            version: 1,
            generated_at: Utc::now(),
            repo: &session.repo,
            revision: &session.revision,
            summary: session.summary_line(),
            files: session
                .files
                .iter()
                .map(|file| FileArtifact {
                    path: &file.path,
                    old_path: file.old_path.as_deref(),
                    status: file.status.to_string(),
                    viewed: file.viewed,
                    additions: file.additions,
                    deletions: file.deletions,
                    fingerprint: &file.fingerprint,
                })
                .collect(),
            comments: &session.comments,
        }
    }
}

pub fn write_artifact(session: &ReviewSession, format: ArtifactFormat, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let artifact = ReviewArtifact::from(session);
    let body = match format {
        ArtifactFormat::Json => serde_json::to_string_pretty(&artifact)?,
        ArtifactFormat::Markdown => to_markdown(&artifact),
    };
    fs::write(path, body)?;
    Ok(())
}

fn to_markdown(artifact: &ReviewArtifact<'_>) -> String {
    let mut out = String::new();
    out.push_str("# jj change review\n\n");
    out.push_str(&format!("- Revision: `{}`\n", artifact.revision));
    out.push_str(&format!("- Repository: `{}`\n", artifact.repo.display()));
    out.push_str(&format!("- Generated: `{}`\n", artifact.generated_at));
    out.push_str(&format!("- Summary: {}\n\n", artifact.summary));

    out.push_str("## Files\n\n");
    for file in &artifact.files {
        out.push_str(&format!(
            "- [{}] `{}` — {} (+{}/-{})\n",
            if file.viewed { "x" } else { " " },
            file.path,
            file.status,
            file.additions,
            file.deletions
        ));
    }

    out.push_str("\n## Comments\n\n");
    if artifact.comments.is_empty() {
        out.push_str("No comments recorded.\n");
    } else {
        for comment in artifact.comments {
            match comment.line {
                Some(line) => out.push_str(&format!("### `{}`:{}\n\n", comment.path, line)),
                None => out.push_str(&format!("### `{}`\n\n", comment.path)),
            }
            out.push_str(comment.body.trim());
            out.push_str("\n\n");
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::ReviewSession, diff::DiffSet, state::ReviewState};

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
        let mut session = ReviewSession::new(".".into(), "@".into(), diff, ReviewState::default());
        session.add_comment("Looks good".into());
        let artifact = ReviewArtifact::from(&session);
        let markdown = to_markdown(&artifact);
        assert!(markdown.contains("`a.txt`"));
        assert!(markdown.contains("Looks good"));
    }
}
