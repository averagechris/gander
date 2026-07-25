#![cfg(unix)]

use std::{
    fs,
    io::{BufRead as _, Write as _},
    os::unix::net::UnixListener,
    process::Command,
};

fn command(repo: &std::path::Path, state: &std::path::Path, runtime: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_gander"));
    command
        .arg("--repo")
        .arg(repo)
        .env("XDG_STATE_HOME", state)
        .env("XDG_RUNTIME_DIR", runtime);
    command
}

fn registry_path(
    repo: &std::path::Path,
    state: &std::path::Path,
    runtime: &std::path::Path,
) -> std::path::PathBuf {
    let output = command(repo, state, runtime).arg("paths").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("instance registry: "))
        .map(std::path::PathBuf::from)
        .expect("paths prints registry")
}

#[test]
fn current_focus_routes_to_live_instance_with_stable_json_and_compact_text() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let state = temp.path().join("state");
    let runtime = temp.path().join("runtime");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&runtime).unwrap();
    let repo = repo.canonicalize().unwrap();
    let registry = registry_path(&repo, &state, &runtime);
    fs::create_dir_all(&registry).unwrap();
    let socket = temp.path().join("focus.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let focus = serde_json::json!({
        "repo": repo,
        "base": "trunk()",
        "revision": "@",
        "pane": "diff",
        "path": "src/lib.rs",
        "line": {"side":"new","old_line":null,"new_line":42,"hunk_header":"@@ -40 +40 @@"}
    });
    let response = serde_json::json!({"jsonrpc":"2.0","id":1,"result":focus});
    let server = std::thread::spawn(move || {
        for _ in 0..4 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            std::io::BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            if !request.is_empty() {
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(&request).unwrap()["method"],
                    "review/current_focus"
                );
                writeln!(stream, "{response}").unwrap();
            }
        }
    });
    fs::write(
        registry.join(format!("{}.json", std::process::id())),
        serde_json::to_vec(&serde_json::json!({
            "pid": std::process::id(),
            "workspace_root": repo,
            "base": "trunk()",
            "rev": "@",
            "summary": "1 file",
            "socket_path": socket,
            "started_at": "2026-07-24T00:00:00Z",
            "last_input_at": "2026-07-24T00:00:00Z"
        }))
        .unwrap(),
    )
    .unwrap();

    let json = command(&repo, &state, &runtime)
        .arg("current-focus")
        .output()
        .unwrap();
    assert!(
        json.status.success(),
        "{}",
        String::from_utf8_lossy(&json.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value["path"], "src/lib.rs");
    assert_eq!(value["line"]["new_line"], 42);
    assert!(
        value.get("jsonrpc").is_none(),
        "CLI strips transport envelope"
    );

    let text = command(&repo, &state, &runtime)
        .args(["current-focus", "--format", "text"])
        .output()
        .unwrap();
    assert!(
        text.status.success(),
        "{}",
        String::from_utf8_lossy(&text.stderr)
    );
    assert_eq!(
        String::from_utf8(text.stdout).unwrap(),
        "diff src/lib.rs:new:42 trunk()..@\n"
    );
    server.join().unwrap();
}

#[test]
fn current_focus_requires_a_live_instance() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let runtime = temp.path().join("runtime");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&runtime).unwrap();
    let output = command(&repo, &temp.path().join("state"), &runtime)
        .arg("current-focus")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "error: no live instance for this workspace; start `gander tui` or `gander web` and retry\n"
    );
}
