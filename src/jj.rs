use std::{fmt, path::PathBuf, process::Command};

use color_eyre::eyre::{Result, bail};

#[derive(Debug, Clone)]
pub struct JjCommand {
    binary: PathBuf,
    repo: PathBuf,
    target: ReviewTarget,
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
}
