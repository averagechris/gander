#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    path::PathBuf,
    process::{Command, Output},
};

use serde_json::{Value, json};

struct Fixture {
    _dir: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    state: PathBuf,
    state_home: PathBuf,
    runtime_dir: PathBuf,
    config_home: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir(&repo).unwrap();
        let repo = repo.canonicalize().unwrap();
        let fake_jj = dir.path().join("fake-jj");
        fs::write(
            &fake_jj,
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  echo "jj 0.99.0"
  exit 0
fi
cat <<'EOF'
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,2 +1,2 @@
-old
+new
 tail
diff --git a/Cargo.lock b/Cargo.lock
--- a/Cargo.lock
+++ b/Cargo.lock
@@ -1 +1 @@
-old lock
+new lock
diff --git a/gen/client.ts b/gen/client.ts
--- a/gen/client.ts
+++ b/gen/client.ts
@@ -1 +1 @@
-old generated
+new generated
diff --git a/ignored/snapshot.txt b/ignored/snapshot.txt
--- a/ignored/snapshot.txt
+++ b/ignored/snapshot.txt
@@ -1 +1 @@
-old snapshot
+new snapshot
EOF
"#,
        )
        .unwrap();
        let mut permissions = fs::metadata(&fake_jj).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_jj, permissions).unwrap();
        let config = dir.path().join("gander.toml");
        fs::write(
            &config,
            format!(
                "[jj]\nbinary = {:?}\n\n[generated]\nglobs = [\"gen/**\"]\n\n[ignore]\nglobs = [\"ignored/**\"]\n",
                fake_jj.display().to_string()
            ),
        )
        .unwrap();
        let state = dir.path().join("state.json");
        fs::write(
            &state,
            serde_json::to_vec_pretty(&json!({
                "meta": { "version": 5 },
                "sessions": [{
                    "id": "attention-session",
                    "target": {
                        "repo": repo,
                        "base": "trunk()",
                        "revision": "@",
                        "revset": "trunk()..@"
                    },
                    "status": "open",
                    "walkthroughs": [],
                    "action_items": []
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        Self {
            state_home: dir.path().join("state-home"),
            runtime_dir: dir.path().join("runtime"),
            config_home: dir.path().join("config-home"),
            home: dir.path().join("home"),
            _dir: dir,
            repo,
            config,
            state,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        let output = self.run_raw(args);
        assert!(
            output.status.success(),
            "gander {args:?} failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn run_raw(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_gander"))
            .arg("--repo")
            .arg(&self.repo)
            .arg("--config")
            .arg(&self.config)
            .arg("--state-file")
            .arg(&self.state)
            .args(args)
            .env("XDG_STATE_HOME", &self.state_home)
            .env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env("HOME", &self.home)
            .output()
            .unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_slice(&self.run(args).stdout).unwrap()
    }
}

#[test]
fn cli_attention_lifecycle_has_stable_json_and_text() {
    let fixture = Fixture::new();
    let initial = fixture.json(&["attention", "list"]);
    assert_eq!(initial["mode"], "effective");
    assert_eq!(initial["default_salience"], "supporting");
    assert!(
        initial["regions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|region| region["salience"] == "supporting")
    );

    let set = fixture.json(&[
        "attention",
        "set",
        "--path",
        "src/lib.rs",
        "--line",
        "1",
        "--salience",
        "spotlight",
        "--rationale",
        "core invariant",
    ]);
    assert_eq!(set["source"], "human");
    assert_eq!(set["stale"], false);
    assert!(set["target"]["anchor"]["diff_fingerprint"].is_string());
    let migrated: Value = serde_json::from_slice(&fs::read(&fixture.state).unwrap()).unwrap();
    assert_eq!(migrated["meta"]["version"], 6);

    let demoted = fixture.json(&["attention", "demote", "--path", "src/lib.rs", "--line", "1"]);
    assert_eq!(demoted["salience"], "supporting");
    assert_eq!(demoted["source"], "human");
    assert_eq!(demoted["stale"], false);
    let text = fixture.run(&[
        "attention",
        "list",
        "--mode",
        "assigned",
        "--format",
        "text",
    ]);
    assert!(
        String::from_utf8(text.stdout)
            .unwrap()
            .contains("supporting  human")
    );

    let cleared = fixture.json(&["attention", "clear", "--path", "src/lib.rs", "--line", "1"]);
    assert_eq!(cleared["cleared"], true);
}

#[test]
fn cli_clear_uses_stale_identity_while_other_mutations_require_current_targets() {
    let fixture = Fixture::new();
    fixture.run(&[
        "attention",
        "set",
        "--path",
        "src/lib.rs",
        "--salience",
        "skim",
    ]);
    let mut state: Value = serde_json::from_slice(&fs::read(&fixture.state).unwrap()).unwrap();
    let target = &mut state["sessions"][0]["attention_regions"][0]["target"];
    target["file"] = "missing.rs".into();
    target["anchor"]["path"] = "missing.rs".into();
    fs::write(&fixture.state, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
    let cleared = fixture.json(&["attention", "clear", "--path", "missing.rs"]);
    assert_eq!(cleared["cleared"], true);

    fixture.run(&[
        "attention",
        "set",
        "--path",
        "src/lib.rs",
        "--line",
        "1",
        "--salience",
        "skim",
    ]);
    let mut state: Value = serde_json::from_slice(&fs::read(&fixture.state).unwrap()).unwrap();
    state["sessions"][0]["attention_regions"][0]["target"]["line"] = 99.into();
    fs::write(&fixture.state, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
    let cleared = fixture.json(&["attention", "clear", "--path", "src/lib.rs", "--line", "99"]);
    assert_eq!(cleared["cleared"], true);

    for args in [
        vec![
            "attention",
            "set",
            "--path",
            "missing.rs",
            "--salience",
            "skim",
        ],
        vec![
            "attention",
            "promote",
            "--path",
            "src/lib.rs",
            "--line",
            "99",
        ],
        vec!["attention", "demote", "--path", "missing.rs"],
    ] {
        let output = fixture.run_raw(&args);
        assert!(!output.status.success(), "unexpected success for {args:?}");
    }
}

#[test]
fn cli_seeds_lockfile_generated_and_ignored_heuristics_without_supporting_noise() {
    let fixture = Fixture::new();
    let seeded = fixture.json(&["attention", "seed-heuristics"]);
    assert_eq!(seeded["update"]["added"], 3);
    let regions = seeded["regions"].as_array().unwrap();
    assert_eq!(regions.len(), 3);
    assert!(regions.iter().all(|region| region["source"] == "heuristic"));
    assert!(regions.iter().all(|region| region["salience"] == "skim"));
    assert!(regions.iter().any(|region| {
        region["target"]["file"] == "ignored/snapshot.txt"
            && region["rationale"]
                .as_str()
                .unwrap()
                .contains("ignore policy")
    }));
    assert!(
        regions
            .iter()
            .all(|region| region["target"]["file"] != "src/lib.rs")
    );

    let recomputed = fixture.json(&["attention", "recompute-heuristics"]);
    assert_eq!(recomputed["update"]["added"], 0);
    assert_eq!(recomputed["regions"].as_array().unwrap().len(), 3);

    let artifact = fixture.json(&["export", "json", "--profile", "human"]);
    let ignored = artifact["attention_regions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|region| region["target"]["file"] == "ignored/snapshot.txt")
        .unwrap();
    assert_eq!(ignored["stale"], false);
    let html = fixture.run(&["export", "html", "--profile", "human"]);
    let html = String::from_utf8(html.stdout).unwrap();
    assert!(html.contains("ignored/snapshot.txt"));
    assert!(!html.contains("<span class=\"pill\">stale</span>"));
}

#[test]
fn artifacts_include_private_attention_for_human_but_not_team() {
    let fixture = Fixture::new();
    fixture.run(&[
        "attention",
        "set",
        "--path",
        "src/lib.rs",
        "--salience",
        "spotlight",
    ]);
    let human = fixture.json(&["export", "json", "--profile", "human"]);
    assert_eq!(human["version"], 11);
    assert_eq!(human["attention_regions"].as_array().unwrap().len(), 1);
    let team = fixture.json(&["export", "json", "--profile", "team"]);
    assert!(team.get("attention_regions").is_none());
}
