#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    path::PathBuf,
    process::{Command, Output},
};

use serde_json::{Value, json};

const CREATED_AT: &str = "2026-07-19T12:00:00Z";
const SESSION_ID: &str = "acceptance-session";

struct CliFixture {
    _dir: tempfile::TempDir,
    repo: PathBuf,
    config: PathBuf,
    state: PathBuf,
    state_home: PathBuf,
    runtime_dir: PathBuf,
    config_home: PathBuf,
    home: PathBuf,
}

impl CliFixture {
    fn new(comments: Vec<Value>) -> Self {
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
@@ -1 +1 @@
-old
+new
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
                "[jj]\nbinary = {:?}\n\n[identity]\nname = \"CLI Reviewer\"\n\n[agent]\nname = \"Configured Agent\"\n",
                fake_jj.display().to_string()
            ),
        )
        .unwrap();
        let state = dir.path().join("state.json");
        fs::write(
            &state,
            serde_json::to_vec_pretty(&json!({
                "comments": comments,
                "sessions": [{
                    "id": SESSION_ID,
                    "title": null,
                    "target": {
                        "repo": repo,
                        "base": "trunk()",
                        "revision": "@",
                        "revset": "trunk()..@"
                    },
                    "status": "open",
                    "walkthroughs": [],
                    "action_items": [],
                    "created_at": null,
                    "updated_at": null
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
        let output = Command::new(env!("CARGO_BIN_EXE_gander"))
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
            .unwrap();
        assert!(
            output.status.success(),
            "gander {args:?} failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn run_json(&self, args: &[&str]) -> Value {
        serde_json::from_slice(&self.run(args).stdout).unwrap()
    }

    fn load_state(&self) -> Value {
        serde_json::from_slice(&fs::read(&self.state).unwrap()).unwrap()
    }
}

fn comment(id: &str, body: &str, state: &str, channel: &str) -> Value {
    json!({
        "id": id,
        "session_id": SESSION_ID,
        "body": body,
        "state": state,
        "author": { "kind": "human", "name": "Seed Reviewer" },
        "channel": channel,
        "created_at": CREATED_AT
    })
}

fn expected_listed_comment(id: &str, body: &str, state: &str, channel: &str) -> Value {
    let mut value = comment(id, body, state, channel);
    let object = value.as_object_mut().unwrap();
    let selector = id.chars().take(8).collect::<String>();
    object.insert("selector".into(), selector.clone().into());
    object.insert("short_id".into(), selector.into());
    value
}

#[test]
fn cli_comments_list_executes_channel_filter_and_preserves_unfiltered_shape_order() {
    let fixture = CliFixture::new(vec![
        comment("aa-note", "first note", "draft", "note"),
        comment("bb-delegation", "second delegation", "todo", "delegation"),
        comment(
            "cc-collaboration",
            "third collaboration",
            "resolved",
            "collaboration",
        ),
        comment("dd-onboarding", "fourth onboarding", "draft", "onboarding"),
    ]);

    let unfiltered = fixture.run_json(&["comments", "list", "--format", "json"]);
    assert_eq!(
        unfiltered,
        json!({ "comments": [
            expected_listed_comment("aa-note", "first note", "draft", "note"),
            expected_listed_comment("bb-delegation", "second delegation", "todo", "delegation"),
            expected_listed_comment("cc-collaboration", "third collaboration", "resolved", "collaboration"),
            expected_listed_comment("dd-onboarding", "fourth onboarding", "draft", "onboarding")
        ] })
    );

    let filtered_json = fixture.run_json(&[
        "comments",
        "list",
        "--channel",
        "delegation",
        "--format",
        "json",
    ]);
    assert_eq!(
        filtered_json,
        json!({ "comments": [
            expected_listed_comment("bb-delegation", "second delegation", "todo", "delegation")
        ] })
    );

    let filtered_text = fixture.run(&[
        "comments",
        "list",
        "--channel",
        "collaboration",
        "--format",
        "text",
    ]);
    assert_eq!(
        String::from_utf8(filtered_text.stdout).unwrap(),
        "cc-colla [resolved] [note/none]          general/session                      r0  third collaboration\n"
    );
}

#[test]
fn cli_disposition_show_set_clear_executes_and_persists_json_and_text_outputs() {
    let fixture = CliFixture::new(Vec::new());

    assert_eq!(
        fixture.run_json(&["reviews", "disposition", "show", "--format", "json"]),
        json!({ "session_id": SESSION_ID, "disposition": null })
    );
    let set = fixture.run(&[
        "reviews",
        "disposition",
        "set",
        "request-changes",
        "--format",
        "text",
    ]);
    assert_eq!(
        String::from_utf8(set.stdout).unwrap(),
        "disposition: request-changes\n"
    );
    assert_eq!(
        fixture.load_state()["sessions"][0]["disposition"],
        "request-changes"
    );
    assert_eq!(
        fixture.run_json(&["reviews", "disposition", "show", "--format", "json"]),
        json!({ "session_id": SESSION_ID, "disposition": "request-changes" })
    );

    let cleared = fixture.run(&["reviews", "disposition", "clear", "--format", "text"]);
    assert_eq!(
        String::from_utf8(cleared.stdout).unwrap(),
        "disposition: none\n"
    );
    assert!(fixture.load_state()["sessions"][0]["disposition"].is_null());
    assert_eq!(
        fixture.run_json(&["reviews", "disposition", "show", "--format", "json"]),
        json!({ "session_id": SESSION_ID, "disposition": null })
    );
}

#[test]
fn cli_configured_human_identity_is_stamped_by_executed_comment_and_reply_commands() {
    let fixture = CliFixture::new(Vec::new());
    let added = fixture.run_json(&[
        "comments",
        "add",
        "--general",
        "--state",
        "draft",
        "--body",
        "human comment",
        "--format",
        "json",
    ]);
    assert_eq!(
        added["author"],
        json!({ "kind": "human", "name": "CLI Reviewer" })
    );
    let id = added["id"].as_str().unwrap();

    let replied = fixture.run_json(&[
        "comments",
        "reply",
        id,
        "--body",
        "human reply",
        "--format",
        "json",
    ]);
    assert_eq!(
        replied["replies"][0]["author"],
        json!({ "kind": "human", "name": "CLI Reviewer" })
    );

    let state = fixture.load_state();
    assert_eq!(
        state["comments"][0]["author"],
        json!({ "kind": "human", "name": "CLI Reviewer" })
    );
    assert_eq!(
        state["comments"][0]["replies"][0]["author"],
        json!({ "kind": "human", "name": "CLI Reviewer" })
    );
}
