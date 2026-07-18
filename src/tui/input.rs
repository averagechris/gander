//! One typed terminal-input pipeline for both OSC responses and user events.
//!
//! Termina supplies the maintained incremental escape parser. On Unix Gander
//! owns the FD read loop so parser framing is independent of kernel short-read
//! boundaries: partial sequences get a bounded inter-byte grace period,
//! protocol frames are size-limited, and an unfinished OSC 11 query is reset at
//! its deadline. Typed events are converted through maintained terminput
//! adapters into the crossterm types used by the existing TUI handlers.

use std::time::Duration;

use termina::{
    Event as TerminaEvent,
    escape::osc::{ColorOrQuery, DynamicColorNumber, Osc},
};

use super::theme::Rgb;

const INTER_BYTE_TIMEOUT: Duration = Duration::from_millis(40);
const MAX_PROTOCOL_BYTES: usize = 4096;

#[cfg(unix)]
mod imp {
    use std::{
        collections::VecDeque,
        fs::OpenOptions,
        io::{self, Read as _},
        os::{
            fd::{AsRawFd as _, OwnedFd},
            unix::net::UnixStream,
        },
        time::{Duration, Instant},
    };

    use mio::{Events, Interest, Poll, Token, unix::SourceFd};
    use termina::{Event as TerminaEvent, Parser};

    use super::{
        INTER_BYTE_TIMEOUT, MAX_PROTOCOL_BYTES, Rgb, background_from_event, query_bytes,
        to_crossterm_event,
    };

    const INPUT: Token = Token(0);
    const RESIZE: Token = Token(1);

    #[derive(Debug, Default)]
    struct FramedParser {
        parser: Parser,
        events: VecDeque<TerminaEvent>,
        pending_len: usize,
        pending_prefix: [u8; 2],
        prefix_len: usize,
        last_byte: Option<Instant>,
        discarding_osc: bool,
        discard_previous_esc: bool,
    }

    impl FramedParser {
        fn push(&mut self, bytes: &[u8], now: Instant) {
            for &byte in bytes {
                self.push_byte(byte, now);
            }
        }

        fn push_byte(&mut self, byte: u8, now: Instant) {
            if self.discarding_osc {
                self.last_byte = Some(now);
                let terminated = byte == b'\x07' || self.discard_previous_esc && byte == b'\\';
                self.discard_previous_esc = byte == b'\x1b';
                if terminated {
                    self.reset();
                }
                return;
            }

            if self.prefix_len < self.pending_prefix.len() {
                self.pending_prefix[self.prefix_len] = byte;
                self.prefix_len += 1;
            }
            self.pending_len += 1;
            self.last_byte = Some(now);
            self.parser.parse(&[byte], true);

            let mut emitted = false;
            while let Some(event) = self.parser.pop() {
                self.events.push_back(event);
                emitted = true;
            }
            if emitted {
                self.reset_tracking();
                return;
            }

            if self.pending_len >= MAX_PROTOCOL_BYTES {
                let osc = self.pending_is_osc();
                self.parser = Parser::default();
                self.reset_tracking();
                if osc {
                    self.discarding_osc = true;
                    self.last_byte = Some(now);
                }
            }
        }

        fn expire_if_needed(&mut self, now: Instant) {
            let Some(last_byte) = self.last_byte else {
                return;
            };
            if now.duration_since(last_byte) < INTER_BYTE_TIMEOUT {
                return;
            }
            if self.pending_is_osc() || self.discarding_osc {
                self.reset();
                return;
            }

            // Let the maintained parser resolve an ambiguous ordinary prefix
            // such as a lone Esc. Any still-incomplete/invalid sequence is
            // dropped as one unit rather than reinterpreted byte-by-byte.
            self.parser.parse(&[], false);
            while let Some(event) = self.parser.pop() {
                self.events.push_back(event);
            }
            self.reset();
        }

        fn drop_incomplete_osc(&mut self) {
            if self.pending_is_osc() || self.discarding_osc {
                self.reset();
            }
        }

        fn next_deadline(&self) -> Option<Instant> {
            self.last_byte.map(|last| last + INTER_BYTE_TIMEOUT)
        }

        fn pop(&mut self) -> Option<TerminaEvent> {
            self.events.pop_front()
        }

        fn pending_is_osc(&self) -> bool {
            self.prefix_len >= 2 && self.pending_prefix == [b'\x1b', b']']
        }

        fn reset_tracking(&mut self) {
            self.pending_len = 0;
            self.pending_prefix = [0; 2];
            self.prefix_len = 0;
            self.last_byte = None;
            self.discard_previous_esc = false;
        }

        fn reset(&mut self) {
            self.parser = Parser::default();
            self.discarding_osc = false;
            self.reset_tracking();
        }
    }

    pub(crate) struct TerminalInput {
        read: OwnedFd,
        write: OwnedFd,
        poll: Poll,
        poll_events: Events,
        resize_read: UnixStream,
        _resize_write: Option<UnixStream>,
        sigwinch_id: Option<signal_hook::SigId>,
        parser: FramedParser,
        deferred: VecDeque<TerminaEvent>,
    }

    impl TerminalInput {
        pub(crate) fn new() -> io::Result<Self> {
            let file = OpenOptions::new().read(true).write(true).open("/dev/tty")?;
            let read: OwnedFd = file.into();
            let write = read.try_clone()?;
            let (resize_read, resize_write) = UnixStream::pair()?;
            resize_read.set_nonblocking(true)?;
            let sigwinch_id = signal_hook::low_level::pipe::register(
                signal_hook::consts::SIGWINCH,
                resize_write,
            )?;
            Self::from_fds(read, write, resize_read, None, Some(sigwinch_id))
        }

        fn from_fds(
            read: OwnedFd,
            write: OwnedFd,
            resize_read: UnixStream,
            resize_write: Option<UnixStream>,
            sigwinch_id: Option<signal_hook::SigId>,
        ) -> io::Result<Self> {
            let poll = Poll::new()?;
            let mut input_source = SourceFd(&read.as_raw_fd());
            poll.registry()
                .register(&mut input_source, INPUT, Interest::READABLE)?;
            let mut resize_source = SourceFd(&resize_read.as_raw_fd());
            poll.registry()
                .register(&mut resize_source, RESIZE, Interest::READABLE)?;
            Ok(Self {
                read,
                write,
                poll,
                poll_events: Events::with_capacity(8),
                resize_read,
                _resize_write: resize_write,
                sigwinch_id,
                parser: FramedParser::default(),
                deferred: VecDeque::new(),
            })
        }

        #[cfg(test)]
        fn from_stream(stream: UnixStream) -> io::Result<Self> {
            let write_stream = stream.try_clone()?;
            let read: OwnedFd = stream.into();
            let write: OwnedFd = write_stream.into();
            let (resize_read, resize_write) = UnixStream::pair()?;
            resize_read.set_nonblocking(true)?;
            resize_write.set_nonblocking(true)?;
            Self::from_fds(read, write, resize_read, Some(resize_write), None)
        }

        pub(crate) fn query_background(&mut self, timeout: Duration) -> Option<Rgb> {
            if self.write_all(&query_bytes()).is_err() {
                return None;
            }
            let deadline = Instant::now() + timeout;
            let mut skipped = VecDeque::new();
            let result = loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break None;
                }
                match self.next_typed_event(remaining) {
                    Ok(Some(event)) => {
                        if let Some(background) = background_from_event(&event) {
                            break Some(background);
                        }
                        skipped.push_back(event);
                    }
                    Ok(None) | Err(_) => break None,
                }
            };
            self.parser.drop_incomplete_osc();
            self.deferred.append(&mut skipped);
            result
        }

        pub(crate) fn next_event(
            &mut self,
            timeout: Duration,
        ) -> io::Result<Option<crossterm::event::Event>> {
            let deadline = Instant::now() + timeout;
            loop {
                let event = if let Some(event) = self.deferred.pop_front() {
                    Some(event)
                } else {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Ok(None);
                    }
                    self.next_typed_event(remaining)?
                };
                let Some(event) = event else {
                    return Ok(None);
                };
                if let Some(event) = to_crossterm_event(event) {
                    return Ok(Some(event));
                }
            }
        }

        fn next_typed_event(&mut self, timeout: Duration) -> io::Result<Option<TerminaEvent>> {
            let deadline = Instant::now() + timeout;
            loop {
                let now = Instant::now();
                self.parser.expire_if_needed(now);
                if let Some(event) = self.parser.pop() {
                    return Ok(Some(event));
                }
                let remaining = deadline.saturating_duration_since(now);
                if remaining.is_zero() {
                    return Ok(None);
                }
                let wait = self
                    .parser
                    .next_deadline()
                    .map(|partial| partial.saturating_duration_since(now).min(remaining))
                    .unwrap_or(remaining);
                self.poll.poll(&mut self.poll_events, Some(wait))?;
                let mut input_ready = false;
                let mut resize_ready = false;
                for event in &self.poll_events {
                    match event.token() {
                        INPUT => input_ready = true,
                        RESIZE => resize_ready = true,
                        _ => {}
                    }
                }
                if input_ready {
                    let mut available = rustix::io::ioctl_fionread(&self.read)? as usize;
                    while available > 0 {
                        let mut bytes = [0u8; 256];
                        let requested = available.min(bytes.len());
                        match rustix::io::read(&self.read, &mut bytes[..requested]) {
                            Ok(read) if read > 0 => {
                                self.parser.push(&bytes[..read], Instant::now());
                                available = available.saturating_sub(read);
                            }
                            Ok(_) => break,
                            Err(error) if error == rustix::io::Errno::AGAIN => break,
                            Err(error) => {
                                return Err(io::Error::from_raw_os_error(error.raw_os_error()));
                            }
                        }
                    }
                }
                if resize_ready {
                    let mut drain = [0u8; 64];
                    while self.resize_read.read(&mut drain).is_ok() {}
                    if let Ok(size) = rustix::termios::tcgetwinsize(&self.write) {
                        return Ok(Some(TerminaEvent::WindowResized(size.into())));
                    }
                }
            }
        }

        fn write_all(&self, mut bytes: &[u8]) -> io::Result<()> {
            while !bytes.is_empty() {
                match rustix::io::write(&self.write, bytes) {
                    Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                    Ok(written) => bytes = &bytes[written..],
                    Err(error) if error == rustix::io::Errno::INTR => continue,
                    Err(error) => return Err(io::Error::from_raw_os_error(error.raw_os_error())),
                }
            }
            Ok(())
        }
    }

    impl Drop for TerminalInput {
        fn drop(&mut self) {
            if let Some(id) = self.sigwinch_id.take() {
                signal_hook::low_level::unregister(id);
            }
        }
    }

    #[cfg(test)]
    mod fd_tests {
        use std::{io::Write as _, thread};

        use crossterm::event::{Event, KeyCode};

        use super::*;

        fn pair() -> (TerminalInput, UnixStream) {
            let (application, peer) = UnixStream::pair().unwrap();
            (TerminalInput::from_stream(application).unwrap(), peer)
        }

        #[test]
        fn actual_fd_lone_escape_waits_for_inter_byte_deadline() {
            let (mut input, mut peer) = pair();
            peer.write_all(b"\x1b").unwrap();
            let event = input.next_event(Duration::from_millis(200)).unwrap();
            assert!(matches!(event, Some(Event::Key(key)) if key.code == KeyCode::Esc));
        }

        #[test]
        fn actual_fd_short_read_after_escape_keeps_sequence_intact() {
            let (mut input, mut peer) = pair();
            peer.write_all(b"\x1b").unwrap();
            thread::sleep(Duration::from_millis(5));
            peer.write_all(b"[A").unwrap();
            let event = input.next_event(Duration::from_millis(200)).unwrap();
            assert!(matches!(event, Some(Event::Key(key)) if key.code == KeyCode::Up));
        }

        #[test]
        fn actual_fd_preserves_split_osc_boundaries_and_following_key() {
            let response = b"\x1b]11;rgb:ffff/0000/0000\x1b\\";
            for split in [1, 2, 8, response.len() - 1] {
                let (mut input, mut peer) = pair();
                peer.write_all(&response[..split]).unwrap();
                thread::sleep(Duration::from_millis(5));
                peer.write_all(&response[split..]).unwrap();
                peer.write_all(b"q").unwrap();
                let event = input.next_event(Duration::from_millis(200)).unwrap();
                assert!(matches!(event, Some(Event::Key(key)) if key.code == KeyCode::Char('q')));
            }
        }

        #[test]
        fn query_timeout_drops_unterminated_osc_before_later_key() {
            let (mut input, mut peer) = pair();
            let worker = thread::spawn(move || {
                let mut query = [0u8; 64];
                let _ = peer.read(&mut query).unwrap();
                peer.write_all(b"\x1b]11;rgb:ffff/").unwrap();
                thread::sleep(Duration::from_millis(60));
                peer.write_all(b"q").unwrap();
            });
            assert_eq!(input.query_background(Duration::from_millis(30)), None);
            let event = input.next_event(Duration::from_millis(200)).unwrap();
            assert!(
                matches!(event, Some(Event::Key(key)) if key.code == KeyCode::Char('q')),
                "{event:?}"
            );
            worker.join().unwrap();
        }

        #[test]
        fn actual_fd_bounds_and_discards_oversized_osc_frame() {
            let (mut input, mut peer) = pair();
            peer.write_all(b"\x1b]11;").unwrap();
            peer.write_all(&vec![b'a'; MAX_PROTOCOL_BYTES + 32])
                .unwrap();
            peer.write_all(b"\x07q").unwrap();
            let event = input.next_event(Duration::from_millis(300)).unwrap();
            assert!(
                matches!(event, Some(Event::Key(key)) if key.code == KeyCode::Char('q')),
                "{event:?}"
            );
        }

        #[test]
        fn actual_fd_dispatches_focus_mouse_and_paste() {
            let (mut input, mut peer) = pair();
            peer.write_all(b"\x1b[I\x1b[<0;3;4M\x1b[200~hello\x1b[201~")
                .unwrap();
            assert!(matches!(
                input.next_event(Duration::from_millis(200)).unwrap(),
                Some(Event::FocusGained)
            ));
            assert!(matches!(
                input.next_event(Duration::from_millis(200)).unwrap(),
                Some(Event::Mouse(_))
            ));
            assert!(matches!(
                input.next_event(Duration::from_millis(200)).unwrap(),
                Some(Event::Paste(text)) if text == "hello"
            ));
        }
    }
}

#[cfg(unix)]
pub(super) use imp::TerminalInput;

#[cfg(not(unix))]
mod imp {
    use std::{io, time::Duration};

    use termina::{PlatformTerminal, Terminal as _};

    use super::{Rgb, background_from_event, query_bytes, to_crossterm_event};

    pub(crate) struct TerminalInput {
        terminal: PlatformTerminal,
    }

    impl TerminalInput {
        pub(crate) fn new() -> io::Result<Self> {
            Ok(Self {
                terminal: PlatformTerminal::new()?,
            })
        }

        pub(crate) fn query_background(&mut self, timeout: Duration) -> Option<Rgb> {
            use std::io::Write as _;

            self.terminal.write_all(query_bytes()).ok()?;
            self.terminal.flush().ok()?;
            self.terminal
                .poll(
                    |event| background_from_event(event).is_some(),
                    Some(timeout),
                )
                .ok()?
                .then(|| {
                    self.terminal
                        .read(|event| background_from_event(event).is_some())
                        .ok()
                        .and_then(|event| background_from_event(&event))
                })
                .flatten()
        }

        pub(crate) fn next_event(
            &mut self,
            timeout: Duration,
        ) -> io::Result<Option<crossterm::event::Event>> {
            let deadline = std::time::Instant::now() + timeout;
            loop {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if !self.terminal.poll(|_| true, Some(remaining))? {
                    return Ok(None);
                }
                if let Some(event) = to_crossterm_event(self.terminal.read(|_| true)?) {
                    return Ok(Some(event));
                }
            }
        }
    }
}

#[cfg(not(unix))]
pub(super) use imp::TerminalInput;

fn query_bytes() -> Vec<u8> {
    Osc::ChangeDynamicColors(
        DynamicColorNumber::TextBackgroundColor,
        vec![ColorOrQuery::Query],
    )
    .to_string()
    .into_bytes()
}

fn background_from_event(event: &TerminaEvent) -> Option<Rgb> {
    let TerminaEvent::Osc(Osc::ChangeDynamicColors(
        DynamicColorNumber::TextBackgroundColor,
        colors,
    )) = event
    else {
        return None;
    };
    colors.iter().find_map(|color| match color {
        ColorOrQuery::Color(color) => Some(Rgb::new(color.red, color.green, color.blue)),
        ColorOrQuery::Query => None,
    })
}

fn to_crossterm_event(event: TerminaEvent) -> Option<crossterm::event::Event> {
    if event.is_escape() {
        return None;
    }
    let event = terminput_termina::to_terminput(event).ok()?;
    terminput_crossterm::to_crossterm(event).ok()
}

#[cfg(test)]
mod parser_tests {
    use termina::{Event as TerminaEvent, Parser};

    use super::*;

    fn parse_chunks(chunks: &[&[u8]]) -> Vec<TerminaEvent> {
        let mut parser = Parser::default();
        let mut events = Vec::new();
        for chunk in chunks {
            parser.parse(chunk, true);
            while let Some(event) = parser.pop() {
                events.push(event);
            }
        }
        parser.parse(&[], false);
        while let Some(event) = parser.pop() {
            events.push(event);
        }
        events
    }

    #[test]
    fn parser_handles_bel_and_st_split_at_every_boundary() {
        for response in [
            b"\x1b]11;rgb:ffff/8000/0000\x07".as_slice(),
            b"\x1b]11;rgb:ffff/8000/0000\x1b\\".as_slice(),
        ] {
            for split in 0..=response.len() {
                let events = parse_chunks(&[&response[..split], &response[split..]]);
                assert_eq!(events.len(), 1, "split {split} for {response:?}");
                assert_eq!(
                    background_from_event(&events[0]),
                    Some(Rgb::new(255, 127, 0)),
                    "split {split} for {response:?}"
                );
            }
        }
    }

    #[test]
    fn malformed_then_valid_and_multiple_frames_find_valid_colors() {
        let events = parse_chunks(&[
            b"\x1b]11;not-a-color\x07",
            b"\x1b]11;rgb:1111/2222/3333\x07\x1b]11;rgb:aaaa/bbbb/cccc\x1b\\",
        ]);
        assert_eq!(events.len(), 2, "malformed OSC must not become key input");
        let colors: Vec<_> = events.iter().filter_map(background_from_event).collect();
        assert_eq!(
            colors,
            [Rgb::new(0x11, 0x22, 0x33), Rgb::new(0xaa, 0xbb, 0xcc)]
        );
    }
}
