use std::fmt;

use color_eyre::eyre::Result;
use globset::{Glob, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffSet {
    pub raw_header: Vec<String>,
    pub files: Vec<FileDiff>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: String,
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub additions: usize,
    pub deletions: usize,
    pub hunks: Vec<Hunk>,
    pub raw: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    Binary,
    Unknown,
}

impl fmt::Display for FileStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            FileStatus::Added => "added",
            FileStatus::Modified => "mod",
            FileStatus::Deleted => "deleted",
            FileStatus::Renamed => "renamed",
            FileStatus::Copied => "copied",
            FileStatus::Binary => "binary",
            FileStatus::Unknown => "unknown",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hunk {
    pub old_start: usize,
    pub old_len: usize,
    pub new_start: usize,
    pub new_len: usize,
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_lineno: Option<usize>,
    pub new_lineno: Option<usize>,
    pub text: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
    Meta,
}

impl DiffSet {
    pub fn parse(input: &str) -> Result<Self> {
        let mut raw_header = Vec::new();
        let mut files = Vec::new();
        let mut current = Vec::new();
        let mut in_file = false;

        for line in input.lines() {
            if line.starts_with("diff --git ") {
                if !current.is_empty() {
                    files.push(FileDiff::parse(&current.join("\n"))?);
                    current.clear();
                }
                in_file = true;
            }

            if in_file {
                current.push(line.to_owned());
            } else {
                raw_header.push(line.to_owned());
            }
        }

        if !current.is_empty() {
            files.push(FileDiff::parse(&current.join("\n"))?);
        }

        Ok(Self { raw_header, files })
    }

    pub fn apply_ignores(&mut self, patterns: &[String]) -> Result<()> {
        if patterns.is_empty() {
            return Ok(());
        }
        let mut builder = GlobSetBuilder::new();
        for pattern in patterns {
            builder.add(Glob::new(pattern)?);
        }
        let set = builder.build()?;
        self.files.retain(|file| !set.is_match(&file.path));
        Ok(())
    }
}

impl FileDiff {
    fn parse(raw: &str) -> Result<Self> {
        let mut path = String::from("<unknown>");
        let mut old_path = None;
        let mut status = FileStatus::Modified;
        let mut hunks = Vec::new();
        let mut current_hunk: Option<Hunk> = None;
        let mut old_line = 0;
        let mut new_line = 0;
        let mut additions = 0;
        let mut deletions = 0;

        for line in raw.lines() {
            if let Some(rest) = line.strip_prefix("diff --git ") {
                let mut parts = rest.split_whitespace();
                let old = parts
                    .next()
                    .unwrap_or("a/<unknown>")
                    .trim_start_matches("a/");
                let new = parts
                    .next()
                    .unwrap_or("b/<unknown>")
                    .trim_start_matches("b/");
                path = new.to_owned();
                old_path = Some(old.to_owned()).filter(|old| old != new);
            } else if line.starts_with("new file mode") {
                status = FileStatus::Added;
            } else if line.starts_with("deleted file mode") {
                status = FileStatus::Deleted;
            } else if line.starts_with("rename from ") {
                status = FileStatus::Renamed;
            } else if line.starts_with("copy from ") {
                status = FileStatus::Copied;
            } else if line.starts_with("Binary files ") {
                status = FileStatus::Binary;
            } else if let Some(rest) = line.strip_prefix("+++ b/") {
                path = rest.to_owned();
            } else if let Some((old_start, old_len, new_start, new_len, header)) =
                parse_hunk_header(line)
            {
                if let Some(hunk) = current_hunk.take() {
                    hunks.push(hunk);
                }
                old_line = old_start;
                new_line = new_start;
                current_hunk = Some(Hunk {
                    old_start,
                    old_len,
                    new_start,
                    new_len,
                    header,
                    lines: Vec::new(),
                });
            } else if let Some(hunk) = current_hunk.as_mut() {
                let (kind, old_lineno, new_lineno, text) =
                    if line.starts_with('+') && !line.starts_with("+++") {
                        let lineno = new_line;
                        new_line += 1;
                        additions += 1;
                        (
                            DiffLineKind::Added,
                            None,
                            Some(lineno),
                            line[1..].to_owned(),
                        )
                    } else if line.starts_with('-') && !line.starts_with("---") {
                        let lineno = old_line;
                        old_line += 1;
                        deletions += 1;
                        (
                            DiffLineKind::Removed,
                            Some(lineno),
                            None,
                            line[1..].to_owned(),
                        )
                    } else if let Some(text) = line.strip_prefix(' ') {
                        let old_lineno = old_line;
                        let new_lineno = new_line;
                        old_line += 1;
                        new_line += 1;
                        (
                            DiffLineKind::Context,
                            Some(old_lineno),
                            Some(new_lineno),
                            text.to_owned(),
                        )
                    } else {
                        (DiffLineKind::Meta, None, None, line.to_owned())
                    };
                hunk.lines.push(DiffLine {
                    kind,
                    old_lineno,
                    new_lineno,
                    text,
                });
            }
        }

        if let Some(hunk) = current_hunk.take() {
            hunks.push(hunk);
        }

        let mut hasher = Sha256::new();
        hasher.update(raw.as_bytes());
        let fingerprint = format!("{:x}", hasher.finalize());

        Ok(Self {
            path,
            old_path,
            status,
            additions,
            deletions,
            hunks,
            raw: raw.to_owned(),
            fingerprint,
        })
    }
}

fn parse_hunk_header(line: &str) -> Option<(usize, usize, usize, usize, String)> {
    if !line.starts_with("@@ ") {
        return None;
    }
    let header = line.to_owned();
    let mut parts = line.split_whitespace();
    parts.next()?;
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let (old_start, old_len) = parse_range(old)?;
    let (new_start, new_len) = parse_range(new)?;
    Some((old_start, old_len, new_start, new_len, header))
}

fn parse_range(range: &str) -> Option<(usize, usize)> {
    if let Some((start, len)) = range.split_once(',') {
        Some((start.parse().ok()?, len.parse().ok()?))
    } else {
        Some((range.parse().ok()?, 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_git_diff_files_and_hunks() {
        let diff = DiffSet::parse(
            r#"commit metadata
diff --git a/src/main.rs b/src/main.rs
index 111..222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,2 +1,3 @@
 fn main() {
-    println!("old");
+    println!("new");
+    println!("extra");
 }
"#,
        )
        .unwrap();

        assert_eq!(diff.raw_header, vec!["commit metadata"]);
        assert_eq!(diff.files.len(), 1);
        assert_eq!(diff.files[0].path, "src/main.rs");
        assert_eq!(diff.files[0].additions, 2);
        assert_eq!(diff.files[0].deletions, 1);
        assert_eq!(diff.files[0].hunks[0].lines.len(), 5);
    }
}
