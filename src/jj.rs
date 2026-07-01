use std::{
    fmt, io,
    path::{Path, PathBuf},
    process::Command,
};

use color_eyre::eyre::{Result, bail, eyre};

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
    Command::new(binary)
        .arg("--version")
        .output()
        .map(|output| output.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, io::ErrorKind};

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
}
