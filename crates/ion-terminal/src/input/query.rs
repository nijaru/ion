//! Recognize terminal replies without consuming unrelated type-ahead.
#[derive(Debug)]
pub(super) enum KeyFrame {
    Bytes(Vec<u8>),
    Reply,
}

#[derive(Debug, Default)]
pub(super) struct TerminalReplyFilter {
    state: State,
    pub(super) keyboard: Option<bool>,
    pub(super) cursor: Option<(u16, u16)>,
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
}

impl TerminalReplyFilter {
    pub(super) fn feed(&mut self, bytes: &[u8], flush: bool, cursor_query: bool) -> Vec<KeyFrame> {
        let mut frames = Vec::new();
        let mut output = Vec::with_capacity(bytes.len());
        for &byte in bytes {
            self.state = match std::mem::take(&mut self.state) {
                State::Ground if byte == b'\x1b' => State::Esc,
                State::Ground => {
                    output.push(byte);
                    State::Ground
                }
                State::Esc if byte == b'[' => State::Csi,
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
                    if byte == b'\x1b' {
                        Self::emit(&mut output, private, &bytes);
                        State::Esc
                    } else {
                        bytes.push(byte);
                        if (0x40..=0x7e).contains(&byte) || bytes.len() >= 128 {
                            if self.reply(private, &bytes, cursor_query) {
                                if !output.is_empty() {
                                    frames.push(KeyFrame::Bytes(std::mem::take(&mut output)));
                                }
                                // Flush key disambiguation at a removed reply. Otherwise
                                // unrelated fragments could form an unchecked paste opener.
                                frames.push(KeyFrame::Reply);
                            } else {
                                Self::emit(&mut output, private, &bytes);
                            }
                            State::Ground
                        } else {
                            State::Body { private, bytes }
                        }
                    }
                }
            };
        }
        if flush {
            match std::mem::take(&mut self.state) {
                State::Esc => output.push(b'\x1b'),
                State::Csi => output.extend_from_slice(b"\x1b["),
                State::Body { private, bytes } => Self::emit(&mut output, private, &bytes),
                State::Ground => {}
            }
        }
        if !output.is_empty() {
            frames.push(KeyFrame::Bytes(output));
        }
        frames
    }

    fn emit(output: &mut Vec<u8>, private: bool, bytes: &[u8]) {
        output.extend_from_slice(if private { b"\x1b[?" } else { b"\x1b[" });
        output.extend_from_slice(bytes);
    }

    fn reply(&mut self, private: bool, bytes: &[u8], cursor_query: bool) -> bool {
        let Some((&last, body)) = bytes.split_last() else {
            return false;
        };
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
