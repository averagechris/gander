use std::{
    fmt, io,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use color_eyre::eyre::{Result, bail, eyre};

pub struct JjCommand;

pub trait JjBackend {
    /// Apply subprocess limits for long-lived callers such as the web watcher.
    /// In-memory/test backends may ignore this; the CLI backend kills and reaps
    /// an active child on cancellation or timeout.
    fn configure_process_control(&mut self, _control: JjProcessControl) {}
    fn diff(&self, repo: &Path, target: &ReviewTarget) -> Result<String>;
    fn change_summaries(&self, repo: &Path) -> Result<Vec<JjChangeSummary>>;
    /// Changes in the reviewed range (`base..rev`), oldest first.
    fn stack_changes(&self, repo: &Path, target: &ReviewTarget) -> Result<Vec<JjChangeSummary>>;
    /// Consistent author identity across the non-empty reviewed range, read
    /// without snapshotting or mutating the workspace. Each field is `Some`
    /// only when it is non-empty and consistent across every change; mixed,
    /// empty, or ambiguous ranges leave the field `None`.
    fn target_author(&self, _repo: &Path, _target: &ReviewTarget) -> Result<TargetAuthor> {
        Ok(TargetAuthor::default())
    }
    /// Deliberately snapshot the working copy so subsequent read-only queries
    /// can observe disk edits without each query implicitly writing an op.
    fn snapshot_working_copy(&self, repo: &Path) -> Result<()>;
    /// A cheap fingerprint of the reviewed range: the commit ids of every
    /// change in `base..rev`. This is a read-only query; callers that need to
    /// notice working-copy edits should call [`Self::snapshot_working_copy`]
    /// first.
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

/// Consistent author identity of the reviewed range for channel inference.
/// Fields are independent: benign drift in one field (for example a display
/// name spelled differently) does not discard the other as evidence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TargetAuthor {
    pub name: Option<String>,
    pub email: Option<String>,
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
    process_control: Option<JjProcessControl>,
}

#[derive(Debug, Clone)]
pub struct JjProcessControl {
    cancelled: Arc<AtomicBool>,
    timeout: Duration,
}

impl JjProcessControl {
    pub fn new(cancelled: Arc<AtomicBool>, timeout: Duration) -> Self {
        Self { cancelled, timeout }
    }
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

    pub fn is_symbolic(&self) -> bool {
        fn symbolic(revset: &str) -> bool {
            matches!(revset, "@" | "@-") || revset.contains('@') || revset.contains("trunk()")
        }
        symbolic(&self.base) || symbolic(&self.rev)
    }
}

impl fmt::Display for ReviewTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.base, self.rev)
    }
}

impl JjCommand {
    pub fn resolve_binary(configured: &Path) -> Result<PathBuf> {
        resolve_binary_with_probe(configured, can_run_jj)
    }

    fn run_diff(
        binary: &Path,
        repo: &Path,
        target: &ReviewTarget,
        at_operation: Option<&str>,
        process_control: Option<&JjProcessControl>,
    ) -> Result<String> {
        let mut command = Command::new(binary);
        command.arg("--ignore-working-copy");
        if let Some(operation_id) = at_operation {
            command.arg("--at-operation").arg(operation_id);
        }
        let output = run_output(
            command
                .arg("diff")
                .arg("--from")
                .arg(&target.base)
                .arg("--to")
                .arg(&target.rev)
                .arg("--git")
                .arg("--color=never")
                .arg("--no-pager")
                .stdin(Stdio::null())
                .current_dir(repo),
            process_control,
            "jj diff",
        )?;

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

    #[cfg(test)]
    fn operations(binary: &Path, repo: &Path) -> Result<Vec<JjOperationSummary>> {
        Self::operations_controlled(binary, repo, None)
    }

    fn operations_controlled(
        binary: &Path,
        repo: &Path,
        process_control: Option<&JjProcessControl>,
    ) -> Result<Vec<JjOperationSummary>> {
        let mut command = Command::new(binary);
        let output = run_output(command
            .arg("--ignore-working-copy")
            .arg("op")
            .arg("log")
            .arg("--no-graph")
            .arg("--color=never")
            .arg("--no-pager")
            .arg("--template")
            .arg("id.short() ++ \"\\t\" ++ time.end().ago() ++ \"\\t\" ++ description ++ \"\\n\"")
            .stdin(Stdio::null())
            .current_dir(repo), process_control, "jj op log")?;

        if !output.status.success() {
            bail!(
                "jj op log failed with status {}:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }

        parse_operation_summaries(&String::from_utf8_lossy(&output.stdout))
    }

    #[cfg(test)]
    fn change_summaries(binary: &Path, repo: &Path) -> Result<Vec<JjChangeSummary>> {
        Self::log_summaries(
            binary,
            repo,
            "ancestors(@) | trunk() | bookmarks()",
            false,
            None,
        )
    }

    #[cfg(test)]
    fn file_contents(binary: &Path, repo: &Path, rev: &str, path: &str) -> Result<String> {
        Self::file_contents_controlled(binary, repo, rev, path, None)
    }

    fn file_contents_controlled(
        binary: &Path,
        repo: &Path,
        rev: &str,
        path: &str,
        process_control: Option<&JjProcessControl>,
    ) -> Result<String> {
        let mut command = Command::new(binary);
        let output = run_output(
            command
                .arg("--ignore-working-copy")
                .arg("file")
                .arg("show")
                .arg("-r")
                .arg(rev)
                .arg(path)
                .arg("--color=never")
                .arg("--no-pager")
                .stdin(Stdio::null())
                .current_dir(repo),
            process_control,
            "jj file show",
        )?;

        if !output.status.success() {
            bail!(
                "jj file show failed for {path} at {rev} with status {}:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Changes between the review base and tip, oldest first, so callers
    /// can step through the stack change-by-change.
    #[cfg(test)]
    fn stack_changes(
        binary: &Path,
        repo: &Path,
        target: &ReviewTarget,
    ) -> Result<Vec<JjChangeSummary>> {
        Self::log_summaries(
            binary,
            repo,
            &format!("{}..{}", target.base, target.rev),
            true,
            None,
        )
    }

    /// Commit ids of every change in the target range, one per line.
    #[cfg(test)]
    fn change_fingerprint(binary: &Path, repo: &Path, target: &ReviewTarget) -> Result<String> {
        Self::change_fingerprint_controlled(binary, repo, target, None)
    }

    fn change_fingerprint_controlled(
        binary: &Path,
        repo: &Path,
        target: &ReviewTarget,
        process_control: Option<&JjProcessControl>,
    ) -> Result<String> {
        let mut command = Command::new(binary);
        let output = run_output(command
            .arg("--ignore-working-copy")
            .arg("log")
            .arg("-r")
            .arg(format!("{}..{}", target.base, target.rev))
            .arg("--no-graph")
            .arg("--color=never")
            .arg("--no-pager")
            .arg("--template")
            .arg(
                "if(current_working_copy, \"@ \" ++ change_id.short() ++ \" \" ++ commit_id ++ \"\\n\", \"\") ++ change_id.short() ++ \" \" ++ commit_id ++ \"\\n\"",
            )
            .stdin(Stdio::null())
            .current_dir(repo), process_control, "jj log fingerprint")?;

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

    #[cfg(test)]
    fn target_author(binary: &Path, repo: &Path, target: &ReviewTarget) -> Result<TargetAuthor> {
        Self::target_author_controlled(binary, repo, target, None)
    }

    fn target_author_controlled(
        binary: &Path,
        repo: &Path,
        target: &ReviewTarget,
        process_control: Option<&JjProcessControl>,
    ) -> Result<TargetAuthor> {
        let mut command = Command::new(binary);
        let output = run_output(
            command
                .arg("--ignore-working-copy")
                .arg("log")
                .arg("-r")
                // Exclude empty changes (for example the ubiquitous empty
                // working-copy commit sitting on top of a reviewed stack) so
                // channel inference sees only the changes that carry the reviewed
                // work, per docs/annotations.md rule 4 ("every non-empty change").
                .arg(format!("({}..{}) ~ empty()", target.base, target.rev))
                .arg("--no-graph")
                .arg("--color=never")
                .arg("--no-pager")
                .arg("--template")
                .arg("author.name() ++ \"\\x1f\" ++ author.email() ++ \"\\0\"")
                .stdin(Stdio::null())
                .current_dir(repo),
            process_control,
            "jj log authors",
        )?;

        if !output.status.success() {
            bail!(
                "jj log failed reading authors for {target} with status {}:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(parse_consistent_target_author(&String::from_utf8_lossy(
            &output.stdout,
        )))
    }

    fn log_summaries(
        binary: &Path,
        repo: &Path,
        revset: &str,
        reversed: bool,
        process_control: Option<&JjProcessControl>,
    ) -> Result<Vec<JjChangeSummary>> {
        let mut command = Command::new(binary);
        command
            .arg("--ignore-working-copy")
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
        let output = run_output(
            command.stdin(Stdio::null()).current_dir(repo),
            process_control,
            "jj log summaries",
        )?;

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

fn run_output(
    command: &mut Command,
    process_control: Option<&JjProcessControl>,
    operation: &str,
) -> Result<Output> {
    let Some(control) = process_control else {
        return Ok(command.output()?);
    };
    if control.cancelled.load(Ordering::Acquire) {
        bail!("{operation} cancelled before start");
    }

    use wait_timeout::ChildExt;

    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| eyre!("{operation} stdout was not captured"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| eyre!("{operation} stderr was not captured"))?;
    // Drain both pipes while the process runs, otherwise a verbose child can
    // fill an OS pipe and never reach the wait/cancellation loop.
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut stdout = stdout;
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut stderr = stderr;
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let started = Instant::now();
    let poll = Duration::from_millis(25);
    let (status, stopped) = loop {
        if control.cancelled.load(Ordering::Acquire) {
            break (None, Some("cancelled"));
        }
        let elapsed = started.elapsed();
        if elapsed >= control.timeout {
            break (None, Some("timed out"));
        }
        let wait = poll.min(control.timeout.saturating_sub(elapsed));
        if let Some(status) = child.wait_timeout(wait)? {
            break (Some(status), None);
        }
    };
    if let Some(reason) = stopped {
        let _ = child.kill();
        let _ = child.wait();
        let _ = stdout_reader.join();
        let _ = stderr_reader.join();
        bail!(
            "{operation} {reason} after {}ms; child process was killed and reaped",
            started.elapsed().as_millis()
        );
    }
    let stdout = stdout_reader
        .join()
        .map_err(|_| eyre!("{operation} stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| eyre!("{operation} stderr reader panicked"))??;
    Ok(Output {
        status: status.expect("completed child has an exit status"),
        stdout,
        stderr,
    })
}

fn snapshot_working_copy(
    binary: &Path,
    repo: &Path,
    process_control: Option<&JjProcessControl>,
) -> Result<()> {
    let mut command = Command::new(binary);
    let output = run_output(
        command
            .arg("util")
            .arg("snapshot")
            .arg("--color=never")
            .arg("--no-pager")
            .stdin(Stdio::null())
            .current_dir(repo),
        process_control,
        "jj util snapshot",
    )?;

    if !output.status.success() {
        bail!(
            "jj util snapshot failed with status {}:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

impl JjCliBackend {
    pub fn from_configured(configured: &Path) -> Result<Self> {
        Ok(Self {
            binary: JjCommand::resolve_binary(configured)?,
            process_control: None,
        })
    }
}

impl JjBackend for JjCliBackend {
    fn configure_process_control(&mut self, control: JjProcessControl) {
        self.process_control = Some(control);
    }

    fn snapshot_working_copy(&self, repo: &Path) -> Result<()> {
        snapshot_working_copy(&self.binary, repo, self.process_control.as_ref())
    }

    fn diff(&self, repo: &Path, target: &ReviewTarget) -> Result<String> {
        JjCommand::run_diff(
            &self.binary,
            repo,
            target,
            None,
            self.process_control.as_ref(),
        )
    }

    fn change_summaries(&self, repo: &Path) -> Result<Vec<JjChangeSummary>> {
        JjCommand::log_summaries(
            &self.binary,
            repo,
            "ancestors(@) | trunk() | bookmarks()",
            false,
            self.process_control.as_ref(),
        )
    }

    fn stack_changes(&self, repo: &Path, target: &ReviewTarget) -> Result<Vec<JjChangeSummary>> {
        JjCommand::log_summaries(
            &self.binary,
            repo,
            &format!("{}..{}", target.base, target.rev),
            true,
            self.process_control.as_ref(),
        )
    }

    fn target_author(&self, repo: &Path, target: &ReviewTarget) -> Result<TargetAuthor> {
        JjCommand::target_author_controlled(
            &self.binary,
            repo,
            target,
            self.process_control.as_ref(),
        )
    }

    fn change_fingerprint(&self, repo: &Path, target: &ReviewTarget) -> Result<String> {
        JjCommand::change_fingerprint_controlled(
            &self.binary,
            repo,
            target,
            self.process_control.as_ref(),
        )
    }

    fn operations(&self, repo: &Path) -> Result<Vec<JjOperationSummary>> {
        JjCommand::operations_controlled(&self.binary, repo, self.process_control.as_ref())
    }

    fn diff_at_operation(
        &self,
        repo: &Path,
        target: &ReviewTarget,
        operation_id: &str,
    ) -> Result<String> {
        JjCommand::run_diff(
            &self.binary,
            repo,
            target,
            Some(operation_id),
            self.process_control.as_ref(),
        )
    }

    fn file_contents(&self, repo: &Path, rev: &str, path: &str) -> Result<String> {
        JjCommand::file_contents_controlled(
            &self.binary,
            repo,
            rev,
            path,
            self.process_control.as_ref(),
        )
    }

    fn run_command(&self, repo: &Path, args: &[String]) -> Result<String> {
        let mut command = Command::new(&self.binary);
        let output = run_output(
            command
                .args(args)
                .arg("--color=never")
                .arg("--no-pager")
                .stdin(Stdio::null())
                .current_dir(repo),
            self.process_control.as_ref(),
            "jj command",
        )?;

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

/// One record per change: `name \x1f email`, NUL-terminated. Each identity
/// field is kept only when it is non-empty and identical (after trimming)
/// across every record; fields are evaluated independently so drift in one
/// does not discard the other.
fn parse_consistent_target_author(output: &str) -> TargetAuthor {
    let trimmed = output.strip_suffix('\0').unwrap_or(output);
    if trimmed.is_empty() {
        return TargetAuthor::default();
    }
    let mut name: Option<Option<&str>> = None;
    let mut email: Option<Option<&str>> = None;
    for record in trimmed.split('\0') {
        let (record_name, record_email) = record.split_once('\x1f').unwrap_or((record, ""));
        merge_consistent_field(&mut name, record_name);
        merge_consistent_field(&mut email, record_email);
    }
    TargetAuthor {
        name: name.flatten().map(str::to_owned),
        email: email.flatten().map(str::to_owned),
    }
}

/// Fold one record's field into the running consistency state:
/// `None` = unseen, `Some(None)` = poisoned (empty or inconsistent).
fn merge_consistent_field<'a>(state: &mut Option<Option<&'a str>>, value: &'a str) {
    let value = value.trim();
    *state = match *state {
        _ if value.is_empty() => Some(None),
        Some(None) => Some(None),
        None => Some(Some(value)),
        Some(Some(expected)) if expected == value => Some(Some(value)),
        Some(Some(_)) => Some(None),
    };
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

    fn author(name: Option<&str>, email: Option<&str>) -> TargetAuthor {
        TargetAuthor {
            name: name.map(str::to_owned),
            email: email.map(str::to_owned),
        }
    }

    #[test]
    fn target_author_requires_one_consistent_nonempty_range_author_per_field() {
        assert_eq!(
            parse_consistent_target_author("Reviewer\u{1f}reviewer@example.com\0"),
            author(Some("Reviewer"), Some("reviewer@example.com"))
        );
        assert_eq!(
            parse_consistent_target_author("  Reviewer  \u{1f} reviewer@example.com \0"),
            author(Some("Reviewer"), Some("reviewer@example.com"))
        );
        assert_eq!(
            parse_consistent_target_author(
                "Reviewer\u{1f}reviewer@example.com\0 Reviewer \u{1f}reviewer@example.com\0"
            ),
            author(Some("Reviewer"), Some("reviewer@example.com"))
        );
        // Fields are independent: name drift keeps the consistent email and
        // vice versa; an empty field in any record poisons only that field.
        assert_eq!(
            parse_consistent_target_author(
                "chris\u{1f}chris@example.com\0Chris Ericson\u{1f}chris@example.com\0"
            ),
            author(None, Some("chris@example.com"))
        );
        assert_eq!(
            parse_consistent_target_author(
                "Reviewer\u{1f}work@example.com\0Reviewer\u{1f}home@example.com\0"
            ),
            author(Some("Reviewer"), None)
        );
        assert_eq!(
            parse_consistent_target_author("Reviewer\u{1f}\0Reviewer\u{1f}chris@example.com\0"),
            author(Some("Reviewer"), None)
        );
        // Records without the field separator still yield the name.
        assert_eq!(
            parse_consistent_target_author("Reviewer\0"),
            author(Some("Reviewer"), None)
        );
        assert_eq!(parse_consistent_target_author(""), TargetAuthor::default());
        assert_eq!(
            parse_consistent_target_author(
                "Reviewer\u{1f}reviewer@example.com\0Teammate\u{1f}teammate@example.com\0"
            ),
            TargetAuthor::default()
        );
        assert_eq!(
            parse_consistent_target_author("Reviewer\u{1f}reviewer@example.com\0\u{1f}\0"),
            TargetAuthor::default()
        );
    }

    #[cfg(unix)]
    #[test]
    fn target_author_fake_backend_handles_single_same_mixed_and_empty_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("jj-fake-author");
        let args_path = dir.path().join("args");
        let target = ReviewTarget::new("main", "feature");
        for (output, expected) in [
            (
                "Reviewer\\037r@example.com\\0",
                author(Some("Reviewer"), Some("r@example.com")),
            ),
            (
                "Reviewer\\037r@example.com\\0Reviewer\\037r@example.com\\0",
                author(Some("Reviewer"), Some("r@example.com")),
            ),
            (
                "Reviewer\\037r@example.com\\0Teammate\\037r@example.com\\0",
                author(None, Some("r@example.com")),
            ),
            ("", TargetAuthor::default()),
        ] {
            fs::write(
                &script,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf '{}'\n",
                    args_path.display(),
                    output
                ),
            )
            .unwrap();
            let mut permissions = fs::metadata(&script).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&script, permissions).unwrap();

            assert_eq!(
                JjCommand::target_author(&script, dir.path(), &target).unwrap(),
                expected,
                "fake output {output:?}"
            );
            let args = fs::read_to_string(&args_path).unwrap();
            assert!(args.starts_with("--ignore-working-copy\n"));
            assert!(
                args.contains("-r\n(main..feature) ~ empty()\n"),
                "author inference must exclude empty changes such as the \
                 working-copy commit: {args:?}"
            );
            assert!(
                args.contains("author.name() ++ \"\\x1f\" ++ author.email() ++ \"\\0\"\n"),
                "author inference must read name and email: {args:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn read_only_commands_ignore_working_copy() {
        let dir = tempfile::tempdir().unwrap();
        let args_path = dir.path().join("args");
        let script = dir.path().join("jj-fake");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\ncase \" $* \" in\n  *' op log '*) printf 'op123\\tnow\\toperation\\n' ;;\n  *' file show '*) printf 'contents' ;;\n  *' log '*) printf 'abc123\\t\\tdesc\\0' ;;\n  *' diff '*) printf 'diff --git a/a b/a\\n' ;;\nesac\n",
                args_path.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();

        let target = ReviewTarget::trunk_to_current();
        JjCommand::run_diff(&script, dir.path(), &target, None, None).unwrap();
        assert!(
            fs::read_to_string(&args_path)
                .unwrap()
                .contains("--ignore-working-copy\n")
        );

        JjCommand::operations(&script, dir.path()).unwrap();
        assert!(
            fs::read_to_string(&args_path)
                .unwrap()
                .starts_with("--ignore-working-copy\n")
        );

        JjCommand::file_contents(&script, dir.path(), "@", "a.txt").unwrap();
        assert!(
            fs::read_to_string(&args_path)
                .unwrap()
                .starts_with("--ignore-working-copy\n")
        );

        JjCommand::change_fingerprint(&script, dir.path(), &target).unwrap();
        assert!(
            fs::read_to_string(&args_path)
                .unwrap()
                .starts_with("--ignore-working-copy\n")
        );

        JjCommand::change_summaries(&script, dir.path()).unwrap();
        assert!(
            fs::read_to_string(&args_path)
                .unwrap()
                .starts_with("--ignore-working-copy\n")
        );

        JjCommand::target_author(&script, dir.path(), &target).unwrap();
        let args = fs::read_to_string(&args_path).unwrap();
        assert!(args.starts_with("--ignore-working-copy\n"));
        assert!(args.contains("-r\n(trunk()..@) ~ empty()\n"));
        assert!(args.contains("author.name()"));
    }

    #[cfg(unix)]
    #[test]
    fn stack_changes_uses_review_range_so_bookmark_base_is_excluded() {
        let dir = tempfile::tempdir().unwrap();
        let args_path = dir.path().join("args");
        let script = dir.path().join("jj-fake");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\n' \"$@\" > '{}'\nprintf 'chg1\t\tfeat: one\\0'\n",
                args_path.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();

        let target = ReviewTarget::new("main", "@");
        let rows = JjCommand::stack_changes(&script, dir.path(), &target).unwrap();

        assert_eq!(rows.len(), 1);
        let args = fs::read_to_string(&args_path).unwrap();
        assert!(args.contains("-r\nmain..@\n"), "args were {args:?}");
        assert!(!args.contains("trunk()..@"), "args were {args:?}");
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

    #[cfg(unix)]
    #[test]
    fn controlled_backend_kills_and_reaps_a_cancelled_child() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("jj-slow");
        fs::write(&script, "#!/bin/sh\nexec sleep 30\n").unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let control = JjProcessControl::new(cancelled.clone(), Duration::from_secs(10));
        let started = Instant::now();
        let worker = std::thread::spawn(move || {
            JjCommand::change_fingerprint_controlled(
                &script,
                dir.path(),
                &ReviewTarget::new("main", "@"),
                Some(&control),
            )
            .unwrap_err()
            .to_string()
        });
        std::thread::sleep(Duration::from_millis(75));
        cancelled.store(true, Ordering::Release);
        let error = worker.join().unwrap();
        assert!(error.contains("cancelled"), "{error}");
        assert!(error.contains("killed and reaped"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
