#![cfg(target_os = "macos")]

use std::{fs, process::Command, time::Duration};

use tempfile::TempDir;

fn run(command: &mut Command) {
    let output = command.output().expect("command starts");
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Production-path acceptance guard for the documented frame log. Expect owns
/// the PTY and consumes terminal output after every input, preventing the child
/// from blocking on a full PTY while the timing sample is collected.
#[test]
fn synthetic_large_diff_steady_state_frame_p99_stays_under_budget() {
    let fixture = TempDir::new().unwrap();
    let repo = fixture.path().join("repo");
    fs::create_dir(&repo).unwrap();
    run(Command::new("jj").args(["git", "init"]).current_dir(&repo));

    let path = repo.join("large.rs");
    let before = (0..30_000)
        .map(|line| format!("fn before_{line}() {{}}\n"))
        .collect::<String>();
    fs::write(&path, before).unwrap();
    run(Command::new("jj")
        .args(["describe", "-m", "baseline"])
        .current_dir(&repo));
    run(Command::new("jj").arg("new").current_dir(&repo));
    let after = (0..30_000)
        .map(|line| format!("fn after_{line}() {{}}\n"))
        .collect::<String>();
    fs::write(&path, after).unwrap();
    run(Command::new("jj")
        .args(["describe", "-m", "large replacement"])
        .current_dir(&repo));

    let config = fixture.path().join("config.toml");
    fs::write(
        &config,
        "[theme]\nmode = \"dark\"\n[limits]\nmax-diff-lines = 5000\n",
    )
    .unwrap();
    let frames = fixture.path().join("frames.log");
    let harness = fixture.path().join("frame-budget.expect");
    let binary = std::env::var("GANDER_PROFILE_BINARY")
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_gander").to_owned());
    fs::write(
        &harness,
        format!(
            r#"#!/usr/bin/expect -f
set timeout 30
log_user 0
spawn -noecho env TERM=xterm-256color GANDER_FRAME_LOG={frames} {binary} --repo {repo} --base @- --config {config} tui
# Drain the initial paint before forcing projection of the hidden large file.
expect -re {{.+}}
send -- "\t"
expect -re {{.+}}
send -- "L"
expect -re {{.+}}
# The first movement closes the force-render frame-log pair. Remaining inputs
# are steady state; every expect drains the PTY before another event is sent.
for {{set i 0}} {{$i < 120}} {{incr i}} {{
    send -- "j"
    expect -re {{.+}}
}}
send -- "q"
expect eof
"#,
            frames = frames.display(),
            binary = binary,
            repo = repo.display(),
            config = config.display(),
        ),
    )
    .unwrap();

    let mut child = Command::new("expect").arg(&harness).spawn().unwrap();
    let status = wait_timeout::ChildExt::wait_timeout(&mut child, Duration::from_secs(45))
        .unwrap()
        .unwrap_or_else(|| {
            child.kill().unwrap();
            panic!("PTY frame-budget harness timed out")
        });
    assert!(status.success(), "expect harness failed: {status}");

    let all_totals = fs::read_to_string(&frames)
        .unwrap()
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let handle = fields
                .next()?
                .strip_prefix("handle_us=")?
                .parse::<u64>()
                .ok()?;
            let draw = fields
                .next()?
                .strip_prefix("draw_us=")?
                .parse::<u64>()
                .ok()?;
            Some(handle + draw)
        })
        .collect::<Vec<_>>();
    println!(
        "frame-log cold/first-materialization samples: {:?}",
        &all_totals[..all_totals.len().min(5)]
    );
    // Exclude startup and first materialization. This is explicitly the
    // steady-state budget, matching the debugging guidance.
    let mut totals = all_totals.into_iter().skip(5).collect::<Vec<_>>();
    assert!(
        totals.len() >= 100,
        "too few drained frame samples: {}",
        totals.len()
    );
    totals.sort_unstable();
    let p99 = totals[(totals.len() * 99).div_ceil(100) - 1];
    println!(
        "frame-log samples={} p50={}us p99={}us max={}us",
        totals.len(),
        totals[totals.len() / 2],
        p99,
        totals.last().unwrap()
    );
    assert!(p99 < 25_000, "steady-state frame p99 {p99}us exceeded 25ms");
}
