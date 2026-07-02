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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JjChangeSummary {
    pub change_id: String,
    pub bookmarks: String,
    pub description: String,
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
        let output = Command::new(&self.binary)
            .arg("diff")
            .arg("--from")
            .arg(&self.target.base)
            .arg("--to")
            .arg(&self.target.rev)
            .arg("--git")
            .arg("--color=never")
            .arg("--no-pager")
            .stdin(Stdio::null())
            .current_dir(&self.repo)
            .output()?;

        if !output.status.success() {
            bail!(
                "jj diff failed for {} with status {}:\n{}",
                self.target,
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    pub fn change_summaries(binary: &Path, repo: &Path) -> Result<Vec<JjChangeSummary>> {
        let output = Command::new(binary)
            .arg("log")
            .arg("-r")
            .arg("ancestors(@) | trunk() | bookmarks()")
            .arg("--no-graph")
            .arg("--color=never")
            .arg("--no-pager")
            .arg("--template")
            .arg(
                "change_id.short() ++ \"\\t\" ++ bookmarks ++ \"\\t\" ++ description.first_line() ++ \"\\n\"",
            )
            .stdin(Stdio::null())
            .current_dir(repo)
            .output()?;

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
}

fn parse_change_summaries(output: &str) -> Result<Vec<JjChangeSummary>> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let mut parts = line.splitn(3, '\t');
            let change_id = parts.next().unwrap_or_default().trim().to_owned();
            let bookmarks = parts.next().unwrap_or_default().trim().to_owned();
            let description = parts.next().unwrap_or_default().trim().to_owned();
            if change_id.is_empty() {
                bail!("jj log emitted a row without a change id: {line:?}");
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
    fn parses_change_summary_rows() {
        let rows =
            parse_change_summaries("abc123\tmain* feature\tfeat: hello\ndef456\t\t\n").unwrap();

        assert_eq!(
            rows,
            vec![
                JjChangeSummary {
                    change_id: "abc123".to_owned(),
                    bookmarks: "main* feature".to_owned(),
                    description: "feat: hello".to_owned(),
                },
                JjChangeSummary {
                    change_id: "def456".to_owned(),
                    bookmarks: String::new(),
                    description: String::new(),
                },
            ]
        );
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
