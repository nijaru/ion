//! Frame terminal replies and enhanced key metadata without losing type-ahead.
use std::time::Duration;

use tokio::time::Instant;

use crate::colors::observe_osc;
use crate::{Appearance, InputError, Modifiers, TerminalColors};

const MAX_OSC_BYTES: usize = 1024;
const ALL_COLORS: u32 = (1 << 18) - 1;
// Reply framing is not Escape-key disambiguation. Allow one probe budget to
// collect a response, then one more to drain an abandoned suffix. Neither
// deadline is renewed by traffic or cancelled next()/query futures.
const REPLY_TIMEOUT: Duration = Duration::from_millis(100);
const RECOVERY_TIMEOUT: Duration = Duration::from_millis(100);
const MAX_RECOVERY_BYTES: usize = 8192;

#[derive(Clone, Copy, Debug)]
enum ReplyWindow {
    Collecting {
        deadline: Instant,
        drain_until: Instant,
    },
    Draining {
        deadline: Instant,
        remaining: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModeCapture {
    Ready { notify: bool },
    Waiting { retry: bool },
    Abandoned { retry: bool },
    Settled,
}

/// A DECRQM capture has no request identity. Keep each mode's outstanding
/// request separate, including across cancellation and terminal reacquisition.
#[derive(Debug)]
pub(super) struct ModeProbe {
    pub(super) value: Option<u8>,
    capture: ModeCapture,
}

impl Default for ModeProbe {
    fn default() -> Self {
        Self {
            value: None,
            capture: ModeCapture::Ready { notify: false },
        }
    }
}

impl ModeProbe {
    pub(super) fn begin(&mut self) -> bool {
        self.value = None;
        self.end();
        if let ModeCapture::Ready { notify } = self.capture {
            self.capture = ModeCapture::Waiting { retry: notify };
            true
        } else {
            false
        }
    }

    pub(super) fn end(&mut self) {
        if let ModeCapture::Waiting { retry } = self.capture {
            self.capture = ModeCapture::Abandoned { retry };
        }
    }

    fn reacquire(&mut self) {
        self.value = None;
        self.capture = match self.capture {
            ModeCapture::Waiting { .. } | ModeCapture::Abandoned { .. } => {
                ModeCapture::Abandoned { retry: false }
            }
            _ => ModeCapture::Ready { notify: false },
        };
    }

    fn retry_ready(&self) -> bool {
        self.capture == ModeCapture::Ready { notify: true }
    }

    fn observe(&mut self, value: Option<u8>) {
        self.capture = match self.capture {
            ModeCapture::Waiting { .. } => {
                self.value = value;
                ModeCapture::Settled
            }
            // Drain old reports without granting authority. Allow one fresh
            // capture, not an endless probe loop on persistently slow terminals.
            ModeCapture::Abandoned { retry: false } if value.is_some_and(|mode| mode != 0) => {
                ModeCapture::Ready { notify: true }
            }
            ModeCapture::Abandoned { .. } => ModeCapture::Settled,
            state => state,
        };
    }
}

#[derive(Debug)]
pub(super) enum KeyFrame {
    Bytes(Vec<u8>),
    Boundary,
    Key {
        bytes: Vec<u8>,
        modifiers: Modifiers,
        repeat: bool,
    },
    ColorsChanged,
    Rejected(InputError),
}

#[derive(Debug)]
pub(super) struct TerminalReplyFilter {
    state: State,
    reply_window: Option<ReplyWindow>,
    pub(super) keyboard: Option<bool>,
    pub(super) cursor: Option<(u16, u16)>,
    pub(super) appearance_mode: ModeProbe,
    pub(super) grapheme_mode: ModeProbe,
    pub(super) colors: TerminalColors,
    colors_need_refresh: bool,
    received_colors: u32,
    accept_colors: bool,
    probe_interrupted: bool,
}

impl Default for TerminalReplyFilter {
    fn default() -> Self {
        Self {
            state: State::default(),
            reply_window: None,
            keyboard: None,
            cursor: None,
            appearance_mode: ModeProbe::default(),
            grapheme_mode: ModeProbe::default(),
            colors: TerminalColors::default(),
            colors_need_refresh: true,
            received_colors: 0,
            accept_colors: true,
            probe_interrupted: false,
        }
    }
}

#[derive(Debug, Default)]
enum State {
    #[default]
    Ground,
    Esc,
    Csi,
    Body {
        private: bool,
        bytes: Vec<u8>,
    },
    // ESC ] is also legacy Alt+]. Do not claim a control string until a
    // numeric OSC command and its semicolon have actually arrived.
    OscPrefix {
        command: Vec<u8>,
    },
    Osc {
        bytes: Vec<u8>,
    },
    // Never replay a committed OSC payload as draft text, even after timeout.
    // C0 controls and a new ESC resynchronize input instead of being swallowed.
    OscDiscard,
    OscEscape {
        bytes: Option<Vec<u8>>,
        expired: bool,
    },
    PrivateDiscard,
    RejectedCr,
}

impl TerminalReplyFilter {
    pub(super) fn quarantine(&mut self) {
        if matches!(self.state, State::Body { private: false, .. }) {
            // Consume a partial key, not merely its decoded events. Its suffix
            // stays in bounded discard custody, never in the next input lease.
            self.state = State::PrivateDiscard;
            return;
        }
        // Forget key disambiguation, not terminal control-string custody.
        let unowned = match &self.state {
            State::Esc | State::Csi | State::OscPrefix { .. } => true,
            State::Body { private, bytes } => !Self::owned_csi(*private, bytes),
            _ => false,
        };
        if unowned {
            self.state = State::Ground;
            self.reply_window = None;
        }
    }

    pub(super) fn paste_boundary(&mut self) {
        // The outer framer has positively identified a new input frame. No
        // unfinished terminal report may retain custody beyond that boundary.
        self.state = State::Ground;
        self.reply_window = None;
    }

    pub(super) fn colors_need_refresh(&self) -> bool {
        self.colors_need_refresh || self.appearance_mode.retry_ready()
    }

    pub(super) fn reacquire(&mut self) {
        self.invalidate_colors();
        self.appearance_mode.reacquire();
        self.grapheme_mode.reacquire();
    }

    pub(super) fn begin_color_probe(&mut self, query_mode: bool) -> bool {
        self.received_colors = 0;
        self.accept_colors = true;
        self.probe_interrupted = false;
        self.colors_need_refresh = true;
        self.appearance_mode.value = None;
        self.end_mode_capture();
        query_mode && self.appearance_mode.begin()
    }

    pub(super) fn color_probe_complete(&self, query_mode: bool) -> bool {
        self.received_colors == ALL_COLORS && (!query_mode || self.appearance_mode.value.is_some())
    }

    pub(super) fn end_mode_capture(&mut self) {
        self.appearance_mode.end();
        self.grapheme_mode.end();
    }

    pub(super) fn finish_color_probe(&mut self) {
        self.end_mode_capture();
        if !self.probe_interrupted {
            // No response is a normal completed observation attempt, not a
            // request to query on every frame. Late replies still apply.
            self.colors_need_refresh = false;
        }
    }

    pub(super) fn invalidate_colors(&mut self) {
        self.colors = TerminalColors {
            appearance: self.colors.appearance,
            ..TerminalColors::default()
        };
        self.colors_need_refresh = true;
        self.accept_colors = false;
        self.received_colors = 0;
        self.probe_interrupted = true;
    }

    pub(super) fn reply_deadline(&self) -> Option<Instant> {
        self.reply_window.map(|window| match window {
            ReplyWindow::Collecting { deadline, .. } | ReplyWindow::Draining { deadline, .. } => {
                deadline
            }
        })
    }

    pub(super) fn expire(&mut self, now: Instant) -> Vec<KeyFrame> {
        let mut frames = Vec::new();
        match self.reply_window {
            Some(ReplyWindow::Collecting { deadline, .. }) if now >= deadline => {
                self.end_mode_capture();
                self.abandon(
                    InputError::IncompleteTerminalReply,
                    &mut frames,
                    &mut Vec::new(),
                );
                if self
                    .reply_deadline()
                    .is_some_and(|deadline| now >= deadline)
                {
                    self.state = State::Ground;
                    self.reply_window = None;
                }
            }
            Some(ReplyWindow::Draining { deadline, .. }) if now >= deadline => {
                self.state = State::Ground;
                self.reply_window = None;
            }
            _ => {}
        }
        frames
    }

    pub(super) fn finish(&mut self) -> Vec<KeyFrame> {
        let incomplete = matches!(self.reply_window, Some(ReplyWindow::Collecting { .. }));
        self.state = State::Ground;
        self.reply_window = None;
        if incomplete {
            vec![
                KeyFrame::Boundary,
                KeyFrame::Rejected(InputError::IncompleteTerminalReply),
            ]
        } else {
            Vec::new()
        }
    }

    fn drain(&mut self) {
        if let Some(ReplyWindow::Collecting { drain_until, .. }) = self.reply_window {
            self.reply_window = Some(ReplyWindow::Draining {
                deadline: drain_until,
                remaining: MAX_RECOVERY_BYTES,
            });
        }
    }

    fn abandon(&mut self, error: InputError, frames: &mut Vec<KeyFrame>, output: &mut Vec<u8>) {
        if matches!(self.reply_window, Some(ReplyWindow::Collecting { .. })) {
            self.drain();
            Self::removed(frames, output, false);
            frames.push(KeyFrame::Rejected(error));
        }
        self.state = match std::mem::take(&mut self.state) {
            State::Osc { .. } => State::OscDiscard,
            State::OscEscape { expired, .. } => State::OscEscape {
                bytes: None,
                expired,
            },
            // Keep the small CSI candidate during recovery: a complete late
            // DECRQM report can drain an abandoned capture, never authorize it.
            state => state,
        };
    }

    fn owns_reply(state: &State) -> bool {
        match state {
            State::Osc { .. }
            | State::OscDiscard
            | State::OscEscape { .. }
            | State::PrivateDiscard
            | State::RejectedCr => true,
            State::Body { private, bytes } => Self::owned_csi(*private, bytes),
            _ => false,
        }
    }

    pub(super) fn feed(&mut self, bytes: &[u8], flush: bool, cursor_query: bool) -> Vec<KeyFrame> {
        let mut frames = self.expire(Instant::now());
        let mut output = Vec::with_capacity(bytes.len());
        for &byte in bytes {
            if let Some(ReplyWindow::Draining { remaining, .. }) = &mut self.reply_window {
                if *remaining == 0 {
                    self.state = State::Ground;
                    self.reply_window = None;
                    Self::removed(&mut frames, &mut output, false);
                } else {
                    *remaining -= 1;
                }
            }
            let mut state = std::mem::take(&mut self.state);
            if matches!(state, State::RejectedCr) {
                self.reply_window = None;
                if byte == b'\n' {
                    continue; // CRLF is one rejected line boundary, not a fresh Enter.
                }
                state = State::Ground;
            }
            if let State::OscEscape { bytes, expired } = state {
                if byte == b'\\' {
                    self.finish_osc(bytes.as_deref(), &mut frames, &mut output);
                    self.reply_window = None;
                    continue;
                }
                // ESC terminates OSC and starts a new escape sequence (as in
                // the VT parser). After its deadline the Escape key was already
                // emitted: do not turn the next ordinary key into an Alt key.
                self.reply_window = None;
                state = if !expired && byte.is_ascii_control() && byte != b'\x1b' {
                    // A pending ST introducer must not turn Stop into Alt-Ctrl-C.
                    // Deliver Escape and the C0 key separately at this boundary.
                    output.push(b'\x1b');
                    Self::removed(&mut frames, &mut output, false);
                    State::Ground
                } else if expired {
                    State::Ground
                } else {
                    State::Esc
                };
            }
            self.state = match state {
                State::Ground if byte == b'\x1b' => State::Esc,
                State::Ground => {
                    output.push(byte);
                    State::Ground
                }
                State::Esc if byte == b'[' => State::Csi,
                State::Esc if byte == b']' => State::OscPrefix {
                    command: Vec::new(),
                },
                State::Esc => {
                    output.push(b'\x1b');
                    if byte == b'\x1b' {
                        State::Esc
                    } else {
                        output.push(byte);
                        State::Ground
                    }
                }
                State::Csi if byte == b'?' => State::Body {
                    private: true,
                    bytes: Vec::new(),
                },
                State::Csi if byte.is_ascii_digit() => State::Body {
                    private: false,
                    bytes: vec![byte],
                },
                State::Csi => {
                    output.extend_from_slice(b"\x1b[");
                    if byte == b'\x1b' {
                        State::Esc
                    } else {
                        output.push(byte);
                        State::Ground
                    }
                }
                State::Body { private, mut bytes } => {
                    if matches!(byte, b'\r' | b'\n') && Self::owned_csi(private, &bytes) {
                        self.reject_newline(byte, &mut frames, &mut output)
                    } else if byte == b'\x1b' || byte.is_ascii_control() {
                        if !Self::owned_csi(private, &bytes) {
                            Self::emit(&mut output, private, &bytes);
                        }
                        Self::removed(&mut frames, &mut output, false);
                        if byte == b'\x1b' {
                            State::Esc
                        } else {
                            output.push(byte);
                            State::Ground
                        }
                    } else {
                        bytes.push(byte);
                        if (0x40..=0x7e).contains(&byte) || bytes.len() >= 128 {
                            let owned = Self::owned_csi(private, &bytes);
                            let previous = (self.colors, self.colors_need_refresh());
                            let retry_was_ready = self.appearance_mode.retry_ready();
                            if self.reply(private, &bytes, cursor_query)
                                || Self::setting_csi(private, &bytes)
                            {
                                Self::removed(
                                    &mut frames,
                                    &mut output,
                                    previous != (self.colors, self.colors_need_refresh())
                                        || (!retry_was_ready && self.appearance_mode.retry_ready()),
                                );
                            } else if let Some(key) = Self::key_event(private, &bytes) {
                                Self::removed(&mut frames, &mut output, false);
                                frames.push(key);
                            } else {
                                Self::emit(&mut output, private, &bytes);
                            }
                            if owned && !(0x40..=0x7e).contains(&byte) {
                                self.state = State::PrivateDiscard;
                                self.abandon(
                                    InputError::TerminalReplyTooLarge,
                                    &mut frames,
                                    &mut output,
                                );
                                State::PrivateDiscard
                            } else {
                                State::Ground
                            }
                        } else {
                            State::Body { private, bytes }
                        }
                    }
                }
                State::OscPrefix { mut command } => {
                    if byte.is_ascii_digit() && command.len() < MAX_OSC_BYTES {
                        command.push(byte);
                        State::OscPrefix { command }
                    } else if byte == b';' && !command.is_empty() && command.len() < MAX_OSC_BYTES {
                        Self::removed(&mut frames, &mut output, false);
                        command.push(byte);
                        State::Osc { bytes: command }
                    } else {
                        output.extend_from_slice(b"\x1b]");
                        output.extend_from_slice(&command);
                        if byte == b'\x1b' {
                            State::Esc
                        } else {
                            output.push(byte);
                            State::Ground
                        }
                    }
                }
                State::Osc { mut bytes } => {
                    if byte == b'\x07' {
                        self.finish_osc(Some(&bytes), &mut frames, &mut output);
                        State::Ground
                    } else if byte == b'\x1b' {
                        State::OscEscape {
                            bytes: Some(bytes),
                            expired: false,
                        }
                    } else if matches!(byte, b'\r' | b'\n') {
                        self.reject_newline(byte, &mut frames, &mut output)
                    } else if byte.is_ascii_control() {
                        // C0 keys (especially Stop, CAN and SUB) must remain
                        // actionable even if a terminal left a report open.
                        Self::removed(&mut frames, &mut output, false);
                        output.push(byte);
                        State::Ground
                    } else if bytes.len() >= MAX_OSC_BYTES {
                        self.state = State::OscDiscard;
                        self.abandon(InputError::TerminalReplyTooLarge, &mut frames, &mut output);
                        State::OscDiscard
                    } else {
                        bytes.push(byte);
                        State::Osc { bytes }
                    }
                }
                State::PrivateDiscard => {
                    if byte == b'\x1b' {
                        State::Esc
                    } else if matches!(byte, b'\r' | b'\n') {
                        self.reject_newline(byte, &mut frames, &mut output)
                    } else if byte.is_ascii_control() {
                        Self::removed(&mut frames, &mut output, false);
                        output.push(byte);
                        State::Ground
                    } else if (0x40..=0x7e).contains(&byte) {
                        Self::removed(&mut frames, &mut output, false);
                        State::Ground
                    } else {
                        State::PrivateDiscard
                    }
                }
                State::OscDiscard => {
                    if byte == b'\x07' {
                        Self::removed(&mut frames, &mut output, false);
                        State::Ground
                    } else if byte == b'\x1b' {
                        State::OscEscape {
                            bytes: None,
                            expired: false,
                        }
                    } else if matches!(byte, b'\r' | b'\n') {
                        self.reject_newline(byte, &mut frames, &mut output)
                    } else if byte.is_ascii_control() {
                        Self::removed(&mut frames, &mut output, false);
                        output.push(byte);
                        State::Ground
                    } else {
                        State::OscDiscard
                    }
                }
                State::OscEscape { .. } | State::RejectedCr => {
                    unreachable!("reply boundary was resolved before dispatch")
                }
            };
            if Self::owns_reply(&self.state) {
                if self.reply_window.is_none() {
                    let deadline = Instant::now() + REPLY_TIMEOUT;
                    self.reply_window = Some(ReplyWindow::Collecting {
                        deadline,
                        drain_until: deadline + RECOVERY_TIMEOUT,
                    });
                }
            } else {
                self.reply_window = None;
            }
        }
        if flush {
            match std::mem::take(&mut self.state) {
                State::Esc => output.push(b'\x1b'),
                State::Csi => output.extend_from_slice(b"\x1b["),
                State::Body { private, bytes } => {
                    if Self::owned_csi(private, &bytes) {
                        // A terminal reply has its own deadline, not the much
                        // shorter Escape-key grace period.
                        self.state = State::Body { private, bytes };
                    } else {
                        Self::emit(&mut output, private, &bytes);
                    }
                }
                State::PrivateDiscard => self.state = State::PrivateDiscard,
                State::RejectedCr => self.state = State::RejectedCr,
                State::OscPrefix { command } => {
                    output.extend_from_slice(b"\x1b]");
                    output.extend_from_slice(&command);
                }
                state @ (State::Osc { .. } | State::OscDiscard) => self.state = state,
                State::OscEscape { expired, .. } => {
                    Self::removed(&mut frames, &mut output, false);
                    if !expired {
                        output.push(b'\x1b');
                        Self::removed(&mut frames, &mut output, false);
                    }
                    // Preserve a possible late ST backslash, but release the
                    // Escape key now and all non-ST typeahead after it.
                    self.drain();
                    self.state = State::OscEscape {
                        bytes: None,
                        expired: true,
                    };
                }
                State::Ground => {}
            }
        }
        if !output.is_empty() {
            frames.push(KeyFrame::Bytes(output));
        }
        frames
    }

    fn reject_newline(
        &mut self,
        byte: u8,
        frames: &mut Vec<KeyFrame>,
        output: &mut Vec<u8>,
    ) -> State {
        // A newline after indeterminate discarded text must not look like an
        // intact input line to a consumer. Reject it explicitly; Stop/Cancel
        // remain actionable through the other C0/ESC branches.
        self.end_mode_capture();
        Self::removed(frames, output, false);
        frames.push(KeyFrame::Rejected(InputError::IncompleteTerminalReply));
        if byte == b'\r' {
            // Only a possible CRLF suffix remains owned; any ordinary typing
            // immediately ends this one-byte guard, even before its deadline.
            self.reply_window = Some(ReplyWindow::Draining {
                deadline: Instant::now() + RECOVERY_TIMEOUT,
                remaining: 1,
            });
            State::RejectedCr
        } else {
            State::Ground
        }
    }

    fn owned_csi(private: bool, bytes: &[u8]) -> bool {
        // Until its final byte, a numeric CSI may be a release report. The
        // Escape grace period cannot authorize replaying its parameters as text.
        Self::setting_csi(private, bytes)
            || (!private && bytes.first().is_some_and(u8::is_ascii_digit))
    }

    fn event_csi(private: bool, bytes: &[u8]) -> bool {
        !private
            && bytes
                .split(|byte| *byte == b';')
                .nth(1)
                .is_some_and(|field| field.contains(&b':'))
    }

    // Only normalize event metadata. The retained key parser still owns key
    // identity; an unsupported framed key must never fall back to draft text.
    fn key_event(private: bool, bytes: &[u8]) -> Option<KeyFrame> {
        if !Self::event_csi(private, bytes) {
            return None;
        }
        let normalized = || -> Option<KeyFrame> {
            let (&last, body) = bytes.split_last()?;
            if !matches!(
                last,
                b'u' | b'~' | b'A' | b'B' | b'C' | b'D' | b'H' | b'F' | b'P' | b'Q' | b'S'
            ) {
                return None;
            }
            let text = std::str::from_utf8(body).ok()?;
            let (key, modifier) = text.split_once(';')?;
            if key.is_empty() || !key.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            // Only the outer bounded framer may enter or leave paste mode.
            if last == b'~' && matches!(key.parse::<u16>(), Ok(200 | 201)) {
                return None;
            }
            let (modifier, event) = modifier.split_once(':')?;
            let modifier = modifier.parse::<u16>().ok()?.checked_sub(1)?;
            if modifier > 255 || modifier & 0x38 != 0 {
                // Super/Hyper/Meta must not alias an unmodified binding.
                return None;
            }
            let repeat = match event {
                "1" => false,
                "2" => true,
                _ => return None, // Releases and unknown event types are inert.
            };
            let mut modifiers = Modifiers::NONE;
            for (bit, flag) in [
                (1, Modifiers::SHIFT),
                (2, Modifiers::ALT),
                (4, Modifiers::CONTROL),
            ] {
                if modifier & bit != 0 {
                    modifiers |= flag;
                }
            }
            // Decode the unmodified identity: some legacy keys have no
            // ";1" spelling. Caps/Num Lock are state, not binding modifiers.
            // Flags 3 do not request alternate identities or text payloads.
            let bytes = format!("\x1b[{key}{}", char::from(last)).into_bytes();
            Some(KeyFrame::Key {
                bytes,
                modifiers,
                repeat,
            })
        };
        Some(normalized().unwrap_or(KeyFrame::Boundary))
    }

    fn setting_csi(private: bool, bytes: &[u8]) -> bool {
        private
            && (bytes.starts_with(b"997;")
                || bytes.starts_with(b"2031;")
                || bytes.starts_with(b"2027;"))
    }

    fn finish_osc(
        &mut self,
        bytes: Option<&[u8]>,
        frames: &mut Vec<KeyFrame>,
        output: &mut Vec<u8>,
    ) {
        let previous = self.colors;
        if self.accept_colors
            && let Some(bytes) = bytes
            && let Some(mask) = observe_osc(&mut self.colors, bytes)
        {
            self.received_colors |= mask;
        }
        Self::removed(frames, output, previous != self.colors);
    }

    fn removed(frames: &mut Vec<KeyFrame>, output: &mut Vec<u8>, changed: bool) {
        if !output.is_empty() {
            frames.push(KeyFrame::Bytes(std::mem::take(output)));
        }
        // A removed protocol frame is a key-parser boundary: fragments on
        // either side must not synthesize keys or unchecked paste openers.
        frames.push(KeyFrame::Boundary);
        if changed {
            frames.push(KeyFrame::ColorsChanged);
        }
    }

    fn emit(output: &mut Vec<u8>, private: bool, bytes: &[u8]) {
        output.extend_from_slice(if private { b"\x1b[?" } else { b"\x1b[" });
        output.extend_from_slice(bytes);
    }

    fn reply(&mut self, private: bool, bytes: &[u8], cursor_query: bool) -> bool {
        let Some((&last, body)) = bytes.split_last() else {
            return false;
        };
        if private && (body.starts_with(b"2031;") || body.starts_with(b"2027;")) {
            let mode = (last == b'y')
                .then(|| body.strip_suffix(b"$"))
                .flatten()
                .and_then(|body| std::str::from_utf8(&body[5..]).ok())
                .and_then(|value| value.parse::<u8>().ok())
                .filter(|value| *value <= 4);
            if body.starts_with(b"2031;") {
                self.appearance_mode.observe(mode);
            } else {
                self.grapheme_mode.observe(mode);
            }
            return true;
        }
        if private && last == b'n' && body.starts_with(b"997;") {
            let appearance = match &body[4..] {
                b"1" => Some(Appearance::Dark),
                b"2" => Some(Appearance::Light),
                _ => None,
            };
            if let Some(appearance) = appearance {
                self.invalidate_colors();
                self.colors.appearance = Some(appearance);
            }
            return true;
        }
        if body.is_empty() || !body.iter().all(|b| b.is_ascii_digit() || *b == b';') {
            return false;
        }
        if private && matches!(last, b'u' | b'c') {
            // Flags are sufficient proof of support; DA1 is only the negative
            // sentinel. Never wait for DA1 after flags, or overwrite them.
            if last == b'u' {
                self.keyboard = Some(true);
            } else {
                self.keyboard.get_or_insert(false);
            }
            return true;
        }
        if !private && last == b'R' && cursor_query {
            self.cursor = std::str::from_utf8(body).ok().and_then(|text| {
                let (row, column) = text.split_once(';')?;
                Some((
                    column.parse::<u16>().ok()?.checked_sub(1)?,
                    row.parse::<u16>().ok()?.checked_sub(1)?,
                ))
            });
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistently_late_mode_reports_allow_one_fresh_capture_not_a_probe_loop() {
        let mut replies = TerminalReplyFilter::default();
        assert!(replies.begin_color_probe(true));
        replies.finish_color_probe();
        replies.feed(b"\x1b[?2031;2$y", false, false);
        assert!(replies.colors_need_refresh());
        assert!(replies.begin_color_probe(true));
        replies.finish_color_probe();
        replies.feed(b"\x1b[?2031;2$y", false, false);
        assert!(!replies.colors_need_refresh());
        assert!(!replies.begin_color_probe(true));
        assert_eq!(
            replies.appearance_mode.value, None,
            "neither late reply is authority to enable"
        );
    }

    #[test]
    fn recovery_has_a_byte_bound_and_incomplete_eof_is_reported_once() {
        let mut replies = TerminalReplyFilter::default();
        replies.feed(b"\x1b]11;rgb:", false, false);
        let frames = replies.feed(&vec![b'a'; MAX_OSC_BYTES + 1], false, false);
        assert_eq!(
            frames
                .iter()
                .filter(|frame| matches!(
                    frame,
                    KeyFrame::Rejected(InputError::TerminalReplyTooLarge)
                ))
                .count(),
            1
        );
        let mut tail = vec![b'a'; MAX_RECOVERY_BYTES];
        tail.push(b'z');
        let frames = replies.feed(&tail, true, false);
        assert!(matches!(frames.last(), Some(KeyFrame::Bytes(bytes)) if bytes.ends_with(b"z")));
        assert!(
            !frames
                .iter()
                .any(|frame| matches!(frame, KeyFrame::Rejected(_)))
        );
        assert!(replies.reply_deadline().is_none());

        replies.feed(b"\x1b]11;rgb:0/", false, false);
        assert!(matches!(
            replies.finish().as_slice(),
            [
                KeyFrame::Boundary,
                KeyFrame::Rejected(InputError::IncompleteTerminalReply)
            ]
        ));
        assert!(replies.finish().is_empty());
    }
}
