//! Bounded encoded JSON accounting and windows for Core admission/delivery.
use std::io::{self, Write};

use serde::Serialize;

pub(crate) fn encoded_len(value: &impl Serialize) -> Result<usize, serde_json::Error> {
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| io::Error::other("encoded JSON size overflow"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.0)
}

pub(crate) struct JsonWindow {
    pub text: String,
    pub next_offset: Option<usize>,
    pub total_bytes: usize,
}

/// Serialize one bounded stored value, retaining only a page plus enough bytes
/// to validate UTF-8 edges. Offsets refer to encoded JSON, not decoded strings.
pub(crate) fn encoded_window(
    value: &impl Serialize,
    offset: usize,
    limit: usize,
) -> Result<Option<JsonWindow>, serde_json::Error> {
    struct Window {
        offset: usize,
        end: usize,
        seen: usize,
        data: Vec<u8>,
    }
    impl Write for Window {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let start = self.offset.saturating_sub(self.seen).min(bytes.len());
            let end = self.end.saturating_sub(self.seen).min(bytes.len());
            if start < end {
                self.data.extend_from_slice(&bytes[start..end]);
            }
            self.seen = self
                .seen
                .checked_add(bytes.len())
                .ok_or_else(|| io::Error::other("encoded JSON size overflow"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut window = Window {
        offset,
        end: offset.saturating_add(limit).saturating_add(3),
        seen: 0,
        data: Vec::new(),
    };
    serde_json::to_writer(&mut window, value)?;
    if offset > window.seen {
        return Ok(None);
    }
    if let Err(error) = std::str::from_utf8(&window.data) {
        if error.error_len().is_some() {
            return Ok(None);
        }
        window.data.truncate(error.valid_up_to());
    }
    let mut text = String::from_utf8(window.data).expect("validated UTF-8 page");
    let mut end = limit.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    let next = offset + text.len();
    Ok(Some(JsonWindow {
        text,
        next_offset: (next < window.seen).then_some(next),
        total_bytes: window.seen,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn windows_reassemble_exact_json_without_splitting_utf8() {
        for value in [
            json!("a😀é中\\\"\n"),
            json!({"n":u64::MAX,"s":"😀"}),
            json!(null),
        ] {
            let encoded = serde_json::to_string(&value).unwrap();
            for limit in 4..12 {
                let mut offset = 0;
                let mut collected = String::new();
                loop {
                    let page = encoded_window(&value, offset, limit).unwrap().unwrap();
                    assert!(page.text.len() <= limit);
                    assert_eq!(page.total_bytes, encoded.len());
                    collected.push_str(&page.text);
                    match page.next_offset {
                        Some(next) => {
                            assert!(next > offset);
                            offset = next;
                        }
                        None => break,
                    }
                }
                assert_eq!(collected, encoded);
            }
        }
        assert!(encoded_window(&json!("😀"), 2, 4).unwrap().is_none());
        assert!(
            encoded_window(&json!(null), usize::MAX, 4)
                .unwrap()
                .is_none()
        );
    }
}
