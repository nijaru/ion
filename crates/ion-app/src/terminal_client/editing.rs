//! Literal-text editing only; admission, history browsing and rendering stay with the client.
use std::ops::Range;

use ion_terminal::{KeyCode, KeyEvent, Modifiers};
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

/// Which kill behavior a key owns, independent of the current draft.
pub(super) fn kill(key: KeyEvent) -> Option<Kill> {
    let control = key.modifiers.contains(Modifiers::CONTROL);
    let alt = key.modifiers.contains(Modifiers::ALT);
    let control_char = control && !alt;
    let alt_char = alt && !control;
    match key.code {
        KeyCode::Backspace if alt => Some(Kill::Backward),
        KeyCode::Delete if alt => Some(Kill::Forward),
        KeyCode::Char('w') if control_char => Some(Kill::Backward),
        KeyCode::Char('d') if alt_char => Some(Kill::Forward),
        KeyCode::Char('u') if control_char => Some(Kill::Backward),
        KeyCode::Char('k') if control_char => Some(Kill::Forward),
        _ => None,
    }
}

pub(super) fn key(text: &str, cursor: usize, key: KeyEvent) -> Option<Edit> {
    let control = key.modifiers.contains(Modifiers::CONTROL);
    let alt = key.modifiers.contains(Modifiers::ALT);
    let modified_word = control || alt;
    let control_char = control && !alt;
    let alt_char = alt && !control;
    let operation = match key.code {
        KeyCode::Home => Edit::Move(line_start(text, cursor)),
        KeyCode::End => Edit::Move(line_end(text, cursor)),
        KeyCode::Left if modified_word => Edit::Move(word_start(text, cursor)),
        KeyCode::Right if modified_word => Edit::Move(word_end(text, cursor)),
        KeyCode::Left => Edit::Move(previous_grapheme(text, cursor)),
        KeyCode::Right => Edit::Move(next_grapheme(text, cursor)),
        KeyCode::Backspace if alt => Edit::Delete {
            range: word_start(text, cursor)..cursor,
            kill: Some(Kill::Backward),
        },
        KeyCode::Delete if alt => Edit::Delete {
            range: cursor..word_end(text, cursor),
            kill: Some(Kill::Forward),
        },
        KeyCode::Backspace => Edit::Delete {
            range: previous_grapheme(text, cursor)..cursor,
            kill: None,
        },
        KeyCode::Delete => Edit::Delete {
            range: cursor..next_grapheme(text, cursor),
            kill: None,
        },
        KeyCode::Char('a') if control_char => Edit::Move(line_start(text, cursor)),
        KeyCode::Char('e') if control_char => Edit::Move(line_end(text, cursor)),
        KeyCode::Char('b') if control_char => Edit::Move(previous_grapheme(text, cursor)),
        KeyCode::Char('f') if control_char => Edit::Move(next_grapheme(text, cursor)),
        KeyCode::Char('b') if alt_char => Edit::Move(word_start(text, cursor)),
        KeyCode::Char('f') if alt_char => Edit::Move(word_end(text, cursor)),
        KeyCode::Char('w') if control_char => Edit::Delete {
            range: word_start(text, cursor)..cursor,
            kill: Some(Kill::Backward),
        },
        KeyCode::Char('d') if alt_char => Edit::Delete {
            range: cursor..word_end(text, cursor),
            kill: Some(Kill::Forward),
        },
        KeyCode::Char('d') if control_char && !text.is_empty() => Edit::Delete {
            range: cursor..next_grapheme(text, cursor),
            kill: None,
        },
        KeyCode::Char('u') if control_char => {
            let start = line_start(text, cursor);
            Edit::Delete {
                range: if cursor == start {
                    previous_grapheme(text, cursor)
                } else {
                    start
                }..cursor,
                kill: Some(Kill::Backward),
            }
        }
        KeyCode::Char('k') if control_char => {
            let end = line_end(text, cursor);
            Edit::Delete {
                range: cursor..if cursor == end {
                    next_grapheme(text, cursor)
                } else {
                    end
                },
                kill: Some(Kill::Forward),
            }
        }
        _ => return None,
    };
    Some(operation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kill_classification_matches_the_delete_ranges_it_owns() {
        let text = "alpha βeta\ngamma";
        for (cursor, key, expected) in [
            (
                text.len(),
                KeyEvent::new(KeyCode::Char('w'), Modifiers::CONTROL),
                Kill::Backward,
            ),
            (
                text.len(),
                KeyEvent::new(KeyCode::Char('u'), Modifiers::CONTROL),
                Kill::Backward,
            ),
            (
                "alpha βeta\n".len(),
                KeyEvent::new(KeyCode::Backspace, Modifiers::ALT),
                Kill::Backward,
            ),
            (
                0,
                KeyEvent::new(KeyCode::Delete, Modifiers::ALT),
                Kill::Forward,
            ),
            (
                "alpha βeta\n".len(),
                KeyEvent::new(KeyCode::Char('d'), Modifiers::ALT),
                Kill::Forward,
            ),
            (
                "alpha βeta".len(),
                KeyEvent::new(KeyCode::Char('k'), Modifiers::CONTROL),
                Kill::Forward,
            ),
        ] {
            assert_eq!(kill(key), Some(expected));
            assert!(matches!(
                self::key(text, cursor, key),
                Some(Edit::Delete {
                    kill: Some(found),
                    ..
                }) if found == expected
            ));
        }
        for key in [
            KeyEvent::new(KeyCode::Backspace, Modifiers::NONE),
            KeyEvent::new(KeyCode::Delete, Modifiers::NONE),
            KeyEvent::new(KeyCode::Char('d'), Modifiers::CONTROL),
        ] {
            assert_eq!(kill(key), None);
            assert!(matches!(
                self::key(text, text.len(), key),
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
        assert!(key("", 0, KeyEvent::new(KeyCode::Char('d'), Modifiers::CONTROL)).is_none());
    }
}
