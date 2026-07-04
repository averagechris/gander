use std::{
    fmt, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use color_eyre::eyre::{Result, bail, eyre};

#[derive(Debug, Clone)]
pub struct JjCommand {
    binary: PathBuf,
    repo: PathBuf,
    target: ReviewTarget,
}

pub trait JjBackend {
    fn diff(&self, repo: &Path, target: &ReviewTarget) -> Result<String>;
    fn change_summaries(&self, repo: &Path) -> Result<Vec<JjChangeSummary>>;
    /// Changes in the current stack (`trunk()..@`), oldest first.
    fn stack_changes(&self, repo: &Path) -> Result<Vec<JjChangeSummary>>;
    /// A cheap fingerprint of the reviewed range: the commit ids of every
    /// change in `base..rev`. Running it snapshots the working copy (like
    /// any jj command), so it changes whenever a change is added, rewritten,
    /// abandoned, or the working copy content moves — the poll primitive
    /// behind live review refresh.
    fn change_fingerprint(&self, repo: &Path, target: &ReviewTarget) -> Result<String>;
    /// Recent operations from `jj op log`, newest first.
    fn operations(&self, repo: &Path) -> Result<Vec<JjOperationSummary>>;
    /// The diff for `target` as it looked at a prior operation.
    fn diff_at_operation(
        &self,
        repo: &Path,
        target: &ReviewTarget,
        operation_id: &str,
    ) -> Result<String>;
    /// Full contents of a file at a revision (`jj file show`), used to
    /// expand hunk context beyond what the diff emitted.
    fn file_contents(&self, repo: &Path, rev: &str, path: &str) -> Result<String>;
    /// Run a mutating jj helper command (e.g. `split`/`squash`). Callers must
    /// only invoke this after explicit user confirmation.
    fn run_command(&self, repo: &Path, args: &[String]) -> Result<String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JjOperationSummary {
    pub operation_id: String,
    pub time: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JjChangeSummary {
    pub change_id: String,
    pub bookmarks: String,
    /// Full multiline description; use [`Self::title`] for one-line surfaces.
    pub description: String,
}

impl JjChangeSummary {
    /// The description's first line — what pickers and notices show.
    pub fn title(&self) -> &str {
        self.description.lines().next().unwrap_or_default()
    }
}

#[derive(Debug, Clone)]
pub struct JjCliBackend {
    binary: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewTarget {
    pub base: String,
    pub rev: String,
}

impl ReviewTarget {
    pub fn new(base: impl Into<String>, rev: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            rev: rev.into(),
        }
    }

    pub fn trunk_to_current() -> Self {
        Self::new("trunk()", "@")
    }

    pub fn parent_to_current() -> Self {
        Self::new("@-", "@")
    }
}

impl fmt::Display for ReviewTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.base, self.rev)
    }
}

impl JjCommand {
    pub fn new(binary: PathBuf, repo: PathBuf, target: ReviewTarget) -> Self {
        Self {
            binary,
            repo,
            target,
        }
    }

    pub fn resolve_binary(configured: &Path) -> Result<PathBuf> {
        resolve_binary_with_probe(configured, can_run_jj)
    }

    pub fn diff(&self) -> Result<String> {
        Self::run_diff(&self.binary, &self.repo, &self.target, None)
    }

    fn run_diff(
        binary: &Path,
        repo: &Path,
        target: &ReviewTarget,
        at_operation: Option<&str>,
    ) -> Result<String> {
        let mut command = Command::new(binary);
        if let Some(operation_id) = at_operation {
            command.arg("--at-operation").arg(operation_id);
        }
        let output = command
            .arg("diff")
            .arg("--from")
            .arg(&target.base)
            .arg("--to")
            .arg(&target.rev)
            .arg("--git")
            .arg("--color=never")
            .arg("--no-pager")
            .stdin(Stdio::null())
            .current_dir(repo)
            .output()?;

        if !output.status.success() {
            bail!(
                "jj diff failed for {} with status {}:\n{}",
                target,
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    pub fn operations(binary: &Path, repo: &Path) -> Result<Vec<JjOperationSummary>> {
        let output = Command::new(binary)
            .arg("op")
            .arg("log")
            .arg("--no-graph")
            .arg("--color=never")
            .arg("--no-pager")
            .arg("--template")
            .arg("id.short() ++ \"\\t\" ++ time.end().ago() ++ \"\\t\" ++ description ++ \"\\n\"")
            .stdin(Stdio::null())
            .current_dir(repo)
            .output()?;

        if !output.status.success() {
            bail!(
                "jj op log failed with status {}:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }

        parse_operation_summaries(&String::from_utf8_lossy(&output.stdout))
    }

    pub fn change_summaries(binary: &Path, repo: &Path) -> Result<Vec<JjChangeSummary>> {
        Self::log_summaries(binary, repo, "ancestors(@) | trunk() | bookmarks()", false)
    }

    pub fn file_contents(binary: &Path, repo: &Path, rev: &str, path: &str) -> Result<String> {
        let output = Command::new(binary)
            .arg("file")
            .arg("show")
            .arg("-r")
            .arg(rev)
            .arg(path)
            .arg("--color=never")
            .arg("--no-pager")
            .stdin(Stdio::null())
            .current_dir(repo)
            .output()?;

        if !output.status.success() {
            bail!(
                "jj file show failed for {path} at {rev} with status {}:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Changes between trunk and the working copy, oldest first, so callers
    /// can step through the stack change-by-change.
    pub fn stack_changes(binary: &Path, repo: &Path) -> Result<Vec<JjChangeSummary>> {
        Self::log_summaries(binary, repo, "trunk()..@", true)
    }

    /// Commit ids of every change in the target range, one per line.
    pub fn change_fingerprint(binary: &Path, repo: &Path, target: &ReviewTarget) -> Result<String> {
        let output = Command::new(binary)
            .arg("log")
            .arg("-r")
            .arg(format!("{}..{}", target.base, target.rev))
            .arg("--no-graph")
            .arg("--color=never")
            .arg("--no-pager")
            .arg("--template")
            .arg("commit_id ++ \"\\n\"")
            .stdin(Stdio::null())
            .current_dir(repo)
            .output()?;

        if !output.status.success() {
            bail!(
                "jj log failed fingerprinting {} with status {}:\n{}",
                target,
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn log_summaries(
        binary: &Path,
        repo: &Path,
        revset: &str,
        reversed: bool,
    ) -> Result<Vec<JjChangeSummary>> {
        let mut command = Command::new(binary);
        command
            .arg("log")
            .arg("-r")
            .arg(revset)
            .arg("--no-graph")
            .arg("--color=never")
            .arg("--no-pager")
            .arg("--template")
            // NUL-terminated records so multiline descriptions survive
            // parsing; tabs separate the single-line fields before it.
            .arg("change_id.short() ++ \"\\t\" ++ bookmarks ++ \"\\t\" ++ description ++ \"\\0\"");
        if reversed {
            command.arg("--reversed");
        }
        let output = command.stdin(Stdio::null()).current_dir(repo).output()?;

        if !output.status.success() {
            bail!(
                "jj log failed with status {}:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }

        parse_change_summaries(&String::from_utf8_lossy(&output.stdout))
    }
}

impl JjCliBackend {
    pub fn from_configured(configured: &Path) -> Result<Self> {
        Ok(Self {
            binary: JjCommand::resolve_binary(configured)?,
        })
    }
}

impl JjBackend for JjCliBackend {
    fn diff(&self, repo: &Path, target: &ReviewTarget) -> Result<String> {
        JjCommand::new(self.binary.clone(), repo.to_path_buf(), target.clone()).diff()
    }

    fn change_summaries(&self, repo: &Path) -> Result<Vec<JjChangeSummary>> {
        JjCommand::change_summaries(&self.binary, repo)
    }

    fn stack_changes(&self, repo: &Path) -> Result<Vec<JjChangeSummary>> {
        JjCommand::stack_changes(&self.binary, repo)
    }

    fn change_fingerprint(&self, repo: &Path, target: &ReviewTarget) -> Result<String> {
        JjCommand::change_fingerprint(&self.binary, repo, target)
    }

    fn operations(&self, repo: &Path) -> Result<Vec<JjOperationSummary>> {
        JjCommand::operations(&self.binary, repo)
    }

    fn diff_at_operation(
        &self,
        repo: &Path,
        target: &ReviewTarget,
        operation_id: &str,
    ) -> Result<String> {
        JjCommand::run_diff(&self.binary, repo, target, Some(operation_id))
    }

    fn file_contents(&self, repo: &Path, rev: &str, path: &str) -> Result<String> {
        JjCommand::file_contents(&self.binary, repo, rev, path)
    }

    fn run_command(&self, repo: &Path, args: &[String]) -> Result<String> {
        let output = Command::new(&self.binary)
            .args(args)
            .arg("--color=never")
            .arg("--no-pager")
            .stdin(Stdio::null())
            .current_dir(repo)
            .output()?;

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if !output.status.success() {
            bail!(
                "jj {} failed with status {}:\n{}",
                args.join(" "),
                output.status,
                stderr
            );
        }
        Ok(if stdout.trim().is_empty() {
            stderr
        } else {
            stdout
        })
    }
}

fn parse_operation_summaries(output: &str) -> Result<Vec<JjOperationSummary>> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let mut parts = line.splitn(3, '\t');
            let operation_id = parts.next().unwrap_or_default().trim().to_owned();
            let time = parts.next().unwrap_or_default().trim().to_owned();
            let description = parts.next().unwrap_or_default().trim().to_owned();
            if operation_id.is_empty() {
                bail!("jj op log emitted a row without an operation id: {line:?}");
            }
            Ok(JjOperationSummary {
                operation_id,
                time,
                description,
            })
        })
        .collect()
}

fn parse_change_summaries(output: &str) -> Result<Vec<JjChangeSummary>> {
    output
        .split('\0')
        .filter(|record| !record.trim().is_empty())
        .map(|record| {
            let mut parts = record.splitn(3, '\t');
            let change_id = parts.next().unwrap_or_default().trim().to_owned();
            let bookmarks = parts.next().unwrap_or_default().trim().to_owned();
            // The description keeps its full multiline body (chapter cards
            // show it); only surrounding whitespace is trimmed.
            let description = parts.next().unwrap_or_default().trim().to_owned();
            if change_id.is_empty() {
                bail!("jj log emitted a record without a change id: {record:?}");
            }
            Ok(JjChangeSummary {
                change_id,
                bookmarks,
                description,
            })
        })
        .collect()
}

fn resolve_binary_with_probe(
    configured: &Path,
    mut can_run: impl FnMut(&Path) -> io::Result<bool>,
) -> Result<PathBuf> {
    match can_run(configured) {
        Ok(true) => return Ok(configured.to_path_buf()),
        Ok(false) => bail!(
            "configured jj binary '{}' ran but did not behave like jj. Set [jj].binary to a valid jj executable, or remove the setting to use jj from PATH.",
            configured.display()
        ),
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            bail!(
                "failed to run configured jj binary '{}': {error}. Set [jj].binary to a valid jj executable, or remove the setting to use jj from PATH.",
                configured.display()
            );
        }
        Err(_) => {}
    }

    let path_jj = Path::new("jj");
    if configured != path_jj
        && let Ok(true) = can_run(path_jj)
    {
        return Ok(path_jj.to_path_buf());
    }

    Err(eyre!(
        "could not find a usable jj executable. Tried configured [jj].binary '{}'{}.

Install jj and ensure it is on PATH, or set [jj].binary to an absolute path, for example:

[jj]
binary = \"/nix/store/.../bin/jj\"",
        configured.display(),
        if configured == path_jj {
            " on PATH"
        } else {
            " and then 'jj' on PATH"
        }
    ))
}

fn can_run_jj(binary: &Path) -> io::Result<bool> {
    can_run_jj_with_timeout(binary, Duration::from_secs(2))
}

fn can_run_jj_with_timeout(binary: &Path, timeout: Duration) -> io::Result<bool> {
    use wait_timeout::ChildExt;

    let mut child = Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    match child.wait_timeout(timeout)? {
        Some(_status) => {
            let output = child.wait_with_output()?;
            Ok(output.status.success()
                && version_output_looks_like_jj(&String::from_utf8_lossy(&output.stdout)))
        }
        None => {
            child.kill()?;
            let _ = child.wait();
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "jj binary '{}' did not respond to --version within {}ms; this may be the wrong package (for Nix, use nixpkgs#jujutsu, not nixpkgs#jj)",
                    binary.display(),
                    timeout.as_millis()
                ),
            ))
        }
    }
}

fn version_output_looks_like_jj(output: &str) -> bool {
    output.trim_start().starts_with("jj ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, fs, io::ErrorKind};

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn resolve_binary_uses_configured_binary_when_available() {
        let resolved = resolve_binary_with_probe(Path::new("/custom/jj"), |path| {
            Ok(path == Path::new("/custom/jj"))
        })
        .unwrap();

        assert_eq!(resolved, PathBuf::from("/custom/jj"));
    }

    #[test]
    fn resolve_binary_falls_back_to_path_when_configured_missing() {
        let resolved = resolve_binary_with_probe(Path::new("/missing/jj"), |path| {
            if path == Path::new("jj") {
                Ok(true)
            } else {
                Err(io::Error::new(ErrorKind::NotFound, "missing"))
            }
        })
        .unwrap();

        assert_eq!(resolved, PathBuf::from("jj"));
    }

    #[test]
    fn resolve_binary_errors_actionably_when_no_jj_exists() {
        let error = resolve_binary_with_probe(Path::new("/missing/jj"), |_| {
            Err(io::Error::new(ErrorKind::NotFound, "missing"))
        })
        .unwrap_err()
        .to_string();

        assert!(error.contains("could not find a usable jj executable"));
        assert!(error.contains("[jj]"));
        assert!(error.contains("/nix/store/.../bin/jj"));
    }

    #[test]
    fn resolve_binary_does_not_fallback_for_invalid_existing_configured_binary() {
        let mut probes = BTreeMap::new();
        probes.insert(PathBuf::from("/not-jj"), Ok(false));
        probes.insert(PathBuf::from("jj"), Ok(true));

        let error = resolve_binary_with_probe(Path::new("/not-jj"), |path| {
            probes
                .remove(path)
                .unwrap_or_else(|| Err(io::Error::new(ErrorKind::NotFound, "missing")))
        })
        .unwrap_err()
        .to_string();

        assert!(error.contains("did not behave like jj"));
    }

    #[test]
    fn version_output_must_look_like_jujutsu() {
        assert!(version_output_looks_like_jj("jj 0.42.0\n"));
        assert!(!version_output_looks_like_jj("json-join 1.0.0\n"));
        assert!(!version_output_looks_like_jj(""));
    }

    #[test]
    fn parses_change_summary_records_with_multiline_descriptions() {
        let rows = parse_change_summaries(
            "abc123\tmain* feature\tfeat: hello\n\nbody line one\nbody line two\0def456\t\t\0",
        )
        .unwrap();

        assert_eq!(
            rows,
            vec![
                JjChangeSummary {
                    change_id: "abc123".to_owned(),
                    bookmarks: "main* feature".to_owned(),
                    description: "feat: hello\n\nbody line one\nbody line two".to_owned(),
                },
                JjChangeSummary {
                    change_id: "def456".to_owned(),
                    bookmarks: String::new(),
                    description: String::new(),
                },
            ]
        );
        assert_eq!(rows[0].title(), "feat: hello");
        assert_eq!(rows[1].title(), "");
    }

    #[test]
    fn parses_operation_summary_rows() {
        let rows = parse_operation_summaries(
            "op123\t5 minutes ago\tsnapshot working copy\nop456\t2 days ago\t\n",
        )
        .unwrap();

        assert_eq!(
            rows,
            vec![
                JjOperationSummary {
                    operation_id: "op123".to_owned(),
                    time: "5 minutes ago".to_owned(),
                    description: "snapshot working copy".to_owned(),
                },
                JjOperationSummary {
                    operation_id: "op456".to_owned(),
                    time: "2 days ago".to_owned(),
                    description: String::new(),
                },
            ]
        );
        assert!(parse_operation_summaries("\tno id\n").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn can_run_jj_times_out_for_blocking_binary() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("jj-blocks");
        fs::write(&script, "#!/bin/sh\nsleep 10\n").unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();

        let error = can_run_jj_with_timeout(&script, Duration::from_millis(25)).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::TimedOut);
        assert!(error.to_string().contains("nixpkgs#jujutsu"));
    }
}
