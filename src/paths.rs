//! Per-workspace state and runtime path resolution (docs/decisions.md D6).
//!
//! Gander keeps runtime state out of the project directory. Durable state
//! (viewed marks, comments, the agent overlay) lives under the XDG state
//! dir, keyed by a hash+slug of the canonicalized workspace root; ephemeral
//! endpoints (sockets, instance registry entries, agent logs) live under
//! `XDG_RUNTIME_DIR` when set, else the state dir. A committed `gander.toml`
//! and XDG user config remain the supported configuration surfaces.
//!
//! The legacy project-local `.gander/` directory is read as a one-release
//! migration fallback: when the new location has no state yet, legacy files
//! are copied over once and all subsequent writes go to the new location.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use color_eyre::eyre::{Result, eyre};
use sha2::{Digest, Sha256};

/// Environment inputs for path resolution, captured explicitly so tests can
/// construct them without mutating process-global environment variables.
#[derive(Debug, Clone, Default)]
pub struct PathsEnv {
    pub state_home: Option<PathBuf>,
    pub runtime_dir: Option<PathBuf>,
    pub home: Option<PathBuf>,
}

impl PathsEnv {
    pub fn from_env() -> Self {
        Self {
            state_home: env_path("XDG_STATE_HOME"),
            runtime_dir: env_path("XDG_RUNTIME_DIR"),
            home: env_path("HOME"),
        }
    }
}

fn env_path(key: &str) -> Option<PathBuf> {
    env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Resolved per-workspace locations for durable state and ephemeral
/// endpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePaths {
    /// Canonicalized workspace root the key is derived from.
    pub workspace_root: PathBuf,
    /// Debuggable directory key: `<slug>-<hash>` of the workspace root.
    pub key: String,
    /// Durable state: `<XDG_STATE_HOME>/gander/<key>/`.
    pub state_dir: PathBuf,
    /// Ephemeral endpoints: `<XDG_RUNTIME_DIR>/gander/<key>/`, else the
    /// state dir.
    pub runtime_dir: PathBuf,
    /// Instance registry shared across *all* workspaces (docs/decisions.md
    /// D3): one entry per running TUI, under the same base as the runtime
    /// dirs.
    pub registry_dir: PathBuf,
    /// The deprecated project-local `.gander/` directory (read-only
    /// migration fallback).
    pub legacy_dir: PathBuf,
}

impl WorkspacePaths {
    pub fn resolve(workspace_root: &Path, env: &PathsEnv) -> Result<Self> {
        let workspace_root = workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.to_path_buf());
        let key = workspace_key(&workspace_root);
        let state_base = env
            .state_home
            .clone()
            .or_else(|| env.home.as_ref().map(|home| home.join(".local/state")))
            .ok_or_else(|| eyre!("cannot resolve a state directory: set XDG_STATE_HOME or HOME"))?;
        let ephemeral_base = env
            .runtime_dir
            .clone()
            .unwrap_or_else(|| state_base.clone());
        let state_dir = state_base.join("gander").join(&key);
        let runtime_dir = ephemeral_base.join("gander").join(&key);
        let registry_dir = ephemeral_base.join("gander").join("registry");
        let legacy_dir = workspace_root.join(".gander");
        Ok(Self {
            workspace_root,
            key,
            state_dir,
            runtime_dir,
            registry_dir,
            legacy_dir,
        })
    }

    /// Durable review state (viewed marks, comments).
    pub fn state_file(&self) -> PathBuf {
        self.state_dir.join("state.json")
    }

    /// The shared agent overlay (ordering, flags, chunks, drafts).
    pub fn overlay_file(&self) -> PathBuf {
        self.state_dir.join("agent.json")
    }

    /// Live ACP socket for one gander instance. Sockets are per-instance
    /// (docs/decisions.md D3) so several gander TUIs can run at once
    /// without contending on a single path.
    pub fn instance_socket_file(&self, pid: u32) -> PathBuf {
        self.runtime_dir.join(format!("acp-{pid}.sock"))
    }

    /// Output log of a summoned review agent.
    pub fn agent_log_file(&self) -> PathBuf {
        self.runtime_dir.join("agent.log")
    }

    pub fn legacy_state_file(&self) -> PathBuf {
        self.legacy_dir.join("state.json")
    }

    pub fn legacy_overlay_file(&self) -> PathBuf {
        self.legacy_dir.join("agent.json")
    }

    /// One-release migration fallback (docs/decisions.md D6): copy legacy
    /// `.gander/` state into the new locations when the new files do not
    /// exist yet. Legacy files are left in place; all writes go to the new
    /// locations. Returns the migrated destination paths.
    pub fn migrate_legacy_state(&self) -> Result<Vec<PathBuf>> {
        let candidates = [
            (self.legacy_state_file(), self.state_file()),
            (self.legacy_overlay_file(), self.overlay_file()),
        ];
        let mut migrated = Vec::new();
        for (legacy, new) in candidates {
            if new.exists() || !legacy.exists() {
                continue;
            }
            if let Some(parent) = new.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&legacy, &new)?;
            migrated.push(new);
        }
        Ok(migrated)
    }
}

/// Directory key for a workspace root: a short, human-readable slug of the
/// final path component plus a hash prefix of the full canonical path, e.g.
/// `gander-3f9c2a4b`. Debuggable in `ls` output while still unique per
/// workspace (jj workspaces get distinct keys via their distinct roots).
pub fn workspace_key(workspace_root: &Path) -> String {
    let digest = Sha256::digest(workspace_root.as_os_str().as_encoded_bytes());
    let hash = digest
        .iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let slug = slugify(
        workspace_root
            .file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_default()
            .as_ref(),
    );
    if slug.is_empty() {
        format!("workspace-{hash}")
    } else {
        format!("{slug}-{hash}")
    }
}

/// Lowercase, keep `[a-z0-9]`, collapse everything else to single dashes,
/// and cap the length so keys stay readable.
fn slugify(name: &str) -> String {
    const MAX_LEN: usize = 32;
    let mut slug = String::new();
    let mut pending_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(ch.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
        if slug.len() >= MAX_LEN {
            break;
        }
    }
    slug
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with(state_home: &Path, runtime_dir: Option<&Path>) -> PathsEnv {
        PathsEnv {
            state_home: Some(state_home.to_path_buf()),
            runtime_dir: runtime_dir.map(Path::to_path_buf),
            home: None,
        }
    }

    #[test]
    fn state_dir_uses_xdg_state_home_and_workspace_key() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("My Project");
        fs::create_dir_all(&workspace).unwrap();
        let state_home = dir.path().join("state");

        let paths = WorkspacePaths::resolve(&workspace, &env_with(&state_home, None)).unwrap();

        assert!(paths.key.starts_with("my-project-"));
        assert_eq!(paths.state_dir, state_home.join("gander").join(&paths.key));
        assert_eq!(paths.state_file(), paths.state_dir.join("state.json"));
        assert_eq!(paths.overlay_file(), paths.state_dir.join("agent.json"));
        // No runtime dir: ephemeral endpoints fall back to the state base.
        assert_eq!(paths.runtime_dir, paths.state_dir);
        assert_eq!(
            paths.instance_socket_file(42),
            paths.state_dir.join("acp-42.sock")
        );
        assert_eq!(
            paths.registry_dir,
            state_home.join("gander").join("registry")
        );
    }

    #[test]
    fn runtime_dir_prefers_xdg_runtime_dir() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("repo");
        fs::create_dir_all(&workspace).unwrap();
        let state_home = dir.path().join("state");
        let runtime = dir.path().join("run");

        let paths =
            WorkspacePaths::resolve(&workspace, &env_with(&state_home, Some(&runtime))).unwrap();

        assert_eq!(paths.runtime_dir, runtime.join("gander").join(&paths.key));
        assert_ne!(paths.runtime_dir, paths.state_dir);
        assert_eq!(paths.agent_log_file(), paths.runtime_dir.join("agent.log"));
        assert_eq!(paths.registry_dir, runtime.join("gander").join("registry"));
    }

    #[test]
    fn state_home_falls_back_to_home_local_state() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("repo");
        fs::create_dir_all(&workspace).unwrap();
        let env = PathsEnv {
            state_home: None,
            runtime_dir: None,
            home: Some(dir.path().join("home")),
        };

        let paths = WorkspacePaths::resolve(&workspace, &env).unwrap();

        assert!(
            paths
                .state_dir
                .starts_with(dir.path().join("home/.local/state/gander"))
        );
    }

    #[test]
    fn resolve_errors_without_state_home_or_home() {
        let dir = tempfile::tempdir().unwrap();

        assert!(WorkspacePaths::resolve(dir.path(), &PathsEnv::default()).is_err());
    }

    #[test]
    fn distinct_roots_get_distinct_keys() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("repo");
        let second = dir.path().join("nested").join("repo");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();

        let first_key = workspace_key(&first.canonicalize().unwrap());
        let second_key = workspace_key(&second.canonicalize().unwrap());

        assert_ne!(first_key, second_key);
        assert!(first_key.starts_with("repo-"));
        assert!(second_key.starts_with("repo-"));
    }

    #[test]
    fn workspace_key_is_stable_for_the_same_root() {
        let root = Path::new("/some/stable/path");

        assert_eq!(workspace_key(root), workspace_key(root));
    }

    #[test]
    fn slugify_collapses_punctuation_and_lowercases() {
        assert_eq!(slugify("My Cool_Repo!!v2"), "my-cool-repo-v2");
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("---"), "");
    }

    #[test]
    fn migrates_legacy_state_and_overlay_once() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("repo");
        fs::create_dir_all(workspace.join(".gander")).unwrap();
        fs::write(workspace.join(".gander/state.json"), "{\"legacy\":true}").unwrap();
        fs::write(workspace.join(".gander/agent.json"), "{\"version\":1}").unwrap();
        let paths = WorkspacePaths::resolve(&workspace, &env_with(&dir.path().join("state"), None))
            .unwrap();

        let migrated = paths.migrate_legacy_state().unwrap();

        assert_eq!(migrated, [paths.state_file(), paths.overlay_file()]);
        assert_eq!(
            fs::read_to_string(paths.state_file()).unwrap(),
            "{\"legacy\":true}"
        );
        // Legacy files stay for older releases; a second run migrates nothing.
        assert!(paths.legacy_state_file().exists());
        assert!(paths.migrate_legacy_state().unwrap().is_empty());
    }

    #[test]
    fn migration_never_overwrites_new_state() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("repo");
        fs::create_dir_all(workspace.join(".gander")).unwrap();
        fs::write(workspace.join(".gander/state.json"), "legacy").unwrap();
        let paths = WorkspacePaths::resolve(&workspace, &env_with(&dir.path().join("state"), None))
            .unwrap();
        fs::create_dir_all(&paths.state_dir).unwrap();
        fs::write(paths.state_file(), "new").unwrap();

        let migrated = paths.migrate_legacy_state().unwrap();

        assert!(migrated.is_empty());
        assert_eq!(fs::read_to_string(paths.state_file()).unwrap(), "new");
    }
}
