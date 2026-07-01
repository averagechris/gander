use std::{path::PathBuf, process::Command};

use color_eyre::eyre::{Result, bail};

#[derive(Debug, Clone)]
pub struct JjCommand {
    repo: PathBuf,
    rev: String,
}

impl JjCommand {
    pub fn new(repo: PathBuf, rev: String) -> Self {
        Self { repo, rev }
    }

    pub fn show(&self) -> Result<String> {
        let output = Command::new("jj")
            .arg("show")
            .arg("--git")
            .arg("--color=never")
            .arg("--no-pager")
            .arg("-r")
            .arg(&self.rev)
            .current_dir(&self.repo)
            .output()?;

        if !output.status.success() {
            bail!(
                "jj show failed with status {}:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}
