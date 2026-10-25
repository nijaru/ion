//! Literal-text editing only; admission, history browsing and rendering stay with the client.
use std::ops::Range;

use crate::keybindings::KeyAction;
use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kill {
    Backward,
    Forward,
}

pub(super) enum Edit {
    Move(usize),
    Delete {
        range: Range<usize>,
        kill: Option<Kill>,
    },
}

fn previous_grapheme(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(at, _)| at)
}

fn next_grapheme(text: &str, cursor: usize) -> usize {
    text[cursor..]
        .graphemes(true)
        .next()
        .map_or(cursor, |g| cursor + g.len())
}

fn line_start(text: &str, cursor: usize) -> usize {
    text[..cursor].rfind('\n').map_or(0, |at| at + 1)
}

fn line_end(text: &str, cursor: usize) -> usize {
    text[cursor..].find('\n').map_or(text.len(), |at| {
        let end = cursor + at;
        if end > cursor && text[..end].ends_with('\r') {
            end - 1
        } else {
            end
        }
    })
}

// Unicode word boundaries can fall inside a grapheme (for example Thai กำ).
// Snap outward with the complete literal's context, not a truncated word slice.
fn word_grapheme_boundary(text: &str, offset: usize, forward: bool) -> usize {
    let mut cursor = GraphemeCursor::new(offset, text.len(), true);
    if cursor
        .is_boundary(text, 0)
        .expect("complete literal supplies grapheme context")
    {
        return offset;
    }
    let boundary = if forward {
        cursor.next_boundary(text, 0)
    } else {
        cursor.prev_boundary(text, 0)
    };
    boundary
        .expect("complete literal supplies grapheme context")
        .expect("an interior grapheme offset has an outward boundary")
}

fn word_start(text: &str, cursor: usize) -> usize {
    let start = line_start(text, cursor);
    if cursor == start {
        return previous_grapheme(text, cursor);
    }
    let destination = text[start..cursor]
        .split_word_bound_indices()
        .rev()
        .find(|(_, part)| !part.chars().all(char::is_whitespace))
        .map_or(start, |(at, _)| start + at);
    word_grapheme_boundary(text, destination, false)
}

fn word_end(text: &str, cursor: usize) -> usize {
    let end = line_end(text, cursor);
    if cursor == end {
        return next_grapheme(text, cursor);
    }
    let destination = text[cursor..end]
        .split_word_bound_indices()
        .find(|(_, part)| !part.chars().all(char::is_whitespace))
        .map_or(end, |(at, part)| cursor + at + part.len());
    word_grapheme_boundary(text, destination, true)
}

pub(super) fn vertical_cursor(text: &str, cursor: usize, down: bool) -> Option<usize> {
    let start = line_start(text, cursor);
    let column = text[start..cursor].graphemes(true).count();
    let (target_start, target_end) = if down {
        let next_start = text[cursor..].find('\n').map(|at| cursor + at + 1)?;
        (next_start, line_end(text, next_start))
    } else {
        if start == 0 {
            return None;
        }
        let previous_end = previous_grapheme(text, start);
        (line_start(text, previous_end), previous_end)
    };
    Some(
        text[target_start..target_end]
            .grapheme_indices(true)
            .nth(column)
            .map_or(target_end, |(at, _)| target_start + at),
    )
}

/// Kill direction belongs to the semantic edit, not its physical shortcut.
pub(super) fn kill(action: KeyAction) -> Option<Kill> {
    match action {
        KeyAction::KillWordBackward | KeyAction::KillLineBackward => Some(Kill::Backward),
        KeyAction::KillWordForward | KeyAction::KillLineForward => Some(Kill::Forward),
        _ => None,
    }
}

pub(super) fn apply(text: &str, cursor: usize, action: KeyAction) -> Option<Edit> {
    use KeyAction::*;
    let kill = kill(action);
    Some(match action {
        LineStart => Edit::Move(line_start(text, cursor)),
        LineEnd => Edit::Move(line_end(text, cursor)),
        WordLeft => Edit::Move(word_start(text, cursor)),
        WordRight => Edit::Move(word_end(text, cursor)),
        Left => Edit::Move(previous_grapheme(text, cursor)),
        Right => Edit::Move(next_grapheme(text, cursor)),
        KillWordBackward => Edit::Delete {
            range: word_start(text, cursor)..cursor,
            kill,
        },
        KillWordForward => Edit::Delete {
            range: cursor..word_end(text, cursor),
            kill,
        },
        Backspace => Edit::Delete {
            range: previous_grapheme(text, cursor)..cursor,
            kill,
        },
        Delete => Edit::Delete {
            range: cursor..next_grapheme(text, cursor),
            kill,
        },
        Exit if !text.is_empty() => Edit::Delete {
            range: cursor..next_grapheme(text, cursor),
            kill,
        },
        KillLineBackward => {
            let start = line_start(text, cursor);
            Edit::Delete {
                range: if cursor == start {
                    previous_grapheme(text, cursor)
                } else {
                    start
                }..cursor,
                kill,
            }
        }
        KillLineForward => {
            let end = line_end(text, cursor);
            Edit::Delete {
                range: cursor..if cursor == end {
                    next_grapheme(text, cursor)
                } else {
                    end
                },
                kill,
            }
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kill_classification_matches_the_delete_ranges_it_owns() {
        let text = "alpha βeta\ngamma";
        for (cursor, action, expected) in [
            (text.len(), KeyAction::KillWordBackward, Kill::Backward),
            (text.len(), KeyAction::KillLineBackward, Kill::Backward),
            (0, KeyAction::KillWordForward, Kill::Forward),
            (
                "alpha βeta".len(),
                KeyAction::KillLineForward,
                Kill::Forward,
            ),
        ] {
            assert_eq!(kill(action), Some(expected));
            assert!(
                matches!(apply(text, cursor, action), Some(Edit::Delete { kill: Some(found), .. }) if found == expected)
            );
        }
        for action in [KeyAction::Backspace, KeyAction::Delete, KeyAction::Exit] {
            assert_eq!(kill(action), None);
            assert!(matches!(
                apply(text, text.len(), action),
                Some(Edit::Delete { kill: None, .. }) | None
            ));
        }
    }

    #[test]
    fn word_keys_respect_unicode_punctuation_and_line_boundaries() {
        let text = "café 👩‍💻 /src/file.rs\nnext";
        for (cursor, start, end) in [
            (0, 0, "café".len()),
            ("café ".len(), 0, "café 👩‍💻".len()),
            ("café 👩‍💻".len(), "café ".len(), "café 👩‍💻 /".len()),
            (
                "café 👩‍💻 /src/".len(),
                "café 👩‍💻 /src".len(),
                "café 👩‍💻 /src/file.rs".len(),
            ),
            (
                "café 👩‍💻 /src/file.rs".len(),
                "café 👩‍💻 /src/".len(),
                "café 👩‍💻 /src/file.rs\n".len(),
            ),
            (
                "café 👩‍💻 /src/file.rs\n".len(),
                "café 👩‍💻 /src/file.rs".len(),
                text.len(),
            ),
        ] {
            assert_eq!(word_start(text, cursor), start);
            assert_eq!(word_end(text, cursor), end);
        }
    }

    #[test]
    fn word_destinations_do_not_split_thai_spacing_mark_graphemes() {
        let text = "กำ";
        assert_eq!(text.graphemes(true).count(), 1);
        assert_eq!(word_start(text, text.len()), 0);
        assert_eq!(word_end(text, 0), text.len());
    }

    #[test]
    fn line_keys_do_not_split_crlf_or_unicode_graphemes() {
        let text = "e\u{301}👩‍💻\r\nnext";
        let end = "e\u{301}👩‍💻".len();
        assert_eq!(line_end(text, 0), end);
        assert_eq!(next_grapheme(text, 0), "e\u{301}".len());
        assert_eq!(previous_grapheme(text, end), "e\u{301}".len());
        assert_eq!(word_end(text, end), end + 2);
        assert_eq!(word_start(text, end + 2), end);
        assert_eq!(vertical_cursor(text, end, true), Some(end + 4));
        assert_eq!(
            vertical_cursor(text, end + 3, false),
            Some("e\u{301}".len())
        );
        assert!(apply("", 0, KeyAction::Exit).is_none());
    }
}
