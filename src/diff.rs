use std::{fmt, io::BufRead};

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

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    #[default]
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

impl FileDiff {
    /// Whether this patch carries binary content independently of its
    /// structural rename/copy status.
    pub fn is_binary(&self) -> bool {
        self.status == FileStatus::Binary
            || self
                .raw
                .lines()
                .any(|line| line.starts_with("Binary files ") || line == "GIT binary patch")
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
        Self::parse_reader(input.as_bytes())
    }

    /// Streaming parse: consumes the reader line-by-line, building each file
    /// incrementally instead of materializing the whole diff up front.
    pub fn parse_reader(reader: impl BufRead) -> Result<Self> {
        Self::parse_lines(reader.lines().map(|line| line.map_err(Into::into)))
    }

    fn parse_lines(lines: impl Iterator<Item = Result<String>>) -> Result<Self> {
        let mut raw_header = Vec::new();
        let mut files = Vec::new();
        let mut current: Option<FileDiffBuilder> = None;

        for line in lines {
            let line = line?;
            if line.starts_with("diff --git ") {
                if let Some(builder) = current.take() {
                    files.push(builder.finish());
                }
                current = Some(FileDiffBuilder::new());
            }

            match current.as_mut() {
                Some(builder) => builder.push_line(&line),
                None => raw_header.push(line),
            }
        }

        if let Some(builder) = current.take() {
            files.push(builder.finish());
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

impl Hunk {
    pub fn content_fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.header.as_bytes());
        hasher.update(b"\n");
        for line in &self.lines {
            hasher.update(match line.kind {
                DiffLineKind::Context => b" " as &[u8],
                DiffLineKind::Added => b"+" as &[u8],
                DiffLineKind::Removed => b"-" as &[u8],
                DiffLineKind::Meta => b"\\" as &[u8],
            });
            hasher.update(line.text.as_bytes());
            hasher.update(b"\n");
        }
        format!("{:x}", hasher.finalize())
    }
}

/// Incremental single-file parser fed one line at a time, so callers can
/// stream arbitrarily large diffs without buffering per-file line vectors.
#[derive(Debug, Default)]
struct FileDiffBuilder {
    path: Option<String>,
    old_path: Option<String>,
    status: FileStatus,
    hunks: Vec<Hunk>,
    current_hunk: Option<Hunk>,
    old_line: usize,
    new_line: usize,
    additions: usize,
    deletions: usize,
    binary: bool,
    raw: String,
}

impl FileDiffBuilder {
    fn new() -> Self {
        Self::default()
    }

    fn push_line(&mut self, line: &str) {
        if !self.raw.is_empty() {
            self.raw.push('\n');
        }
        self.raw.push_str(line);

        // Header metadata (---/+++, rename/copy, mode lines) must only be
        // parsed before the first hunk; inside hunks, lines like
        // "--- text" are diff content, not file markers.
        let in_header = self.hunks.is_empty() && self.current_hunk.is_none();
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if let Some((old, new)) = parse_diff_git_paths(rest) {
                let old = strip_path_prefix(&old, "a/");
                let new = strip_path_prefix(&new, "b/");
                self.path = Some(new);
                self.old_path = Some(old);
            }
        } else if in_header && line.starts_with("new file mode") {
            self.status = FileStatus::Added;
        } else if in_header && line.starts_with("deleted file mode") {
            self.status = FileStatus::Deleted;
        } else if in_header && let Some(rest) = line.strip_prefix("rename from ") {
            self.status = FileStatus::Renamed;
            self.old_path = Some(parse_bare_path(rest));
        } else if in_header && let Some(rest) = line.strip_prefix("rename to ") {
            self.status = FileStatus::Renamed;
            self.path = Some(parse_bare_path(rest));
        } else if in_header && let Some(rest) = line.strip_prefix("copy from ") {
            self.status = FileStatus::Copied;
            self.old_path = Some(parse_bare_path(rest));
        } else if in_header && let Some(rest) = line.strip_prefix("copy to ") {
            self.status = FileStatus::Copied;
            self.path = Some(parse_bare_path(rest));
        } else if in_header && (line.starts_with("Binary files ") || line == "GIT binary patch") {
            self.binary = true;
        } else if in_header && let Some(rest) = line.strip_prefix("--- ") {
            if let Some(parsed) = parse_marker_path(rest, "a/") {
                self.old_path = Some(parsed);
            }
        } else if in_header && let Some(rest) = line.strip_prefix("+++ ") {
            if let Some(parsed) = parse_marker_path(rest, "b/") {
                self.path = Some(parsed);
            }
        } else if let Some((old_start, old_len, new_start, new_len, header)) =
            parse_hunk_header(line)
        {
            if let Some(hunk) = self.current_hunk.take() {
                self.hunks.push(hunk);
            }
            self.old_line = old_start;
            self.new_line = new_start;
            self.current_hunk = Some(Hunk {
                old_start,
                old_len,
                new_start,
                new_len,
                header,
                lines: Vec::new(),
            });
        } else if let Some(hunk) = self.current_hunk.as_mut() {
            let (kind, old_lineno, new_lineno, text) =
                if line.starts_with('+') && !line.starts_with("+++") {
                    let lineno = self.new_line;
                    self.new_line += 1;
                    self.additions += 1;
                    (
                        DiffLineKind::Added,
                        None,
                        Some(lineno),
                        line[1..].to_owned(),
                    )
                } else if line.starts_with('-') && !line.starts_with("---") {
                    let lineno = self.old_line;
                    self.old_line += 1;
                    self.deletions += 1;
                    (
                        DiffLineKind::Removed,
                        Some(lineno),
                        None,
                        line[1..].to_owned(),
                    )
                } else if let Some(text) = line.strip_prefix(' ') {
                    let old_lineno = self.old_line;
                    let new_lineno = self.new_line;
                    self.old_line += 1;
                    self.new_line += 1;
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

    fn finish(mut self) -> FileDiff {
        if let Some(hunk) = self.current_hunk.take() {
            self.hunks.push(hunk);
        }

        let mut hasher = Sha256::new();
        hasher.update(self.raw.as_bytes());
        let fingerprint = format!("{:x}", hasher.finalize());

        let path = self.path.unwrap_or_else(|| "<unknown>".to_owned());
        let status =
            if self.binary && !matches!(self.status, FileStatus::Renamed | FileStatus::Copied) {
                FileStatus::Binary
            } else {
                self.status
            };
        FileDiff {
            old_path: self.old_path.filter(|old| *old != path),
            path,
            status,
            additions: self.additions,
            deletions: self.deletions,
            hunks: self.hunks,
            raw: self.raw,
            fingerprint,
        }
    }
}

fn strip_path_prefix(path: &str, prefix: &str) -> String {
    path.strip_prefix(prefix).unwrap_or(path).to_owned()
}

/// Parse the path from a `--- ` or `+++ ` file marker, handling quoted paths
/// and returning `None` for `/dev/null`.
fn parse_marker_path(rest: &str, prefix: &str) -> Option<String> {
    let rest = rest.trim_end();
    if rest == "/dev/null" {
        return None;
    }
    let path = parse_bare_path(rest);
    Some(strip_path_prefix(&path, prefix))
}

/// Parse a path that may be C-style quoted (as in `rename to "sp ace.rs"`).
fn parse_bare_path(rest: &str) -> String {
    let rest = rest.trim_end();
    if rest.starts_with('"')
        && let Some((path, _)) = split_leading_quoted(rest)
    {
        return path;
    }
    rest.to_owned()
}

/// Split the remainder of a `diff --git ` line into old and new path tokens
/// (still carrying their `a/`/`b/` prefixes). Handles quoted paths and
/// unquoted paths containing spaces.
fn parse_diff_git_paths(rest: &str) -> Option<(String, String)> {
    let rest = rest.trim();

    // Quoted old path: `diff --git "a/sp ace" "b/sp ace"` (new may be unquoted).
    if let Some((old, remainder)) = split_leading_quoted(rest) {
        let remainder = remainder.trim_start();
        let new = match split_leading_quoted(remainder) {
            Some((token, _)) => token,
            None => remainder.to_owned(),
        };
        return Some((old, new));
    }

    // Unquoted old path, quoted new path: `diff --git a/x "b/sp ace"`.
    if let Some(pos) = rest.find(" \"") {
        let (new, _) = split_leading_quoted(rest[pos + 1..].trim_start())?;
        return Some((rest[..pos].to_owned(), new));
    }

    // Both unquoted. Paths with spaces make the boundary ambiguous, so prefer
    // the ` b/` split where both sides agree (the overwhelmingly common case
    // of an unrenamed file), falling back to the first ` b/` boundary.
    let positions: Vec<usize> = rest.match_indices(" b/").map(|(index, _)| index).collect();
    let Some(&first) = positions.first() else {
        let mut parts = rest.split_whitespace();
        return Some((parts.next()?.to_owned(), parts.next()?.to_owned()));
    };
    let boundary = positions
        .iter()
        .copied()
        .find(|&pos| {
            let old = &rest[..pos];
            let new = &rest[pos + 1..];
            old.strip_prefix("a/") == new.strip_prefix("b/")
        })
        .unwrap_or(first);
    Some((rest[..boundary].to_owned(), rest[boundary + 1..].to_owned()))
}

/// Parse a leading C-style quoted string (as produced by git for unusual
/// paths), returning the unescaped value and the remainder after the closing
/// quote. Returns `None` when `s` does not start with a terminated quote.
fn split_leading_quoted(s: &str) -> Option<(String, &str)> {
    let inner = s.strip_prefix('"')?;
    let raw = inner.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut index = 0;
    while index < raw.len() {
        match raw[index] {
            b'"' => {
                return Some((
                    String::from_utf8_lossy(&out).into_owned(),
                    &inner[index + 1..],
                ));
            }
            b'\\' => {
                index += 1;
                match raw.get(index)? {
                    b'n' => {
                        out.push(b'\n');
                        index += 1;
                    }
                    b't' => {
                        out.push(b'\t');
                        index += 1;
                    }
                    b'r' => {
                        out.push(b'\r');
                        index += 1;
                    }
                    digit @ b'0'..=b'7' => {
                        // Up to three octal digits encode one raw byte.
                        let mut value = u32::from(digit - b'0');
                        index += 1;
                        let mut digits = 1;
                        while digits < 3 && index < raw.len() && (b'0'..=b'7').contains(&raw[index])
                        {
                            value = value * 8 + u32::from(raw[index] - b'0');
                            index += 1;
                            digits += 1;
                        }
                        out.push(value as u8);
                    }
                    &other => {
                        out.push(other);
                        index += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    None
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

    #[test]
    fn parses_unquoted_paths_with_spaces() {
        let diff = DiffSet::parse(
            r#"diff --git a/docs/my notes.md b/docs/my notes.md
--- a/docs/my notes.md
+++ b/docs/my notes.md
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();

        assert_eq!(diff.files[0].path, "docs/my notes.md");
        assert_eq!(diff.files[0].old_path, None);
    }

    #[test]
    fn parses_quoted_paths_with_spaces_and_escapes() {
        let diff = DiffSet::parse(
            "diff --git \"a/sp ace \\\"q\\\".rs\" \"b/sp ace \\\"q\\\".rs\"\n--- \"a/sp ace \\\"q\\\".rs\"\n+++ \"b/sp ace \\\"q\\\".rs\"\n@@ -1 +1 @@\n-old\n+new\n",
        )
        .unwrap();

        assert_eq!(diff.files[0].path, "sp ace \"q\".rs");
        assert_eq!(diff.files[0].old_path, None);
    }

    #[test]
    fn parses_quoted_paths_with_octal_utf8_escapes() {
        let diff = DiffSet::parse(
            "diff --git \"a/na\\303\\257ve.txt\" \"b/na\\303\\257ve.txt\"\n--- \"a/na\\303\\257ve.txt\"\n+++ \"b/na\\303\\257ve.txt\"\n@@ -1 +1 @@\n-old\n+new\n",
        )
        .unwrap();

        assert_eq!(diff.files[0].path, "naïve.txt");
    }

    #[test]
    fn parses_rename_with_similarity_index() {
        let diff = DiffSet::parse(
            r#"diff --git a/src/old_name.rs b/src/new_name.rs
similarity index 97%
rename from src/old_name.rs
rename to src/new_name.rs
--- a/src/old_name.rs
+++ b/src/new_name.rs
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();

        assert_eq!(diff.files[0].status, FileStatus::Renamed);
        assert_eq!(diff.files[0].path, "src/new_name.rs");
        assert_eq!(diff.files[0].old_path.as_deref(), Some("src/old_name.rs"));
    }

    #[test]
    fn parses_quoted_rename_paths() {
        let diff = DiffSet::parse(
            "diff --git \"a/old name.rs\" \"b/new name.rs\"\nsimilarity index 90%\nrename from \"old name.rs\"\nrename to \"new name.rs\"\n--- \"a/old name.rs\"\n+++ \"b/new name.rs\"\n@@ -1 +1 @@\n-old\n+new\n",
        )
        .unwrap();

        assert_eq!(diff.files[0].status, FileStatus::Renamed);
        assert_eq!(diff.files[0].path, "new name.rs");
        assert_eq!(diff.files[0].old_path.as_deref(), Some("old name.rs"));
    }

    #[test]
    fn parses_copy_paths() {
        let diff = DiffSet::parse(
            r#"diff --git a/src/base.rs b/src/copy.rs
similarity index 100%
copy from src/base.rs
copy to src/copy.rs
"#,
        )
        .unwrap();

        assert_eq!(diff.files[0].status, FileStatus::Copied);
        assert_eq!(diff.files[0].path, "src/copy.rs");
        assert_eq!(diff.files[0].old_path.as_deref(), Some("src/base.rs"));
    }

    #[test]
    fn added_and_deleted_files_use_dev_null_markers() {
        let diff = DiffSet::parse(
            r#"diff --git a/gone.rs b/gone.rs
deleted file mode 100644
--- a/gone.rs
+++ /dev/null
@@ -1 +0,0 @@
-old
"#,
        )
        .unwrap();

        assert_eq!(diff.files[0].status, FileStatus::Deleted);
        assert_eq!(diff.files[0].path, "gone.rs");
        assert_eq!(diff.files[0].old_path, None);
    }

    #[test]
    fn hunk_content_resembling_markers_does_not_clobber_paths() {
        let diff = DiffSet::parse(
            r#"diff --git a/notes.md b/notes.md
--- a/notes.md
+++ b/notes.md
@@ -1,3 +1,3 @@
 context
--- b/other-file.rs looks like a marker
+++ b/another looks like an added marker
"#,
        )
        .unwrap();

        assert_eq!(diff.files[0].path, "notes.md");
        assert_eq!(diff.files[0].old_path, None);
        // The marker-looking lines stay hunk content.
        assert_eq!(diff.files[0].hunks[0].lines.len(), 3);
    }

    #[test]
    fn repeated_prefix_like_paths_are_not_over_stripped() {
        let diff = DiffSet::parse(
            r#"diff --git a/a/b/x.rs b/a/b/x.rs
--- a/a/b/x.rs
+++ b/a/b/x.rs
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();

        assert_eq!(diff.files[0].path, "a/b/x.rs");
    }

    #[test]
    fn parse_reader_streams_and_matches_string_parse() {
        let input = r#"commit metadata
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,2 +1,3 @@
 fn main() {
-    println!("old");
+    println!("new");
+    println!("extra");
 }
diff --git a/README.md b/README.md
--- a/README.md
+++ b/README.md
@@ -1 +1 @@
-old
+new
"#;

        let from_str = DiffSet::parse(input).unwrap();
        let from_reader = DiffSet::parse_reader(std::io::Cursor::new(input)).unwrap();

        assert_eq!(from_reader.raw_header, from_str.raw_header);
        assert_eq!(from_reader.files.len(), from_str.files.len());
        for (streamed, parsed) in from_reader.files.iter().zip(&from_str.files) {
            assert_eq!(streamed.path, parsed.path);
            assert_eq!(streamed.raw, parsed.raw);
            // Fingerprints must stay stable across parser implementations or
            // saved viewed-state would silently invalidate.
            assert_eq!(streamed.fingerprint, parsed.fingerprint);
            assert_eq!(streamed.additions, parsed.additions);
            assert_eq!(streamed.deletions, parsed.deletions);
        }
    }

    #[test]
    fn git_binary_patch_marks_file_binary() {
        let diff = DiffSet::parse(
            r#"diff --git a/logo.png b/logo.png
index 111..222 100644
GIT binary patch
literal 5
McmZQzU|;|M00aO5

literal 4
LcmZQzU|;|M0Ha
"#,
        )
        .unwrap();

        assert_eq!(diff.files[0].status, FileStatus::Binary);
        assert_eq!(diff.files[0].path, "logo.png");
        assert!(diff.files[0].hunks.is_empty());
        assert!(diff.files[0].is_binary());
    }

    #[test]
    fn binary_rename_and_copy_keep_structural_status() {
        let renamed = DiffSet::parse(
            "diff --git a/old.bin b/new.bin\nsimilarity index 50%\nrename from old.bin\nrename to new.bin\nBinary files a/old.bin and b/new.bin differ",
        )
        .unwrap();
        assert_eq!(renamed.files[0].status, FileStatus::Renamed);
        assert!(renamed.files[0].is_binary());

        let copied = DiffSet::parse(
            "diff --git a/old.bin b/copy.bin\nsimilarity index 50%\ncopy from old.bin\ncopy to copy.bin\nBinary files a/old.bin and b/copy.bin differ",
        )
        .unwrap();
        assert_eq!(copied.files[0].status, FileStatus::Copied);
        assert!(copied.files[0].is_binary());
    }

    #[test]
    fn split_leading_quoted_unescapes_and_returns_rest() {
        let (value, rest) = split_leading_quoted("\"a/sp \\t ace\" trailing").unwrap();

        assert_eq!(value, "a/sp \t ace");
        assert_eq!(rest, " trailing");
        assert!(split_leading_quoted("\"unterminated").is_none());
        assert!(split_leading_quoted("not quoted").is_none());
    }
}

#[cfg(test)]
mod property_tests {
    use super::*;
    use proptest::prelude::*;

    /// Lines that look like diff structure, plus arbitrary noise, so shrunk
    /// counterexamples stay readable.
    fn diffish_line() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("diff --git a/some/file.rs b/some/file.rs".to_owned()),
            Just("--- a/some/file.rs".to_owned()),
            Just("+++ b/some/file.rs".to_owned()),
            Just("+++ /dev/null".to_owned()),
            Just("@@ -1,2 +3,4 @@ fn header()".to_owned()),
            Just("rename from old.rs".to_owned()),
            Just("rename to new.rs".to_owned()),
            Just("new file mode 100644".to_owned()),
            Just("deleted file mode 100644".to_owned()),
            Just("Binary files a/x and b/x differ".to_owned()),
            Just("GIT binary patch".to_owned()),
            "[ +\\-@\\\\\"]{0,6}.{0,40}",
            ".{0,60}",
        ]
    }

    /// git-style C quoting for paths with unusual bytes.
    fn quote_git_path(path: &str) -> String {
        let mut out = String::from("\"");
        for byte in path.bytes() {
            match byte {
                b'"' => out.push_str("\\\""),
                b'\\' => out.push_str("\\\\"),
                b'\n' => out.push_str("\\n"),
                b'\t' => out.push_str("\\t"),
                b'\r' => out.push_str("\\r"),
                0x20..=0x7e => out.push(byte as char),
                other => out.push_str(&format!("\\{other:03o}")),
            }
        }
        out.push('"');
        out
    }

    proptest! {
        #[test]
        fn parse_never_panics_on_arbitrary_input(input in ".{0,2000}") {
            let _ = DiffSet::parse(&input);
        }

        #[test]
        fn parse_never_panics_on_diffish_lines(lines in proptest::collection::vec(diffish_line(), 0..80)) {
            let _ = DiffSet::parse(&lines.join("\n"));
        }

        #[test]
        fn parse_reader_matches_string_parse(lines in proptest::collection::vec(diffish_line(), 0..60)) {
            let input = lines.join("\n");

            let from_str = DiffSet::parse(&input).unwrap();
            let from_reader = DiffSet::parse_reader(std::io::Cursor::new(input.clone())).unwrap();

            prop_assert_eq!(from_str.raw_header, from_reader.raw_header);
            prop_assert_eq!(from_str.files.len(), from_reader.files.len());
            for (left, right) in from_str.files.iter().zip(&from_reader.files) {
                prop_assert_eq!(&left.path, &right.path);
                prop_assert_eq!(&left.fingerprint, &right.fingerprint);
                prop_assert_eq!(&left.raw, &right.raw);
            }
        }

        #[test]
        fn generated_diff_counts_and_numbering_are_consistent(
            adds in proptest::collection::vec("[a-z ]{0,20}", 0..12),
            removes in proptest::collection::vec("[a-z ]{0,20}", 0..12),
            contexts in proptest::collection::vec("[a-z ]{0,20}", 0..12),
            old_start in 1usize..500,
            new_start in 1usize..500,
        ) {
            let mut body = format!(
                "diff --git a/gen.rs b/gen.rs\n--- a/gen.rs\n+++ b/gen.rs\n@@ -{old_start},{} +{new_start},{} @@\n",
                contexts.len() + removes.len(),
                contexts.len() + adds.len(),
            );
            for line in &contexts {
                body.push_str(&format!(" {line}\n"));
            }
            for line in &removes {
                body.push_str(&format!("-{line}\n"));
            }
            for line in &adds {
                body.push_str(&format!("+{line}\n"));
            }

            let diff = DiffSet::parse(&body).unwrap();
            prop_assert_eq!(diff.files.len(), 1);
            let file = &diff.files[0];
            prop_assert_eq!(file.additions, adds.len());
            prop_assert_eq!(file.deletions, removes.len());
            prop_assert_eq!(file.hunks.len(), 1);
            prop_assert_eq!(
                file.hunks[0].lines.len(),
                adds.len() + removes.len() + contexts.len()
            );

            // Line numbering is monotonic per side and starts at the hunk header.
            let mut expected_old = old_start;
            let mut expected_new = new_start;
            for line in &file.hunks[0].lines {
                match line.kind {
                    DiffLineKind::Context => {
                        prop_assert_eq!(line.old_lineno, Some(expected_old));
                        prop_assert_eq!(line.new_lineno, Some(expected_new));
                        expected_old += 1;
                        expected_new += 1;
                    }
                    DiffLineKind::Added => {
                        prop_assert_eq!(line.new_lineno, Some(expected_new));
                        expected_new += 1;
                    }
                    DiffLineKind::Removed => {
                        prop_assert_eq!(line.old_lineno, Some(expected_old));
                        expected_old += 1;
                    }
                    DiffLineKind::Meta => {}
                }
            }
        }

        #[test]
        fn quoted_paths_round_trip(path in "[a-zA-Z0-9 ._\\-\"\\\\éü/]{1,30}") {
            // Avoid path segments git would never emit.
            prop_assume!(!path.starts_with('/') && !path.ends_with('/') && !path.contains("//"));
            prop_assume!(path.trim() == path && path != "/dev/null");

            let quoted_old = quote_git_path(&format!("a/{path}"));
            let quoted_new = quote_git_path(&format!("b/{path}"));
            let body = format!(
                "diff --git {quoted_old} {quoted_new}\n--- {quoted_old}\n+++ {quoted_new}\n@@ -1 +1 @@\n-old\n+new\n"
            );

            let diff = DiffSet::parse(&body).unwrap();
            prop_assert_eq!(diff.files.len(), 1);
            prop_assert_eq!(&diff.files[0].path, &path);
        }
    }
}
