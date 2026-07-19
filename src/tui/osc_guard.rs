//! Containment for late OSC 11 replies (roadmap M19).
//!
//! The startup background query (see [`crate::tui::theme`]) runs before
//! Gander enables crossterm raw mode or starts the crossterm event reader
//! (terminal-colorsaurus temporarily uses and restores its own guarded raw
//! mode). A parsed `Detected` reply is the only outcome that proves the reply
//! bytes were consumed. Every other outcome (`Unsupported`, timeout, I/O
//! error, malformed reply) arms this guard because the terminal may still
//! deliver its OSC 11 reply after the TUI event loop is running, and
//! crossterm — which has no OSC grammar — would shred it into key events
//! (`Alt+']'`, digits, `Ctrl+G`, …) that could dispatch arbitrary commands.
//!
//! [`OscTailGuard`] contains that tail at the *event* level. It is not a
//! byte parser and never touches the terminal: it watches the already-decoded
//! crossterm events for the exact event shape of a stray OSC 11 reply (or the
//! tail of one whose head was consumed before the query gave up) and drops
//! only that. Low-confidence candidates (a partial intro or a headless tail)
//! flush in their original order on any deviating event or on the next idle
//! tick, so ordinary typing is delayed by at most one poll interval. Once a
//! payload is *confident* (`Alt+']' 11;` matched — a shape no human
//! produces), it is held across idle ticks and dropped rather than
//! dispatched if interrupted: leaking protocol bytes as commands is strictly
//! worse than losing input of that shape. Resize events are signal-driven
//! and pass through without disturbing a match. Holding is bounded
//! (64 events, a few-second arming window). After one contained reply it
//! resumes watching until the original deadline, so duplicate or late multiple
//! replies in the same armed window are contained too.
//!
//! The residual gaps of this event-level containment (they are real, rare,
//! and bounded) are documented in docs/theme.md §"Documented deltas".

use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

/// How long after any non-`Detected` query outcome stray replies are contained.
pub(crate) const OSC_GUARD_WINDOW: Duration = Duration::from_secs(8);

/// Maximum events held while a candidate payload accumulates. A real OSC 11
/// reply decodes to well under this many events.
const MAX_HELD: usize = 64;

#[derive(Debug, Default)]
enum GuardState {
    #[default]
    Inactive,
    /// Armed: watching for a payload intro or tail.
    Watching,
    /// Saw `Alt+']'`; expecting the literal `11;` prefix next.
    Prefix { held: Vec<Event>, matched: usize },
    /// Confident payload (`Alt+']' 11;` seen); holding until terminator.
    Payload { held: Vec<Event> },
    /// Payload-alphabet characters that may be the tail of a reply whose
    /// head was consumed by the timed-out query.
    Tail { held: Vec<Event> },
    /// Oversized confident payload: drop characters until the terminator.
    Discarding,
}

/// What the run loop should do with events after admitting one.
#[derive(Debug, Default)]
pub(crate) struct Admitted {
    /// Events to dispatch now, in order.
    pub events: Vec<Event>,
}

#[derive(Debug)]
pub(crate) struct OscTailGuard {
    state: GuardState,
    deadline: Option<Instant>,
}

impl Default for OscTailGuard {
    fn default() -> Self {
        Self::inactive()
    }
}

impl OscTailGuard {
    /// A guard that passes every event through untouched.
    pub(crate) fn inactive() -> Self {
        Self {
            state: GuardState::Inactive,
            deadline: None,
        }
    }

    /// Arm containment for [`OSC_GUARD_WINDOW`] starting at `now`.
    pub(crate) fn armed(now: Instant) -> Self {
        Self {
            state: GuardState::Watching,
            deadline: Some(now + OSC_GUARD_WINDOW),
        }
    }

    /// Whether the guard is still doing anything (for tests/diagnostics).
    #[allow(dead_code)]
    pub(crate) fn is_active(&self) -> bool {
        !matches!(self.state, GuardState::Inactive)
    }

    /// Admit one event read from the terminal, returning the events that
    /// should be dispatched now (possibly none, possibly several previously
    /// held ones followed by `event`).
    pub(crate) fn admit(&mut self, event: Event, now: Instant) -> Admitted {
        if matches!(self.state, GuardState::Inactive) {
            return Admitted {
                events: vec![event],
            };
        }
        if self.window_expired(now) {
            let mut events = self.disarm();
            events.push(event);
            return Admitted { events };
        }
        // Resize is signal-driven (SIGWINCH), not part of the terminal byte
        // stream, so it can legitimately interleave a reply's decoded
        // events. It passes straight through without disturbing any match.
        if matches!(event, Event::Resize(..)) {
            return Admitted {
                events: vec![event],
            };
        }

        let class = classify(&event);
        let state = std::mem::take(&mut self.state);
        let (next, events) = match state {
            GuardState::Inactive => unreachable!("handled above"),
            GuardState::Watching => match class {
                EventClass::OscIntro => (
                    GuardState::Prefix {
                        held: vec![event],
                        matched: 0,
                    },
                    Vec::new(),
                ),
                EventClass::PayloadChar(_) => (GuardState::Tail { held: vec![event] }, Vec::new()),
                // A bare terminator with nothing held is indistinguishable
                // from a real keypress; pass it through.
                EventClass::Terminator | EventClass::Other => (GuardState::Watching, vec![event]),
            },
            GuardState::Prefix { mut held, matched } => match class {
                EventClass::PayloadChar(c) if PREFIX.get(matched) == Some(&c) => {
                    held.push(event);
                    let matched = matched + 1;
                    if matched == PREFIX.len() {
                        (GuardState::Payload { held }, Vec::new())
                    } else {
                        (GuardState::Prefix { held, matched }, Vec::new())
                    }
                }
                // Not the OSC 11 prefix: this was user input. Flush.
                _ => {
                    held.push(event);
                    (GuardState::Watching, held)
                }
            },
            GuardState::Payload { mut held } => match class {
                EventClass::PayloadChar(_) => {
                    if held.len() >= MAX_HELD {
                        // Oversized reply: confidently protocol, never user
                        // typing. Drop it and keep discarding to the
                        // terminator so the tail cannot dispatch either.
                        (GuardState::Discarding, Vec::new())
                    } else {
                        held.push(event);
                        (GuardState::Payload { held }, Vec::new())
                    }
                }
                // Full reply contained. Stay armed until the original
                // deadline so duplicate/multiple late replies in the same
                // startup window are contained too.
                EventClass::Terminator => (GuardState::Watching, Vec::new()),
                // A confident payload (`Alt+']' 11;` matched) is essentially
                // never user typing, so on any byte-stream interruption the
                // held prefix is DROPPED, not dispatched: leaking protocol
                // events as commands is strictly worse than losing input of
                // a shape no human produces. The interrupting event itself
                // dispatches normally (or starts a fresh candidate).
                EventClass::OscIntro => (
                    GuardState::Prefix {
                        held: vec![event],
                        matched: 0,
                    },
                    Vec::new(),
                ),
                EventClass::Other => (GuardState::Watching, vec![event]),
            },
            GuardState::Tail { mut held } => match class {
                EventClass::PayloadChar(_) => {
                    if held.len() >= MAX_HELD {
                        // Too long for a reply tail; treat as user input.
                        held.push(event);
                        (GuardState::Watching, held)
                    } else {
                        held.push(event);
                        (GuardState::Tail { held }, Vec::new())
                    }
                }
                EventClass::Terminator => (GuardState::Watching, Vec::new()),
                EventClass::OscIntro => {
                    // Held chars were user input; the intro starts a fresh
                    // candidate.
                    (
                        GuardState::Prefix {
                            held: vec![event],
                            matched: 0,
                        },
                        held,
                    )
                }
                EventClass::Other => {
                    held.push(event);
                    (GuardState::Watching, held)
                }
            },
            GuardState::Discarding => match class {
                EventClass::PayloadChar(_) => (GuardState::Discarding, Vec::new()),
                EventClass::Terminator => (GuardState::Watching, Vec::new()),
                EventClass::OscIntro | EventClass::Other => (GuardState::Discarding, vec![event]),
            },
        };
        self.state = next;
        if matches!(self.state, GuardState::Inactive) {
            self.deadline = None;
        }
        Admitted { events }
    }

    /// Release held events on an idle tick.
    ///
    /// Tail and partial-prefix candidates are released: they are usually
    /// ordinary typing (a reply normally arrives as one contiguous burst),
    /// so they must dispatch in order, delayed by at most one poll interval.
    /// A *confident* payload (`Alt+']' 11;` already matched) is never
    /// released — user input of that exact shape is vanishingly rare, while
    /// a drip-fed late reply fragmented across idle ticks is precisely what
    /// must not reach dispatch.
    pub(crate) fn flush_idle(&mut self, now: Instant) -> Vec<Event> {
        if self.window_expired(now) {
            return self.disarm();
        }
        match std::mem::take(&mut self.state) {
            GuardState::Inactive => {
                self.state = GuardState::Inactive;
                Vec::new()
            }
            GuardState::Watching => {
                self.state = GuardState::Watching;
                Vec::new()
            }
            GuardState::Discarding => {
                self.state = GuardState::Discarding;
                Vec::new()
            }
            GuardState::Payload { held } => {
                self.state = GuardState::Payload { held };
                Vec::new()
            }
            GuardState::Prefix { held, .. } | GuardState::Tail { held } => {
                self.state = GuardState::Watching;
                held
            }
        }
    }

    fn window_expired(&self, now: Instant) -> bool {
        self.deadline.is_some_and(|deadline| now >= deadline)
    }

    /// Release everything and go inactive. Confident payloads are dropped,
    /// not dispatched: an unterminated stray reply must never become
    /// commands even when the arming window closes around it.
    fn disarm(&mut self) -> Vec<Event> {
        self.deadline = None;
        match std::mem::replace(&mut self.state, GuardState::Inactive) {
            GuardState::Prefix { held, .. } | GuardState::Tail { held } => held,
            _ => Vec::new(),
        }
    }
}

/// The literal characters crossterm decodes after `Alt+']'` for an OSC 11
/// reply: `\x1b]11;…`.
const PREFIX: [char; 3] = ['1', '1', ';'];

enum EventClass {
    /// `Alt+']'` — how crossterm decodes `ESC ]`.
    OscIntro,
    /// A character that can occur inside an OSC 11 color payload
    /// (`rgb:aaaa/bbbb/cccc`, `rgba:…`, `#rrggbb`, `cmyk:…` digits).
    PayloadChar(char),
    /// BEL decodes as `Ctrl+G`; `ST` (`ESC \`) decodes as `Alt+'\'`.
    Terminator,
    Other,
}

fn classify(event: &Event) -> EventClass {
    let Event::Key(key) = event else {
        return EventClass::Other;
    };
    if key.kind != KeyEventKind::Press {
        return EventClass::Other;
    }
    let KeyCode::Char(c) = key.code else {
        return EventClass::Other;
    };
    let mods = key.modifiers;
    if c == ']' && mods == KeyModifiers::ALT {
        return EventClass::OscIntro;
    }
    if (c == 'g' || c == 'G') && mods.contains(KeyModifiers::CONTROL) {
        return EventClass::Terminator;
    }
    if c == '\\' && mods == KeyModifiers::ALT {
        return EventClass::Terminator;
    }
    let plain = mods.difference(KeyModifiers::SHIFT).is_empty();
    if plain && is_payload_char(c) {
        return EventClass::PayloadChar(c);
    }
    EventClass::Other
}

fn is_payload_char(c: char) -> bool {
    // `]` is included because a reply whose leading ESC was consumed by the
    // timed-out query resumes with a *plain* `]11;…`.
    c.is_ascii_hexdigit() || matches!(c, 'r' | 'g' | 'i' | ':' | '/' | ';' | '#' | ']')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
    use std::time::{Duration, Instant};

    fn key(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty()))
    }

    fn alt(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT))
    }

    fn ctrl(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    fn f_key(n: u8) -> Event {
        Event::Key(KeyEvent::new(KeyCode::F(n), KeyModifiers::empty()))
    }

    /// The crossterm event shape of `\x1b]11;rgb:1111/2222/3333\x07`.
    fn bel_reply_events() -> Vec<Event> {
        let mut events = vec![alt(']')];
        for c in "11;rgb:1111/2222/3333".chars() {
            events.push(key(c));
        }
        events.push(ctrl('g'));
        events
    }

    fn admit_all(guard: &mut OscTailGuard, events: Vec<Event>, now: Instant) -> Vec<Event> {
        events
            .into_iter()
            .flat_map(|event| guard.admit(event, now).events)
            .collect()
    }

    #[test]
    fn inactive_guard_passes_everything_through() {
        let mut guard = OscTailGuard::inactive();
        let now = Instant::now();
        let out = admit_all(&mut guard, bel_reply_events(), now);
        assert_eq!(out.len(), bel_reply_events().len());
    }

    #[test]
    fn armed_guard_swallows_a_full_bel_terminated_reply() {
        let now = Instant::now();
        let mut guard = OscTailGuard::armed(now);
        let out = admit_all(&mut guard, bel_reply_events(), now);
        assert!(out.is_empty(), "leaked {out:?}");
        // Contained one reply, then kept watching until the original deadline;
        // later ordinary input still flows.
        assert!(guard.is_active());
        let out = guard.admit(key('q'), now).events;
        assert_eq!(out, vec![key('q')]);
    }

    #[test]
    fn armed_guard_swallows_an_st_terminated_reply() {
        let now = Instant::now();
        let mut guard = OscTailGuard::armed(now);
        let mut events = vec![alt(']')];
        for c in "11;rgb:aaaa/bbbb/cccc".chars() {
            events.push(key(c));
        }
        events.push(alt('\\'));
        let out = admit_all(&mut guard, events, now);
        assert!(out.is_empty(), "leaked {out:?}");
        assert!(guard.is_active());
    }

    #[test]
    fn armed_guard_swallows_a_headless_tail() {
        // The query consumed `\x1b]11;rgb:11` before timing out; the rest
        // arrives late.
        let now = Instant::now();
        let mut guard = OscTailGuard::armed(now);
        let mut events = Vec::new();
        for c in "11/2222/3333".chars() {
            events.push(key(c));
        }
        events.push(ctrl('g'));
        let out = admit_all(&mut guard, events, now);
        assert!(out.is_empty(), "leaked {out:?}");
        // Later ordinary input passes.
        assert!(guard.is_active());
        let out = guard.admit(key('j'), now).events;
        assert_eq!(out, vec![key('j')]);
    }

    #[test]
    fn deviating_input_flushes_held_events_in_order() {
        let now = Instant::now();
        let mut guard = OscTailGuard::armed(now);
        // User actually typed Alt+] then "1x".
        let mut out = admit_all(&mut guard, vec![alt(']'), key('1')], now);
        assert!(out.is_empty());
        out = admit_all(&mut guard, vec![key('x')], now);
        assert_eq!(out, vec![alt(']'), key('1'), key('x')]);
    }

    #[test]
    fn function_keys_mouse_and_resize_flush_and_pass() {
        let now = Instant::now();
        let mut guard = OscTailGuard::armed(now);
        assert!(admit_all(&mut guard, vec![key('a'), key('b')], now).is_empty());
        let mouse = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: 1,
            modifiers: KeyModifiers::empty(),
        });
        let out = guard.admit(mouse.clone(), now).events;
        assert_eq!(out, vec![key('a'), key('b'), mouse]);

        let out = guard.admit(f_key(5), now).events;
        assert_eq!(out, vec![f_key(5)]);
        let resize = Event::Resize(80, 24);
        let out = guard.admit(resize.clone(), now).events;
        assert_eq!(out, vec![resize]);
    }

    #[test]
    fn idle_tick_releases_held_user_input_in_order() {
        let now = Instant::now();
        let mut guard = OscTailGuard::armed(now);
        // "gg" is payload-alphabet; the user pauses, the loop goes idle.
        assert!(admit_all(&mut guard, vec![key('g'), key('g')], now).is_empty());
        let out = guard.flush_idle(now);
        assert_eq!(out, vec![key('g'), key('g')]);
        // Still armed for a later stray reply.
        assert!(guard.is_active());
        let out = admit_all(&mut guard, bel_reply_events(), now);
        assert!(out.is_empty());
    }

    #[test]
    fn armed_guard_swallows_multiple_late_replies_in_one_window() {
        let start = Instant::now();
        let mut guard = OscTailGuard::armed(start);

        let out = admit_all(&mut guard, bel_reply_events(), start);
        assert!(out.is_empty(), "first reply leaked {out:?}");
        assert!(guard.is_active(), "guard must retain original window");

        let still_inside = start + OSC_GUARD_WINDOW / 2;
        let out = admit_all(&mut guard, bel_reply_events(), still_inside);
        assert!(out.is_empty(), "second reply leaked {out:?}");
        assert!(
            guard.is_active(),
            "guard must still be armed before deadline"
        );

        let out = guard.admit(key('q'), still_inside).events;
        assert_eq!(out, vec![key('q')], "ordinary input must still flow");
    }

    #[test]
    fn oversized_confident_payload_is_discarded_to_terminator() {
        let now = Instant::now();
        let mut guard = OscTailGuard::armed(now);
        let mut events = vec![alt(']')];
        for _ in 0..(MAX_HELD * 3) {
            events.push(key('a'));
        }
        // Prefix first.
        let mut all = vec![events.remove(0), key('1'), key('1'), key(';')];
        all.extend(events);
        all.push(ctrl('g'));
        let out = admit_all(&mut guard, all, now);
        assert!(
            out.is_empty(),
            "oversized payload leaked {} events",
            out.len()
        );
        // Ordinary input afterwards flows.
        let out = guard.admit(key('q'), now).events;
        assert_eq!(out, vec![key('q')]);
    }

    #[test]
    fn oversized_tail_candidate_flushes_as_user_input() {
        let now = Instant::now();
        let mut guard = OscTailGuard::armed(now);
        let events: Vec<Event> = (0..MAX_HELD + 1).map(|_| key('1')).collect();
        let out = admit_all(&mut guard, events, now);
        assert_eq!(out.len(), MAX_HELD + 1);
    }

    #[test]
    fn resize_mid_payload_passes_through_without_breaking_containment() {
        // SIGWINCH-driven resizes can interleave a reply's decoded events;
        // they must dispatch immediately while the payload stays contained.
        let now = Instant::now();
        let mut guard = OscTailGuard::armed(now);
        let events = bel_reply_events();
        let (head, tail) = events.split_at(8);
        assert!(admit_all(&mut guard, head.to_vec(), now).is_empty());
        let resize = Event::Resize(120, 50);
        let out = guard.admit(resize.clone(), now).events;
        assert_eq!(out, vec![resize], "resize must not flush the payload");
        let out = admit_all(&mut guard, tail.to_vec(), now);
        assert!(out.is_empty(), "payload leaked around a resize: {out:?}");
        assert!(guard.is_active());
    }

    #[test]
    fn byte_stream_interruption_drops_a_confident_payload() {
        // Mouse reports come from the byte stream, so one inside a
        // confident payload means the match is broken — the held protocol
        // prefix is dropped (never dispatched) and the mouse event flows.
        let now = Instant::now();
        let mut guard = OscTailGuard::armed(now);
        let events = bel_reply_events();
        assert!(admit_all(&mut guard, events[..8].to_vec(), now).is_empty());
        let mouse = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: 3,
            modifiers: KeyModifiers::empty(),
        });
        let out = guard.admit(mouse.clone(), now).events;
        assert_eq!(out, vec![mouse], "held payload must not dispatch");
        // Later ordinary input flows; the guard is back to watching.
        let out = guard.admit(key('q'), now).events;
        assert_eq!(out, vec![key('q')]);
    }

    #[test]
    fn window_expiry_disarms_and_flushes() {
        let start = Instant::now();
        let mut guard = OscTailGuard::armed(start);
        assert!(admit_all(&mut guard, vec![key('a')], start).is_empty());
        let later = start + OSC_GUARD_WINDOW + Duration::from_millis(1);
        let out = guard.admit(key('b'), later).events;
        assert_eq!(out, vec![key('a'), key('b')]);
        assert!(!guard.is_active());
        // A reply-shaped burst after expiry passes through (window bounded).
        let out = admit_all(&mut guard, bel_reply_events(), later);
        assert_eq!(out.len(), bel_reply_events().len());
    }

    #[test]
    fn bare_terminators_pass_through_when_nothing_is_held() {
        let now = Instant::now();
        let mut guard = OscTailGuard::armed(now);
        let out = guard.admit(ctrl('g'), now).events;
        assert_eq!(out, vec![ctrl('g')]);
        assert!(guard.is_active());
    }

    #[test]
    fn payload_never_dispatches_even_fragmented_across_idle_ticks() {
        // A drip-fed late reply can straddle idle ticks on a slow link.
        // Once the guard is confident (`Alt+']' 11;` matched) the payload is
        // held across ticks and swallowed at the terminator.
        let now = Instant::now();
        let mut guard = OscTailGuard::armed(now);
        let events = bel_reply_events();
        let (head, tail) = events.split_at(6);
        assert!(admit_all(&mut guard, head.to_vec(), now).is_empty());
        assert!(
            guard.flush_idle(now).is_empty(),
            "confident payload flushed"
        );
        let out = admit_all(&mut guard, tail.to_vec(), now);
        assert!(out.is_empty(), "leaked {out:?}");
        assert!(guard.is_active());
    }

    #[test]
    fn unterminated_confident_payload_is_dropped_at_window_expiry() {
        let start = Instant::now();
        let mut guard = OscTailGuard::armed(start);
        let events = bel_reply_events();
        // Everything but the terminator.
        let held = events[..events.len() - 1].to_vec();
        assert!(admit_all(&mut guard, held, start).is_empty());
        let later = start + OSC_GUARD_WINDOW + Duration::from_millis(1);
        // Expiry drops the payload rather than dispatching it, then passes
        // the fresh user key.
        let out = guard.admit(key('q'), later).events;
        assert_eq!(out, vec![key('q')]);
        assert!(!guard.is_active());
    }
}
