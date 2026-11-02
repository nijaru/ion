//! Bounded live terminal state for PTY-backed programs.
//!
//! Raw output remains the authority in [`super::ring`]. This module only adds a
//! derived screen projection: a bounded control-string guard in front of vt100,
//! automatic protocol replies, paged visible-line reads and ordered resize.
//! Unsupported or unsafe terminal input makes the projection unavailable; it
//! never falls back to replaying a tail of the raw ring.
use std::{
    collections::VecDeque,
    sync::{Mutex, MutexGuard},
};

use vt100::{Callbacks, MouseProtocolEncoding, MouseProtocolMode, Parser, Screen};

pub(super) const DEFAULT_ROWS: u16 = 24;
pub(super) const DEFAULT_COLUMNS: u16 = 80;
pub(super) const MAX_ROWS: u16 = 200;
pub(super) const MAX_COLUMNS: u16 = 400;
pub(super) const MAX_CELLS: u32 = 20_000;
pub(super) const MAX_SCREEN_PAGE_ROWS: u16 = 64;
pub(super) const MAX_SCREEN_TEXT_BYTES: usize = 64 * 1024;

/// Maximum bytes retained while scanning one terminal control string. vte's
/// std configuration accumulates OSC payload in an unbounded `Vec`, so the
/// whole chunk is rejected before it reaches vt100 once this bound is crossed.
const MAX_CONTROL_STRING_BYTES: usize = 4 * 1024;
/// Maximum reply bytes a single parse batch may request. A terminal that
/// exceeds this is not silently starved of required protocol replies.
const MAX_REPLY_BATCH_BYTES: usize = 4 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Dimensions {
    pub rows: u16,
    pub columns: u16,
}

impl Dimensions {
    pub(super) fn new(rows: u16, columns: u16) -> Result<Self, String> {
        if !(1..=MAX_ROWS).contains(&rows) {
            return Err(format!("terminal rows must be 1..={MAX_ROWS}"));
        }
        if !(1..=MAX_COLUMNS).contains(&columns) {
            return Err(format!("terminal columns must be 1..={MAX_COLUMNS}"));
        }
        let cells = u32::from(rows) * u32::from(columns);
        if cells > MAX_CELLS {
            return Err(format!(
                "terminal dimensions contain {cells} cells; maximum is {MAX_CELLS}"
            ));
        }
        Ok(Self { rows, columns })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum GuardState {
    #[default]
    Ground,
    Escape,
    EscapeIntermediate,
    Csi,
    Osc,
    DcsEntry,
    DcsIntermediate,
    DcsParam,
    DcsIgnore,
    DcsPassthrough,
    OtherString,
}

/// Streaming control-string framing guard. It mirrors only enough of vte's
/// state machine to know whether a chunk would extend an OSC/DCS/SOS/PM/APC
/// string beyond the bound. It is not a screen parser and does not interpret
/// terminal content.
#[derive(Default)]
struct ControlStringGuard {
    state: GuardState,
    string_bytes: usize,
}

impl ControlStringGuard {
    fn scan(&mut self, bytes: &[u8]) -> Result<(), String> {
        for byte in bytes {
            if !matches!(
                self.state,
                GuardState::Ground
                    | GuardState::Escape
                    | GuardState::EscapeIntermediate
                    | GuardState::Csi
            ) {
                self.string_bytes = self.string_bytes.saturating_add(1);
                if self.string_bytes > MAX_CONTROL_STRING_BYTES {
                    return Err(format!(
                        "terminal control string exceeded {MAX_CONTROL_STRING_BYTES} bytes; \
                         screen projection unavailable"
                    ));
                }
            }
            match self.state {
                GuardState::Ground => {
                    if *byte == 0x1b {
                        self.state = GuardState::Escape;
                    }
                }
                GuardState::Escape => match *byte {
                    0x1b => self.state = GuardState::Escape,
                    0x5b => self.state = GuardState::Csi,
                    0x5d => {
                        self.state = GuardState::Osc;
                        self.string_bytes = 1;
                    }
                    0x50 => {
                        self.state = GuardState::DcsEntry;
                        self.string_bytes = 1;
                    }
                    0x58 | 0x5e | 0x5f => {
                        self.state = GuardState::OtherString;
                        self.string_bytes = 1;
                    }
                    0x18 | 0x1a => self.state = GuardState::Ground,
                    0x20..=0x2f => self.state = GuardState::EscapeIntermediate,
                    0x30..=0x7e => self.state = GuardState::Ground,
                    // vte executes C0 and ignores DEL/high bytes without
                    // leaving Escape. Losing that state admits an unbounded
                    // OSC after e.g. ESC NUL ']'.
                    _ => {}
                },
                GuardState::EscapeIntermediate => match *byte {
                    0x1b => self.state = GuardState::Escape,
                    0x18 | 0x1a | 0x30..=0x7e => self.state = GuardState::Ground,
                    _ => {}
                },
                GuardState::Csi => match *byte {
                    0x1b => self.state = GuardState::Escape,
                    0x18 | 0x1a => self.state = GuardState::Ground,
                    0x40..=0x7e => self.state = GuardState::Ground,
                    _ => {}
                },
                GuardState::Osc => match *byte {
                    0x07 | 0x18 | 0x1a => self.state = GuardState::Ground,
                    0x1b => self.state = GuardState::Escape,
                    _ => {}
                },
                GuardState::DcsEntry => match *byte {
                    0x1b => self.state = GuardState::Escape,
                    0x18 | 0x1a => self.state = GuardState::Ground,
                    0x20..=0x2f => self.state = GuardState::DcsIntermediate,
                    0x30..=0x3f => self.state = GuardState::DcsParam,
                    0x40..=0x7e => self.state = GuardState::DcsPassthrough,
                    _ => {}
                },
                GuardState::DcsIntermediate => match *byte {
                    0x1b => self.state = GuardState::Escape,
                    0x18 | 0x1a => self.state = GuardState::Ground,
                    0x20..=0x2f => {}
                    0x30..=0x3f => self.state = GuardState::DcsIgnore,
                    0x40..=0x7e => self.state = GuardState::DcsPassthrough,
                    _ => {}
                },
                GuardState::DcsParam => match *byte {
                    0x1b => self.state = GuardState::Escape,
                    0x18 | 0x1a => self.state = GuardState::Ground,
                    0x20..=0x2f => self.state = GuardState::DcsIntermediate,
                    0x30..=0x3b => {}
                    0x3c..=0x3f => self.state = GuardState::DcsIgnore,
                    0x40..=0x7e => self.state = GuardState::DcsPassthrough,
                    _ => {}
                },
                GuardState::DcsIgnore => match *byte {
                    0x1b => self.state = GuardState::Escape,
                    0x18 | 0x1a => self.state = GuardState::Ground,
                    _ => {}
                },
                GuardState::DcsPassthrough => match *byte {
                    0x1b => self.state = GuardState::Escape,
                    0x18 | 0x1a | 0x9c => self.state = GuardState::Ground,
                    _ => {}
                },
                GuardState::OtherString => match *byte {
                    0x1b => self.state = GuardState::Escape,
                    0x18 | 0x1a => self.state = GuardState::Ground,
                    _ => {}
                },
            }
        }
        Ok(())
    }
}

/// Replies are bounded per parse chunk. Overflow is a terminal-protocol
/// failure: the caller must stop rather than silently discard required bytes.
#[derive(Default)]
struct ReplySink {
    enabled: bool,
    bytes: VecDeque<u8>,
    overflow: bool,
}

impl ReplySink {
    fn push(&mut self, reply: &[u8]) {
        if !self.enabled || self.overflow {
            return;
        }
        if self.bytes.len().saturating_add(reply.len()) > MAX_REPLY_BATCH_BYTES {
            self.overflow = true;
            self.bytes.clear();
            return;
        }
        self.bytes.extend(reply);
    }

    fn take(&mut self) -> Result<VecDeque<u8>, String> {
        if self.overflow {
            self.bytes.clear();
            return Err(format!(
                "terminal protocol replies exceeded {MAX_REPLY_BATCH_BYTES} bytes"
            ));
        }
        Ok(std::mem::take(&mut self.bytes))
    }
}

impl Callbacks for ReplySink {
    fn unhandled_csi(
        &mut self,
        screen: &mut Screen,
        i1: Option<u8>,
        _i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        let op = params.first().and_then(|param| param.first().copied());
        match (i1, c, op) {
            (None, 'n', Some(5)) => self.push(b"\x1b[0n"),
            (None, 'n', Some(6)) => {
                let (row, column) = screen.cursor_position();
                self.push(format!("\x1b[{};{}R", row + 1, column + 1).as_bytes());
            }
            (Some(b'?'), 'n', Some(6)) => {
                let (row, column) = screen.cursor_position();
                self.push(format!("\x1b[?{};{};1R", row + 1, column + 1).as_bytes());
            }
            (Some(b'?'), 'n', Some(15)) => self.push(b"\x1b[?13n"),
            (None, 'c', None | Some(0)) => self.push(b"\x1b[?1;2c"),
            (Some(b'>'), 'c', None | Some(0)) => self.push(b"\x1b[>0;10;1c"),
            (None, 't', Some(18)) => {
                let (rows, columns) = screen.size();
                self.push(format!("\x1b[8;{rows};{columns}t").as_bytes());
            }
            _ => {}
        }
    }
}

struct TerminalState {
    parser: Option<Parser<ReplySink>>,
    unavailable: Option<String>,
    dimensions: Dimensions,
    source_bytes: u64,
    revision: u64,
    guard: ControlStringGuard,
}

/// Shared terminal projection. The master descriptor is never stored here.
pub(super) struct Terminal {
    state: Mutex<TerminalState>,
}

#[derive(Debug)]
pub(super) struct TerminalMeta {
    pub revision: u64,
    pub source_bytes: u64,
    pub rows: u16,
    pub columns: u16,
    pub cursor_row: u16,
    pub cursor_column: u16,
    pub cursor_visible: bool,
    pub alternate_screen: bool,
    pub application_cursor: bool,
    pub application_keypad: bool,
    pub bracketed_paste: bool,
    pub mouse_protocol_mode: &'static str,
    pub mouse_protocol_encoding: &'static str,
}

#[derive(Debug)]
pub(super) struct TerminalError {
    pub reason: String,
    pub fatal: bool,
}

pub(super) struct ScreenPage {
    pub text: String,
    pub start_row: u16,
    pub next_row: u16,
    pub visible_rows: u16,
    pub meta: TerminalMeta,
}

impl Terminal {
    pub(super) fn new(dimensions: Dimensions) -> Self {
        Self {
            state: Mutex::new(TerminalState {
                parser: Some(Parser::new_with_callbacks(
                    dimensions.rows,
                    dimensions.columns,
                    0,
                    ReplySink::default(),
                )),
                unavailable: None,
                dimensions,
                source_bytes: 0,
                revision: 0,
                guard: ControlStringGuard::default(),
            }),
        }
    }

    /// Feed one raw output chunk only after the whole chunk has passed the
    /// control-string bound. Returns automatic replies to be written ahead of
    /// model input. On a bound violation the parser is dropped and the screen
    /// stays unavailable; raw capture continues independently.
    pub(super) fn process(
        &self,
        bytes: &[u8],
        replies_enabled: bool,
    ) -> Result<VecDeque<u8>, TerminalError> {
        let mut state = self.lock();
        if state.parser.is_none() {
            return Ok(VecDeque::new());
        }
        if let Err(reason) = state.guard.scan(bytes) {
            state.parser = None;
            state.unavailable = Some(reason.clone());
            return Err(TerminalError {
                reason,
                fatal: false,
            });
        }
        let replies = {
            let parser = state.parser.as_mut().expect("checked above");
            // Output drain continues after input quiesces. Parse its screen
            // effects without attempting replies to an exited/quiesced peer.
            parser.callbacks_mut().enabled = replies_enabled;
            parser.process(bytes);
            parser.callbacks_mut().take()
        };
        state.source_bytes = state.source_bytes.saturating_add(bytes.len() as u64);
        state.revision = state.revision.saturating_add(1);
        match replies {
            Ok(replies) => Ok(replies),
            Err(reason) => {
                state.parser = None;
                state.unavailable = Some(reason.clone());
                Err(TerminalError {
                    reason,
                    fatal: true,
                })
            }
        }
    }

    pub(super) fn dimensions(&self) -> Dimensions {
        self.lock().dimensions
    }

    pub(super) fn meta(&self) -> Result<TerminalMeta, String> {
        let state = self.lock();
        let parser = state.parser.as_ref().ok_or_else(|| {
            format!(
                "screen projection unavailable: {}",
                state
                    .unavailable
                    .as_deref()
                    .unwrap_or("live parser is not available")
            )
        })?;
        Ok(meta(parser.screen(), &state))
    }

    pub(super) fn page(&self, start_row: u16, page_rows: u16) -> Result<ScreenPage, String> {
        if page_rows == 0 || page_rows > MAX_SCREEN_PAGE_ROWS {
            return Err(format!("screen rows must be 1..={MAX_SCREEN_PAGE_ROWS}"));
        }
        let state = self.lock();
        let parser = state.parser.as_ref().ok_or_else(|| {
            format!(
                "screen projection unavailable: {}",
                state
                    .unavailable
                    .as_deref()
                    .unwrap_or("live parser is not available")
            )
        })?;
        let screen = parser.screen();
        let columns = state.dimensions.columns;
        let mut text = String::new();
        let mut visible_rows = 0u16;
        for line in screen.rows(0, columns).skip(usize::from(start_row)) {
            if visible_rows >= page_rows {
                break;
            }
            let separator = usize::from(visible_rows > 0);
            if text
                .len()
                .saturating_add(separator)
                .saturating_add(line.len())
                > MAX_SCREEN_TEXT_BYTES
            {
                if visible_rows == 0 {
                    return Err(format!(
                        "terminal row exceeds the {MAX_SCREEN_TEXT_BYTES}-byte projection page"
                    ));
                }
                break;
            }
            if visible_rows > 0 {
                text.push('\n');
            }
            text.push_str(&line);
            visible_rows += 1;
        }
        Ok(ScreenPage {
            text,
            start_row,
            next_row: start_row.saturating_add(visible_rows),
            visible_rows,
            meta: meta(screen, &state),
        })
    }

    /// Set the OS window size and projection size as one ordered operation.
    /// The projection lock prevents output parsing between the successful ioctl
    /// and the parser resize; a failed ioctl publishes no new dimensions.
    pub(super) fn resize(
        &self,
        tty: impl std::os::fd::AsFd,
        dimensions: Dimensions,
    ) -> Result<(), String> {
        let mut state = self.lock();
        if state.parser.is_none() {
            return Err(format!(
                "screen projection unavailable: {}",
                state
                    .unavailable
                    .as_deref()
                    .unwrap_or("live parser is not available")
            ));
        }
        rustix::termios::tcsetwinsize(
            tty,
            rustix::termios::Winsize {
                ws_row: dimensions.rows,
                ws_col: dimensions.columns,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )
        .map_err(|error| format!("terminal resize failed: {error}"))?;
        let parser = state.parser.as_mut().expect("checked above");
        parser
            .screen_mut()
            .set_size(dimensions.rows, dimensions.columns);
        state.dimensions = dimensions;
        state.revision = state.revision.saturating_add(1);
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, TerminalState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

fn meta(screen: &Screen, state: &TerminalState) -> TerminalMeta {
    let (cursor_row, cursor_column) = screen.cursor_position();
    TerminalMeta {
        revision: state.revision,
        source_bytes: state.source_bytes,
        rows: state.dimensions.rows,
        columns: state.dimensions.columns,
        cursor_row,
        cursor_column,
        cursor_visible: !screen.hide_cursor(),
        alternate_screen: screen.alternate_screen(),
        application_cursor: screen.application_cursor(),
        application_keypad: screen.application_keypad(),
        bracketed_paste: screen.bracketed_paste(),
        mouse_protocol_mode: mouse_mode(screen.mouse_protocol_mode()),
        mouse_protocol_encoding: mouse_encoding(screen.mouse_protocol_encoding()),
    }
}

fn mouse_mode(mode: MouseProtocolMode) -> &'static str {
    match mode {
        MouseProtocolMode::None => "none",
        MouseProtocolMode::Press => "press",
        MouseProtocolMode::PressRelease => "press_release",
        MouseProtocolMode::ButtonMotion => "button_motion",
        MouseProtocolMode::AnyMotion => "any_motion",
    }
}

fn mouse_encoding(encoding: MouseProtocolEncoding) -> &'static str {
    match encoding {
        MouseProtocolEncoding::Default => "default",
        MouseProtocolEncoding::Utf8 => "utf8",
        MouseProtocolEncoding::Sgr => "sgr",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimensions_are_validated_before_terminal_allocation() {
        assert_eq!(
            Dimensions::new(24, 80).unwrap(),
            Dimensions {
                rows: 24,
                columns: 80
            }
        );
        assert!(Dimensions::new(0, 80).is_err());
        assert!(Dimensions::new(201, 80).is_err());
        assert!(Dimensions::new(24, 401).is_err());
        assert!(Dimensions::new(200, 200).is_err());
    }

    #[test]
    fn oversized_control_strings_are_rejected_before_vt100_and_stay_unavailable() {
        for (start, terminate) in [
            (b"\x1b]0;".as_slice(), b"\x07".as_slice()),
            (b"\x1bP".as_slice(), b"\x1b\\".as_slice()),
            (b"\x1b_".as_slice(), b"\x1b\\".as_slice()),
        ] {
            let terminal = Terminal::new(Dimensions::new(DEFAULT_ROWS, DEFAULT_COLUMNS).unwrap());
            terminal.process(b"BEFORE\x1b[31mRED\x1b[0m", true).unwrap();
            let mut payload = start.to_vec();
            payload.extend(std::iter::repeat_n(b'x', MAX_CONTROL_STRING_BYTES + 1));
            payload.extend_from_slice(terminate);
            assert!(terminal.process(&payload, true).is_err());
            assert!(terminal.meta().is_err());
            assert!(terminal.process(b"AFTER", true).unwrap().is_empty());
            assert!(terminal.meta().unwrap_err().contains("unavailable"));
            assert!(terminal.page(0, 24).is_err());
        }
    }

    #[test]
    fn escape_padding_cannot_bypass_the_osc_bound() {
        #[derive(Default)]
        struct ObservedOsc(bool);
        impl Callbacks for ObservedOsc {
            fn set_window_title(&mut self, _: &mut Screen, title: &[u8]) {
                self.0 |= title.len() > MAX_CONTROL_STRING_BYTES;
            }
        }
        // The library is an independent framing oracle. Exercise every byte
        // between ESC and ']', including ignored C0/DEL/high bytes and escape
        // intermediates; reset the parser between cases, never its chunks.
        for padding in 0..=u8::MAX {
            let prefix = [0x1b, padding, b']', b'2', b';'];
            let mut bytes = prefix.to_vec();
            bytes.extend(std::iter::repeat_n(b'x', MAX_CONTROL_STRING_BYTES + 1));
            bytes.push(7);
            let mut oracle = Parser::new_with_callbacks(2, 10, 0, ObservedOsc::default());
            oracle.process(&bytes);
            if oracle.callbacks().0 {
                let terminal = Terminal::new(Dimensions::new(2, 10).unwrap());
                // Preserve the escape state across a feed boundary, too.
                terminal.process(&prefix[..1], true).unwrap();
                let result = terminal.process(&bytes[1..], true);
                assert!(
                    result.is_err(),
                    "OSC bound bypassed with padding {padding:#x}"
                );
                assert!(terminal.meta().is_err());
            }
        }
    }

    #[test]
    fn split_control_strings_share_one_bound_across_chunks() {
        let terminal = Terminal::new(Dimensions::new(DEFAULT_ROWS, DEFAULT_COLUMNS).unwrap());
        terminal.process(b"\x1b]0;", true).unwrap();
        for _ in 0..3 {
            terminal
                .process(&vec![b'x'; MAX_CONTROL_STRING_BYTES / 4], true)
                .unwrap();
        }
        assert!(
            terminal
                .process(&vec![b'x'; MAX_CONTROL_STRING_BYTES / 4], true)
                .is_err()
        );
        assert!(terminal.meta().is_err());
    }

    #[test]
    fn csi_framing_does_not_treat_embedded_osc_like_bytes_as_a_string() {
        let guard = ControlStringGuard::default();
        let mut guard = guard;
        guard.scan(b"\x1b[31m").unwrap();
        assert_eq!(guard.state, GuardState::Ground);
        guard.scan(b"\x1b[?1049h").unwrap();
        assert_eq!(guard.state, GuardState::Ground);
    }

    #[test]
    fn eight_bit_c1_stays_aligned_with_vte_seven_bit_framing() {
        let mut guard = ControlStringGuard::default();
        // vte treats 0x9d as an unhandled control byte, not an OSC introducer.
        guard.scan(b"\x9d0;not-a-title\x07").unwrap();
        assert_eq!(guard.state, GuardState::Ground);
        // DCS passthrough accepts the 8-bit ST form.
        guard.scan(b"\x1bPq").unwrap();
        assert_eq!(guard.state, GuardState::DcsPassthrough);
        guard.scan(b"\x9c").unwrap();
        assert_eq!(guard.state, GuardState::Ground);
    }

    #[test]
    fn cursor_and_device_status_queries_produce_bounded_replies() {
        let terminal = Terminal::new(Dimensions::new(DEFAULT_ROWS, DEFAULT_COLUMNS).unwrap());
        let replies = terminal
            .process(b"\x1b[4;7H\x1b[6n\x1b[5n\x1b[c\x1b[>c", true)
            .unwrap();
        assert_eq!(
            Vec::from(replies),
            b"\x1b[4;7R\x1b[0n\x1b[?1;2c\x1b[>0;10;1c".to_vec()
        );
    }

    #[test]
    fn quiesced_input_still_projects_output_without_generating_replies() {
        let terminal = Terminal::new(Dimensions::new(24, 80).unwrap());
        let output = [b"DONE".as_slice(), b"\x1b[6n".repeat(2_000).as_slice()].concat();
        assert!(terminal.process(&output, false).unwrap().is_empty());
        assert_eq!(terminal.page(0, 1).unwrap().text, "DONE");
        assert_eq!(terminal.meta().unwrap().source_bytes, output.len() as u64);
    }

    #[test]
    fn failed_os_resize_does_not_publish_new_dimensions() {
        let terminal = Terminal::new(Dimensions::new(24, 80).unwrap());
        let not_a_terminal = std::fs::File::open("/dev/null").unwrap();
        let before = terminal.meta().unwrap();
        assert!(
            terminal
                .resize(&not_a_terminal, Dimensions::new(12, 40).unwrap())
                .is_err()
        );
        let after = terminal.meta().unwrap();
        assert_eq!((after.rows, after.columns), (24, 80));
        assert_eq!(after.revision, before.revision);
    }

    #[test]
    fn screen_projection_is_bounded_and_paged() {
        let terminal = Terminal::new(Dimensions::new(4, 12).unwrap());
        terminal
            .process(b"one\r\ntwo\r\nthree\r\nfour", true)
            .unwrap();
        let first = terminal.page(1, 2).unwrap();
        assert_eq!(first.text, "two\nthree");
        assert_eq!(first.start_row, 1);
        assert_eq!(first.next_row, 3);
        assert_eq!(first.visible_rows, 2);
        let tail = terminal.page(3, 10).unwrap();
        assert_eq!(tail.text, "four");
        assert_eq!(tail.next_row, 4);
        assert_eq!(tail.visible_rows, 1);
        let beyond = terminal.page(8, 10).unwrap();
        assert_eq!(beyond.text, "");
        assert_eq!(beyond.next_row, 8);
    }
}
