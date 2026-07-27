//! Instance registry: one gander per workstream (docs/decisions.md D3).
//!
//! Each running TUI registers itself as one JSON file in a shared registry
//! directory (see [`crate::paths::WorkspacePaths::registry_dir`]): workspace
//! root, review target, summary, per-instance socket path, pid, and a
//! `last_input_at` heartbeat. `gander acp`/`gander mcp` route to the right
//! instance by cwd, and `last_input_at` disambiguates "the review I'm
//! looking at" when several instances share a workspace. Entries are
//! removed on clean exit and garbage-collected when their socket is dead.

use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use color_eyre::eyre::{Context, Result};
use serde::{Deserialize, Serialize};

/// One running gander instance as advertised in the registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceInfo {
    pub pid: u32,
    /// Canonicalized workspace root the instance is reviewing.
    pub workspace_root: PathBuf,
    /// Review target revsets.
    pub base: String,
    pub rev: String,
    /// Human-readable summary line (file/viewed counts).
    pub summary: String,
    /// Per-instance live ACP socket.
    pub socket_path: PathBuf,
    pub started_at: DateTime<Utc>,
    /// Heartbeat: when the human last interacted with this instance.
    pub last_input_at: DateTime<Utc>,
}

/// A live registry entry owned by a running TUI. The entry file is removed
/// on drop so clean exits leave no stale registrations behind.
#[derive(Debug)]
pub struct InstanceRegistration {
    path: PathBuf,
    info: InstanceInfo,
}

/// Minimum interval between heartbeat writes, so key repeat does not turn
/// into a disk write per event.
const HEARTBEAT_INTERVAL: chrono::Duration = chrono::Duration::seconds(2);

impl InstanceRegistration {
    pub fn register(registry_dir: &Path, info: InstanceInfo) -> Result<Self> {
        let path = registry_dir.join(format!("{}.json", info.pid));
        let registration = Self { path, info };
        registration.write()?;
        Ok(registration)
    }

    #[cfg(test)]
    pub fn info(&self) -> &InstanceInfo {
        &self.info
    }

    /// Record human input: refresh `last_input_at` (throttled) and keep the
    /// advertised target/summary current. Returns whether the entry was
    /// rewritten.
    pub fn record_input(&mut self, base: &str, rev: &str, summary: &str) -> Result<bool> {
        self.record_input_at(base, rev, summary, Utc::now())
    }

    pub(crate) fn record_input_at(
        &mut self,
        base: &str,
        rev: &str,
        summary: &str,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let target_changed =
            self.info.base != base || self.info.rev != rev || self.info.summary != summary;
        let heartbeat_due = now - self.info.last_input_at >= HEARTBEAT_INTERVAL;
        if !target_changed && !heartbeat_due {
            return Ok(false);
        }
        self.info.base = base.to_owned();
        self.info.rev = rev.to_owned();
        self.info.summary = summary.to_owned();
        self.info.last_input_at = now;
        self.write()?;
        Ok(true)
    }

    fn write(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Atomic-rename write so concurrent readers never see a torn entry.
        let mut tmp = self.path.clone();
        tmp.set_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(&self.info)?)?;
        fs::rename(&tmp, &self.path)
            .with_context(|| format!("failed to write registry entry {}", self.path.display()))?;
        Ok(())
    }
}

impl Drop for InstanceRegistration {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// All registered instances, unfiltered. Unreadable entries are skipped.
pub fn list_instances(registry_dir: &Path) -> Vec<InstanceInfo> {
    let Ok(entries) = fs::read_dir(registry_dir) else {
        return Vec::new();
    };
    let mut instances: Vec<InstanceInfo> = entries
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                return None;
            }
            serde_json::from_str(&fs::read_to_string(&path).ok()?).ok()
        })
        .collect();
    instances.sort_by_key(|instance| std::cmp::Reverse(instance.last_input_at));
    instances
}

/// Registered instances whose socket still accepts connections, most
/// recently used first. Entries with dead sockets (crashed processes) are
/// garbage-collected from the registry as a side effect.
pub fn live_instances(registry_dir: &Path) -> Vec<InstanceInfo> {
    list_instances(registry_dir)
        .into_iter()
        .filter(|instance| {
            if socket_is_live(&instance.socket_path) {
                true
            } else {
                let _ = fs::remove_file(registry_dir.join(format!("{}.json", instance.pid)));
                false
            }
        })
        .collect()
}

/// The live instance reviewing `workspace_root`, preferring the one the
/// human touched most recently.
pub fn find_live_for_workspace(registry_dir: &Path, workspace_root: &Path) -> Option<InstanceInfo> {
    let workspace_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    live_instances(registry_dir)
        .into_iter()
        .find(|instance| instance.workspace_root == workspace_root)
}

#[cfg(unix)]
fn socket_is_live(socket_path: &Path) -> bool {
    crate::acp::socket::is_live(socket_path)
}

#[cfg(not(unix))]
fn socket_is_live(_socket_path: &Path) -> bool {
    // No Unix sockets: keep entries and let readers decide.
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(pid: u32, root: &Path, socket: &Path) -> InstanceInfo {
        InstanceInfo {
            pid,
            workspace_root: root.to_path_buf(),
            base: "trunk()".to_owned(),
            rev: "@".to_owned(),
            summary: "2 files".to_owned(),
            socket_path: socket.to_path_buf(),
            started_at: Utc::now(),
            last_input_at: Utc::now(),
        }
    }

    #[test]
    fn register_writes_entry_and_drop_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let registry = dir.path().join("registry");

        let registration = InstanceRegistration::register(
            &registry,
            info(101, Path::new("/ws"), Path::new("/sock")),
        )
        .unwrap();

        let listed = list_instances(&registry);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].pid, 101);
        assert_eq!(listed[0].workspace_root, PathBuf::from("/ws"));

        drop(registration);
        assert!(list_instances(&registry).is_empty());
    }

    #[test]
    fn record_input_throttles_heartbeat_but_always_tracks_retarget() {
        let dir = tempfile::tempdir().unwrap();
        let registry = dir.path().join("registry");
        let mut registration = InstanceRegistration::register(
            &registry,
            info(7, Path::new("/ws"), Path::new("/sock")),
        )
        .unwrap();
        let start = registration.info().last_input_at;

        // Same target, heartbeat not yet due: no write.
        let soon = start + chrono::Duration::milliseconds(200);
        assert!(
            !registration
                .record_input_at("trunk()", "@", "2 files", soon)
                .unwrap()
        );

        // Target change writes immediately even inside the throttle window.
        assert!(
            registration
                .record_input_at("trunk()", "@-", "2 files", soon)
                .unwrap()
        );
        assert_eq!(list_instances(&registry)[0].rev, "@-");

        // Heartbeat due: write refreshes last_input_at.
        let later = soon + chrono::Duration::seconds(3);
        assert!(
            registration
                .record_input_at("trunk()", "@-", "2 files", later)
                .unwrap()
        );
        assert_eq!(list_instances(&registry)[0].last_input_at, later);
    }

    #[test]
    fn list_instances_orders_most_recent_input_first_and_skips_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let registry = dir.path().join("registry");
        fs::create_dir_all(&registry).unwrap();
        let mut old = info(1, Path::new("/ws"), Path::new("/sock1"));
        old.last_input_at = Utc::now() - chrono::Duration::minutes(10);
        let recent = info(2, Path::new("/ws"), Path::new("/sock2"));
        fs::write(
            registry.join("1.json"),
            serde_json::to_string(&old).unwrap(),
        )
        .unwrap();
        fs::write(
            registry.join("2.json"),
            serde_json::to_string(&recent).unwrap(),
        )
        .unwrap();
        fs::write(registry.join("junk.json"), "not json").unwrap();
        fs::write(registry.join("readme.txt"), "ignored").unwrap();

        let listed = list_instances(&registry);

        assert_eq!(listed.iter().map(|i| i.pid).collect::<Vec<_>>(), [2, 1]);
    }

    #[cfg(unix)]
    #[test]
    fn live_instances_gc_dead_sockets_and_find_matches_workspace() {
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let registry = dir.path().join("registry");
        fs::create_dir_all(&registry).unwrap();
        let workspace = dir.path().join("ws");
        fs::create_dir_all(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();

        let live_socket = dir.path().join("live.sock");
        let _listener = UnixListener::bind(&live_socket).unwrap();
        let dead_socket = dir.path().join("dead.sock");

        let live = info(10, &workspace, &live_socket);
        let dead = info(11, &workspace, &dead_socket);
        fs::write(
            registry.join("10.json"),
            serde_json::to_string(&live).unwrap(),
        )
        .unwrap();
        fs::write(
            registry.join("11.json"),
            serde_json::to_string(&dead).unwrap(),
        )
        .unwrap();

        let found = find_live_for_workspace(&registry, &workspace).unwrap();
        assert_eq!(found.pid, 10);
        // The dead entry was garbage-collected.
        assert!(!registry.join("11.json").exists());
        assert!(registry.join("10.json").exists());

        // A different workspace matches nothing.
        assert!(find_live_for_workspace(&registry, dir.path()).is_none());
    }
}
