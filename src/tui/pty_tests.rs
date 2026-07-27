//! Production-path tests for OSC 11 background detection (roadmap M19).
//!
//! Each test spawns this very test binary as a child on a fresh PTY (made
//! the child's controlling terminal), so the *real* stack runs end to end:
//! `theme::detect_terminal_background` (terminal-colorsaurus on `/dev/tty`)
//! followed by the real crossterm event reader composed with the real
//! [`super::osc_guard::OscTailGuard`], exactly as `tui::run`/`run_loop`
//! compose them. The parent drives the master side: it answers (or refuses
//! to answer) the OSC 11 + DA1 query with fragmented, malformed, multiple,
//! late, oversized, or absent replies, interleaves genuine user input, and
//! asserts on the child's recorded results that
//!
//! * detection outcomes are correct and bounded,
//! * termios settings are restored after every query outcome,
//! * unrelated input (function keys, Alt chords, split UTF-8, mouse, focus,
//!   paste, resize) survives in order, and
//! * **no OSC payload byte ever surfaces as a command event.**
//!
//! Timing is deterministic-with-margins: children use short query timeouts,
//! parents synchronize on the child's progress file instead of sleeping
//! blindly, and every wait has a hard deadline so a regression hangs a test
//! rather than the suite.

use std::{
    fs,
    io::{Read, Write},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

/// Hard cap for any single child probe.
const CHILD_DEADLINE: Duration = Duration::from_secs(30);
/// How long a child listens for events after detection.
// Parallel suites can briefly starve the parent while a child is already in
// its event loop (notably large projection tests). Keep this below the hard
// child deadline but long enough that synchronization events are not dropped.
const EVENT_WINDOW_MS: u64 = 20_000;

const QUERY: &[u8] = b"\x1b]11;?";
const DA1_QUERY: &[u8] = b"\x1b[c";
const DA1_REPLY: &[u8] = b"\x1b[?6c";

/// PTY/event-reader scenarios are intentionally end-to-end and exercise a
/// process controlling terminal plus crossterm's global event reader. Keep
/// them module-local serial without adding a test dependency; each child still
/// runs on its own fresh PTY, but sibling PTY tests cannot steal scheduler time
/// from timing-sensitive idle/poll boundaries in the same test binary.
static PTY_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn pty_test_lock() -> std::sync::MutexGuard<'static, ()> {
    PTY_TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("PTY test lock poisoned")
}

// ---------------------------------------------------------------------------
// Child probe
// ---------------------------------------------------------------------------

/// Runs only when re-executed by a parent test with `GANDER_PTY_PROBE=1`;
/// otherwise it is an inert, instantly-passing test.
#[test]
fn pty_probe_child() {
    if std::env::var("GANDER_PTY_PROBE").as_deref() != Ok("1") {
        return;
    }
    probe_main().expect("probe child failed");
}

fn probe_main() -> std::io::Result<()> {
    let out_path = std::env::var("GANDER_PTY_OUT").expect("GANDER_PTY_OUT");
    let mut out = fs::File::create(&out_path)?;
    let timeout_ms: u64 = std::env::var("GANDER_PTY_TIMEOUT_MS")
        .expect("GANDER_PTY_TIMEOUT_MS")
        .parse()
        .expect("timeout ms");
    if std::env::var("GANDER_PTY_IGNORE_HUP").as_deref() == Ok("1") {
        // The hangup scenario tests the zero-byte-read/EOF path of the
        // query itself. A real terminal hangup also delivers SIGHUP, whose
        // default disposition would terminate us before the read path is
        // observable; ignore it for this scenario only.
        // SAFETY: SIG_IGN is a valid disposition for SIGHUP.
        unsafe {
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
        }
    }

    let before = tcgetattr_stdin();
    let started = Instant::now();
    // The production detection entry point, with a test-sized budget.
    let detection = super::theme::detect_terminal_background(Duration::from_millis(timeout_ms));
    let elapsed = started.elapsed();
    let after = tcgetattr_stdin();
    writeln!(out, "detect={detection:?}")?;
    writeln!(out, "detect_ms={}", elapsed.as_millis())?;
    writeln!(out, "termios_restored={}", termios_eq(&before, &after))?;
    writeln!(
        out,
        "termios_before=i:{:x} o:{:x} c:{:x} l:{:x}",
        before.c_iflag, before.c_oflag, before.c_cflag, before.c_lflag
    )?;
    writeln!(
        out,
        "termios_after=i:{:x} o:{:x} c:{:x} l:{:x}",
        after.c_iflag, after.c_oflag, after.c_cflag, after.c_lflag
    )?;
    out.flush()?;

    if std::env::var("GANDER_PTY_READ_EVENTS").as_deref() == Ok("1") {
        // Mirror `tui::run`: only a parsed reply disarms containment, then
        // read events through crossterm exactly like `run_loop`
        // (guard.admit on reads, guard.flush_idle on idle ticks).
        let mut guard = match detection {
            super::theme::BackgroundDetection::Detected(_) => {
                super::osc_guard::OscTailGuard::inactive()
            }
            _ => super::osc_guard::OscTailGuard::armed(Instant::now()),
        };
        crossterm::terminal::enable_raw_mode()?;
        writeln!(out, "events_start")?;
        out.flush()?;
        let deadline = Instant::now() + Duration::from_millis(EVENT_WINDOW_MS);
        'listen: while Instant::now() < deadline {
            let admitted = if crossterm::event::poll(Duration::from_millis(50))? {
                guard
                    .admit(crossterm::event::read()?, Instant::now())
                    .events
            } else {
                guard.flush_idle(Instant::now())
            };
            for event in admitted {
                if let crossterm::event::Event::Key(key) = &event
                    && key.code == crossterm::event::KeyCode::Char('Z')
                {
                    break 'listen;
                }
                writeln!(out, "event={event:?}")?;
            }
            out.flush()?;
        }
        crossterm::terminal::disable_raw_mode()?;
    }
    writeln!(out, "done")?;
    out.flush()?;
    Ok(())
}

fn tcgetattr_stdin() -> libc::termios {
    // SAFETY: zeroed termios is a valid out-param for tcgetattr.
    unsafe {
        let mut termios: libc::termios = std::mem::zeroed();
        libc::tcgetattr(0, &mut termios);
        termios
    }
}

fn termios_eq(a: &libc::termios, b: &libc::termios) -> bool {
    // PENDIN/FLUSHO are transient line-discipline status bits the kernel
    // may set while input is pending across a mode switch; they are not
    // configuration and are excluded from the restoration check.
    let transient = libc::PENDIN | libc::FLUSHO;
    a.c_iflag == b.c_iflag
        && a.c_oflag == b.c_oflag
        && a.c_cflag == b.c_cflag
        && (a.c_lflag & !transient) == (b.c_lflag & !transient)
        && a.c_cc == b.c_cc
}

// ---------------------------------------------------------------------------
// Parent-side harness
// ---------------------------------------------------------------------------

/// A probe child on its own PTY. The parent holds the *only* master fd
/// (non-blocking, drained inline, no helper threads) so dropping it delivers
/// a genuine hangup to the child.
struct PtyProbe {
    child: Child,
    master: Option<fs::File>,
    /// Everything the child wrote to the PTY so far (test-harness chatter
    /// plus the OSC/DA1 queries); scanned for the query bytes.
    seen: Vec<u8>,
    out_path: PathBuf,
    _tmp: tempfile::TempDir,
}

fn spawn_probe(timeout_ms: u64, read_events: bool) -> Option<PtyProbe> {
    spawn_probe_with(timeout_ms, read_events, false)
}

fn spawn_probe_with(timeout_ms: u64, read_events: bool, ignore_hup: bool) -> Option<PtyProbe> {
    let mut master_fd: libc::c_int = -1;
    let mut slave_fd: libc::c_int = -1;
    // SAFETY: openpty fills the two fds; null window-size/termios are allowed.
    let rc = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if rc != 0 {
        // Sandboxes without PTY support skip rather than fail.
        eprintln!(
            "skipping PTY test: openpty failed ({})",
            std::io::Error::last_os_error()
        );
        return None;
    }
    // SAFETY: openpty succeeded, both fds are owned by us now.
    let master = unsafe { OwnedFd::from_raw_fd(master_fd) };
    let slave = unsafe { OwnedFd::from_raw_fd(slave_fd) };
    // Non-blocking master: the parent drains output inline between steps.
    // CLOEXEC on both fds so the child cannot inherit the master (which
    // would defeat the hangup scenario); the child's stdio is wired from
    // explicit dups below.
    // SAFETY: valid fds, plain fcntl flag updates.
    unsafe {
        let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
        libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        libc::fcntl(master.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(slave.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let out_path = tmp.path().join("probe-out.txt");

    let test_name = concat!(module_path!(), "::pty_probe_child")
        .split_once("::")
        .expect("crate-qualified module path")
        .1
        .to_owned();
    let mut command = Command::new(std::env::current_exe().expect("current test binary"));
    command
        .arg("--exact")
        .arg(test_name)
        .arg("--test-threads=1")
        .env("GANDER_PTY_PROBE", "1")
        .env("GANDER_PTY_OUT", &out_path)
        .env("GANDER_PTY_TIMEOUT_MS", timeout_ms.to_string())
        .env("TERM", "xterm-256color")
        .stdin(Stdio::from(slave.try_clone().expect("dup slave")))
        .stdout(Stdio::from(slave.try_clone().expect("dup slave")))
        .stderr(Stdio::from(slave));
    if read_events {
        command.env("GANDER_PTY_READ_EVENTS", "1");
    }
    if ignore_hup {
        command.env("GANDER_PTY_IGNORE_HUP", "1");
    }
    // SAFETY: setsid/ioctl are async-signal-safe; fd 0 is the PTY slave.
    unsafe {
        use std::os::unix::process::CommandExt as _;
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().expect("spawn probe child");

    Some(PtyProbe {
        child,
        master: Some(fs::File::from(master)),
        seen: Vec::new(),
        out_path,
        _tmp: tmp,
    })
}

impl PtyProbe {
    fn write_master(&mut self, bytes: &[u8]) {
        let master = self.master.as_mut().expect("master still open");
        master.write_all(bytes).expect("write to PTY master");
        master.flush().expect("flush PTY master");
    }

    /// Drain whatever the child has written to the PTY into `seen`.
    fn drain_master(&mut self) {
        let Some(master) = self.master.as_mut() else {
            return;
        };
        let mut buf = [0u8; 4096];
        loop {
            match master.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => self.seen.extend_from_slice(&buf[..n]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
    }

    /// Wait until the child has emitted `needle` on the PTY (e.g. the OSC
    /// query).
    fn wait_for_master_bytes(&mut self, needle: &[u8]) {
        let deadline = Instant::now() + CHILD_DEADLINE;
        loop {
            self.drain_master();
            if self
                .seen
                .windows(needle.len())
                .any(|window| window == needle)
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "child never wrote {:?} to the PTY",
                String::from_utf8_lossy(needle)
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Wait until the child's progress file contains a line starting with
    /// `prefix`, returning that line.
    fn wait_for_out_line(&mut self, prefix: &str) -> String {
        let deadline = Instant::now() + CHILD_DEADLINE;
        loop {
            self.drain_master();
            if let Ok(contents) = fs::read_to_string(&self.out_path)
                && let Some(line) = contents.lines().find(|line| line.starts_with(prefix))
            {
                return line.to_owned();
            }
            assert!(
                Instant::now() < deadline,
                "child never wrote a {prefix:?} line; out file: {:?}",
                fs::read_to_string(&self.out_path).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Wait for a resize event while nudging SIGWINCH delivery. On Darwin, a
    /// signal sent immediately after the master-side size change can be lost
    /// if the child is between crossterm polls under load; repeating the signal
    /// keeps this as an event-driven barrier rather than a blind sleep.
    fn wait_for_resize_event(&mut self, cols: u16, rows: u16) {
        let prefix = format!("event=Resize({cols}, {rows})");
        let deadline = Instant::now() + CHILD_DEADLINE;
        loop {
            self.drain_master();
            if let Ok(contents) = fs::read_to_string(&self.out_path)
                && contents.lines().any(|line| line.starts_with(&prefix))
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "child never wrote a {prefix:?} line; out file: {:?}",
                fs::read_to_string(&self.out_path).unwrap_or_default()
            );
            // SAFETY: the probe child pid is live until `finish`; SIGWINCH has
            // its normal terminal-resize meaning.
            let _ = unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGWINCH) };
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Close the only master fd, hanging up the child's terminal.
    fn hang_up(&mut self) {
        self.master = None;
    }

    fn finish(mut self) -> ProbeResult {
        let deadline = Instant::now() + CHILD_DEADLINE;
        loop {
            self.drain_master();
            if let Some(status) = self.child.try_wait().expect("try_wait") {
                let contents = fs::read_to_string(&self.out_path).unwrap_or_default();
                return ProbeResult {
                    success: status.success(),
                    contents,
                };
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!(
                    "probe child did not exit in time; out file: {:?}",
                    fs::read_to_string(&self.out_path).unwrap_or_default()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

struct ProbeResult {
    success: bool,
    contents: String,
}

impl ProbeResult {
    fn line(&self, prefix: &str) -> &str {
        self.contents
            .lines()
            .find(|line| line.starts_with(prefix))
            .unwrap_or_else(|| panic!("no {prefix:?} line in {:?}", self.contents))
    }

    fn events(&self) -> Vec<&str> {
        self.contents
            .lines()
            .filter(|line| line.starts_with("event="))
            .collect()
    }

    fn assert_common(&self) {
        assert!(self.success, "probe child failed: {:?}", self.contents);
        assert_eq!(
            self.line("termios_restored="),
            "termios_restored=true",
            "terminal attributes were not restored after the query: {:?}",
            self.contents
        );
        assert!(
            self.contents.contains("done"),
            "probe did not run to completion: {:?}",
            self.contents
        );
    }

    /// The heart of the safety contract: no OSC payload shape may ever be
    /// recorded as a dispatched event.
    fn assert_no_osc_payload_dispatched(&self) {
        for event in self.events() {
            assert!(
                !event.contains("Char(']')"),
                "OSC intro leaked into events: {event}"
            );
            assert!(
                !event.contains("Char(';')") && !event.contains("Char(':')"),
                "OSC payload separator leaked into events: {event}"
            );
            assert!(
                !event.contains("Char('/')"),
                "OSC payload slash leaked into events: {event}"
            );
            assert!(
                !(event.contains("Char('g')") && event.contains("CONTROL")),
                "OSC BEL terminator leaked into events: {event}"
            );
            assert!(
                !(event.contains("Char('\\\\')") && event.contains("ALT")),
                "OSC ST terminator leaked into events: {event}"
            );
        }
    }
}

/// Reply to the color query and the DA1 fence in one burst.
fn full_reply(rgb16: &str, terminator: &[u8]) -> Vec<u8> {
    let mut reply = format!("\x1b]11;rgb:{rgb16}").into_bytes();
    reply.extend_from_slice(terminator);
    reply.extend_from_slice(DA1_REPLY);
    reply
}

fn wait_query(probe: &mut PtyProbe) {
    probe.wait_for_master_bytes(QUERY);
    probe.wait_for_master_bytes(DA1_QUERY);
}

fn resize_pty(probe: &PtyProbe, cols: u16, rows: u16) {
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let master = probe.master.as_ref().expect("master still open");
    // SAFETY: master fd is valid; TIOCSWINSZ takes a winsize pointer.
    let rc = unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &size) };
    assert_eq!(rc, 0, "TIOCSWINSZ failed");
    // Darwin does not reliably deliver SIGWINCH for a master-side TIOCSWINSZ
    // when many PTY children run in parallel. The real terminal contract is
    // the size update plus SIGWINCH, so deliver the signal explicitly instead
    // of allowing this harness race to omit the resize event.
    // SAFETY: the probe child pid is live and SIGWINCH has its normal meaning.
    let rc = unsafe { libc::kill(probe.child.id() as libc::pid_t, libc::SIGWINCH) };
    assert_eq!(rc, 0, "SIGWINCH delivery failed");
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

#[test]
fn detects_bel_reply_and_preserves_unrelated_input_in_order() {
    let _pty_test_guard = pty_test_lock();
    let Some(mut probe) = spawn_probe(2_000, true) else {
        return;
    };
    wait_query(&mut probe);
    probe.write_master(&full_reply("1111/2222/3333", b"\x07"));
    probe.wait_for_out_line("events_start");

    // Unrelated input after startup: F5, Alt+x, a lone Escape, then a
    // complete arrow sequence, split UTF-8 (é as two delayed bytes), an SGR
    // mouse press, a focus report, a bracketed paste, a resize, and a key.
    probe.write_master(b"\x1b[15~");
    probe.write_master(b"\x1bx");
    probe.write_master(b"\x1b");
    // A bare ESC is only unrelated input after crossterm's escape-sequence
    // ambiguity window closes. Synchronize on the child recording that event
    // instead of relying on the parent getting scheduled again after a short
    // sleep; otherwise a busy full-suite run can append the following arrow
    // bytes early enough for crossterm to merge them into one sequence.
    probe.wait_for_out_line("event=Key(KeyEvent { code: Esc");
    probe.write_master(b"\x1b[A");
    probe.write_master(&[0xC3]);
    std::thread::sleep(Duration::from_millis(80));
    probe.write_master(&[0xA9]);
    probe.write_master(b"\x1b[<0;5;6M");
    probe.write_master(b"\x1b[I");
    probe.write_master(b"\x1b[200~hi!\x1b[201~");
    resize_pty(&probe, 100, 40);
    // Do not race the terminating `Z` against crossterm's SIGWINCH delivery
    // under a busy parallel test runner. The resize is part of this scenario's
    // contract, so wait until the child has observed it before stopping.
    probe.wait_for_resize_event(100, 40);
    probe.write_master(b"q");
    probe.write_master(b"Z");

    let result = probe.finish();
    result.assert_common();
    assert_eq!(
        result.line("detect="),
        "detect=Detected(Rgb { r: 17, g: 34, b: 51 })"
    );

    let events = result.events().join("\n");
    // Ordered key inputs survive.
    let ordered = ["F(5)", "Char('x')", "Esc", "Up", "Char('é')", "Char('q')"];
    let mut cursor = 0;
    for needle in ordered {
        let position = events[cursor..]
            .find(needle)
            .unwrap_or_else(|| panic!("missing or misordered {needle}; events:\n{events}"));
        cursor += position + needle.len();
    }
    assert!(events.contains("ALT"), "Alt modifier lost:\n{events}");
    // Mouse, focus, paste, and resize all survive the pipeline.
    assert!(events.contains("Mouse"), "mouse event lost:\n{events}");
    assert!(
        events.contains("FocusGained"),
        "focus event lost:\n{events}"
    );
    assert!(events.contains("Paste(\"hi!\")"), "paste lost:\n{events}");
    assert!(events.contains("Resize(100, 40)"), "resize lost:\n{events}");
    result.assert_no_osc_payload_dispatched();
}

#[test]
fn st_reply_fragmented_at_every_significant_boundary_still_detects() {
    let _pty_test_guard = pty_test_lock();
    let reply = b"\x1b]11;rgb:aaaa/bbbb/cccc\x1b\\";
    // After ESC; after "]"; after "]11;"; after "rgb:"; inside an RGB
    // component; before the ST backslash (between its ESC and '\').
    let boundaries = [1usize, 2, 5, 9, 14, reply.len() - 1];
    for split in boundaries {
        let Some(mut probe) = spawn_probe(2_000, true) else {
            return;
        };
        wait_query(&mut probe);
        probe.write_master(&reply[..split]);
        std::thread::sleep(Duration::from_millis(60));
        probe.write_master(&reply[split..]);
        probe.write_master(DA1_REPLY);
        probe.wait_for_out_line("events_start");
        probe.write_master(b"f");
        probe.write_master(b"Z");
        let result = probe.finish();
        result.assert_common();
        if split == 1 {
            // Upstream quirk: a reply fragmented immediately after its ESC
            // leaves the reader's lookahead buffer empty, so colorsaurus
            // conservatively classifies the terminal as unsupported (safe
            // dark fallback). Crucially it still consumes the entire reply
            // and the DA1 fence while scanning, so nothing can leak.
            assert_eq!(result.line("detect="), "detect=Unsupported");
        } else {
            assert_eq!(
                result.line("detect="),
                "detect=Detected(Rgb { r: 170, g: 187, b: 204 })",
                "split at byte {split}"
            );
        }
        result.assert_no_osc_payload_dispatched();
        let events = result.events();
        assert_eq!(
            events.len(),
            1,
            "protocol bytes leaked at split {split}: {events:?}"
        );
        assert!(events[0].contains("Char('f')"), "user key lost: {events:?}");
    }
}

#[test]
fn esc_only_fragment_times_out_unsupported_and_headless_tail_never_dispatches() {
    let _pty_test_guard = pty_test_lock();
    // The nastiest colorsaurus edge: a reply fragmented immediately after
    // its leading ESC. The reader consumes the ESC, misclassifies the
    // terminal as unsupported when nothing else arrives, and the *plain*
    // `]11;…` tail lands in the live event loop later. Anything other than
    // `Detected` arms the guard, which must contain the tail.
    let Some(mut probe) = spawn_probe(250, true) else {
        return;
    };
    wait_query(&mut probe);
    probe.write_master(b"\x1b");
    let detect = probe.wait_for_out_line("detect=");
    assert!(
        detect == "detect=Unsupported" || detect == "detect=Inconclusive",
        "unexpected verdict for a lone ESC: {detect}"
    );
    probe.wait_for_out_line("events_start");
    probe.write_master(b"]11;rgb:aaaa/bbbb/cccc\x07");
    std::thread::sleep(Duration::from_millis(80));
    probe.write_master(b"w");
    probe.write_master(b"Z");

    let result = probe.finish();
    result.assert_common();
    result.assert_no_osc_payload_dispatched();
    let events = result.events();
    assert_eq!(events.len(), 1, "headless tail leaked: {events:?}");
    assert!(events[0].contains("Char('w')"), "user key lost: {events:?}");
}

#[test]
fn da1_only_terminal_is_conclusively_unsupported_and_input_flows() {
    let _pty_test_guard = pty_test_lock();
    let Some(mut probe) = spawn_probe(2_000, true) else {
        return;
    };
    wait_query(&mut probe);
    probe.write_master(DA1_REPLY);
    probe.wait_for_out_line("events_start");
    probe.write_master(b"h");
    probe.write_master(b"Z");

    let result = probe.finish();
    result.assert_common();
    assert_eq!(result.line("detect="), "detect=Unsupported");
    let events = result.events().join("\n");
    assert!(events.contains("Char('h')"), "key lost:\n{events}");
    result.assert_no_osc_payload_dispatched();
}

#[test]
fn silent_terminal_times_out_and_full_late_reply_never_dispatches() {
    let _pty_test_guard = pty_test_lock();
    let Some(mut probe) = spawn_probe(250, true) else {
        return;
    };
    wait_query(&mut probe);
    // No reply at all: the query must time out on its own budget.
    let detect = probe.wait_for_out_line("detect=");
    assert_eq!(detect, "detect=Inconclusive");
    probe.wait_for_out_line("events_start");
    // The whole reply arrives only after the overall query timeout, while
    // the event loop is live — the guard must contain it.
    probe.write_master(b"\x1b]11;rgb:4444/5555/6666\x07");
    std::thread::sleep(Duration::from_millis(80));
    probe.write_master(b"j");
    probe.write_master(b"Z");

    let result = probe.finish();
    result.assert_common();
    result.assert_no_osc_payload_dispatched();
    let events = result.events();
    assert_eq!(events.len(), 1, "late reply leaked into events: {events:?}");
    assert!(events[0].contains("Char('j')"), "user key lost: {events:?}");
}

#[test]
fn late_tail_of_partially_consumed_reply_never_dispatches() {
    let _pty_test_guard = pty_test_lock();
    let Some(mut probe) = spawn_probe(250, true) else {
        return;
    };
    wait_query(&mut probe);
    // Head arrives inside the query window (consumed by the reader), but
    // the reply is unterminated, so the query still times out…
    probe.write_master(b"\x1b]11;rgb:77");
    let detect = probe.wait_for_out_line("detect=");
    assert_eq!(detect, "detect=Inconclusive");
    probe.wait_for_out_line("events_start");
    // …and the tail (continuation after both the inter-byte gap and the
    // overall timeout) lands in the live event loop as one burst.
    probe.write_master(b"77/8888/9999\x07");
    std::thread::sleep(Duration::from_millis(80));
    probe.write_master(b"k");
    probe.write_master(b"Z");

    let result = probe.finish();
    result.assert_common();
    result.assert_no_osc_payload_dispatched();
    let events = result.events();
    assert_eq!(events.len(), 1, "reply tail leaked: {events:?}");
    assert!(events[0].contains("Char('k')"), "user key lost: {events:?}");
}

#[test]
fn malformed_then_valid_reply_never_reaches_dispatch() {
    let _pty_test_guard = pty_test_lock();
    let Some(mut probe) = spawn_probe(2_000, true) else {
        return;
    };
    wait_query(&mut probe);
    // Malformed frame first; the valid reply and DA1 answer follow in the
    // same stream. The reader consumes all of it (Parse failure), so the
    // event loop must stay clean.
    probe.write_master(b"\x1b]11;bogus\x07");
    probe.write_master(&full_reply("aaaa/bbbb/cccc", b"\x07"));
    let detect = probe.wait_for_out_line("detect=");
    assert_eq!(
        detect, "detect=Inconclusive",
        "malformed reply is a parse failure"
    );
    probe.wait_for_out_line("events_start");
    probe.write_master(b"m");
    probe.write_master(b"Z");

    let result = probe.finish();
    result.assert_common();
    result.assert_no_osc_payload_dispatched();
    let events = result.events();
    assert_eq!(events.len(), 1, "protocol bytes leaked: {events:?}");
    assert!(events[0].contains("Char('m')"), "user key lost: {events:?}");
}

#[test]
fn multiple_replies_are_consumed_without_leaking() {
    let _pty_test_guard = pty_test_lock();
    let Some(mut probe) = spawn_probe(2_000, true) else {
        return;
    };
    wait_query(&mut probe);
    // Two replies: the first wins; the second is consumed with the fence.
    probe.write_master(b"\x1b]11;rgb:1111/1111/1111\x07");
    probe.write_master(&full_reply("2222/2222/2222", b"\x07"));
    probe.wait_for_out_line("events_start");
    probe.write_master(b"n");
    probe.write_master(b"Z");

    let result = probe.finish();
    result.assert_common();
    assert_eq!(
        result.line("detect="),
        "detect=Detected(Rgb { r: 17, g: 17, b: 17 })"
    );
    result.assert_no_osc_payload_dispatched();
    let events = result.events();
    assert_eq!(events.len(), 1, "second reply leaked: {events:?}");
    assert!(events[0].contains("Char('n')"), "user key lost: {events:?}");
}

#[test]
fn oversized_late_reply_is_discarded_and_input_still_flows() {
    let _pty_test_guard = pty_test_lock();
    let Some(mut probe) = spawn_probe(250, true) else {
        return;
    };
    wait_query(&mut probe);
    let detect = probe.wait_for_out_line("detect=");
    assert_eq!(detect, "detect=Inconclusive");
    probe.wait_for_out_line("events_start");
    // An oversized stray reply (far beyond the guard's hold cap), then a key.
    let mut oversized = b"\x1b]11;".to_vec();
    oversized.extend(std::iter::repeat_n(b'a', 300));
    oversized.push(0x07);
    probe.write_master(&oversized);
    std::thread::sleep(Duration::from_millis(80));
    probe.write_master(b"p");
    probe.write_master(b"Z");

    let result = probe.finish();
    result.assert_common();
    result.assert_no_osc_payload_dispatched();
    let events = result.events();
    // Nothing from the oversized payload may dispatch; only the user key.
    assert_eq!(events.len(), 1, "oversized reply leaked: {events:?}");
    assert!(events[0].contains("Char('p')"), "user key lost: {events:?}");
}

#[test]
fn pty_hangup_during_query_fails_fast_without_spinning() {
    let _pty_test_guard = pty_test_lock();
    // A real hangup also delivers SIGHUP, which terminates the app outright
    // (covered implicitly: the process dies instead of spinning). Here we
    // ignore SIGHUP in the child so the query's zero-byte-read/EOF path
    // itself is observable: it must fail fast and restore termios.
    let Some(mut probe) = spawn_probe_with(5_000, false, true) else {
        return;
    };
    wait_query(&mut probe);
    // Close the only master fd: the child sees EOF/HUP on its controlling
    // terminal mid-query. Detection must fail quickly (zero-byte reads
    // terminate the read; no busy loop burns the 5 s budget) and still
    // restore termios.
    probe.hang_up();

    let result = probe.finish();
    let detect = result.line("detect=");
    assert!(
        detect == "detect=Unsupported" || detect == "detect=Inconclusive",
        "unexpected outcome after hangup: {detect}"
    );
    let detect_ms: u64 = result
        .line("detect_ms=")
        .strip_prefix("detect_ms=")
        .expect("prefix")
        .parse()
        .expect("detect_ms value");
    assert!(
        detect_ms < 4_000,
        "detection waited out the full budget after hangup ({detect_ms} ms)"
    );
    assert_eq!(
        result.line("termios_restored="),
        "termios_restored=true",
        "termios not restored after hangup"
    );
    assert!(result.contents.contains("done"));
}
