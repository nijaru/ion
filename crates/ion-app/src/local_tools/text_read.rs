//! Bounded text pages: one-based LF records or explicit zero-based UTF-8 bytes.
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
};

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::files::{MAX_FILE_BYTES, hex_digest};

pub(super) const MAX_LINES: usize = 2000;
pub(super) const MAX_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy)]
pub(super) enum Range {
    Lines { offset: u64, limit: usize },
    Bytes { offset: u64, limit: usize },
}

pub(super) fn read(
    mut file: File,
    size: u64,
    path: &str,
    range: Range,
    stop: &CancellationToken,
) -> Result<Value, String> {
    // Image sniffing already consumed the header on this same verified descriptor.
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let (page, digest, file_bytes) = if size <= MAX_FILE_BYTES as u64 {
        let mut all = Vec::new();
        file.take(MAX_FILE_BYTES as u64 + 1)
            .read_to_end(&mut all)
            .map_err(|e| e.to_string())?;
        if all.len() > MAX_FILE_BYTES {
            return Err("text file grew beyond the 8 MiB digest bound during read".into());
        }
        let page = select_range(&mut std::io::Cursor::new(&all), range, stop)?;
        (page, Some(hex_digest(&all)), all.len() as u64)
    } else {
        (
            select_range(&mut BufReader::new(file), range, stop)?,
            None,
            size,
        )
    };
    let mut value = json!({
        "path": path, "content": page.content, "has_more": page.has_more,
        "file_bytes": file_bytes, "base_digest": digest,
    });
    match range {
        Range::Lines { offset, .. } => {
            value["start_line"] = json!(offset);
            value["next_line"] = json!(page.next_offset);
        }
        Range::Bytes { offset, .. } => {
            value["start_byte"] = json!(offset);
            value["next_byte"] = json!(page.next_offset);
        }
    }
    Ok(value)
}

struct Page {
    content: String,
    next_offset: u64,
    has_more: bool,
}

fn select_range(
    reader: &mut (impl BufRead + Seek),
    range: Range,
    stop: &CancellationToken,
) -> Result<Page, String> {
    match range {
        Range::Lines { offset, limit } => select_lines(reader, offset, limit, stop),
        Range::Bytes { offset, limit } => select_bytes(reader, offset, limit, stop),
    }
}

fn select_bytes(
    reader: &mut (impl BufRead + Seek),
    offset: u64,
    limit: usize,
    stop: &CancellationToken,
) -> Result<Page, String> {
    check_stop(stop)?;
    let end = reader.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    if offset > end {
        return Err(format!(
            "byte_offset {offset} is beyond end of file ({end} bytes)"
        ));
    }
    reader
        .seek(SeekFrom::Start(offset))
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    Read::take(reader, limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    check_stop(stop)?;
    let mut count = bytes.len().min(limit);
    if let Err(error) = std::str::from_utf8(&bytes[..count]) {
        if error.error_len().is_none() && bytes.len() > limit && error.valid_up_to() > 0 {
            count = error.valid_up_to();
        } else {
            return Err("byte range is not UTF-8; byte_offset must start at a character boundary (use the previous next_byte)".into());
        }
    }
    let has_more = bytes.len() > count;
    bytes.truncate(count);
    Ok(Page {
        content: String::from_utf8(bytes).map_err(|e| e.to_string())?,
        next_offset: offset
            .checked_add(count as u64)
            .ok_or("byte offset overflow")?,
        has_more,
    })
}

fn select_lines(
    reader: &mut impl BufRead,
    offset: u64,
    limit: usize,
    stop: &CancellationToken,
) -> Result<Page, String> {
    let mut current = 1;
    while current < offset {
        if !skip_line(reader, stop)? {
            return Err(format!(
                "offset {offset} is beyond end of file (EOF at line {current})"
            ));
        }
        current += 1;
    }
    let mut bytes = Vec::new();
    let mut byte_limited = false;
    for _ in 0..limit {
        check_stop(stop)?;
        let remaining = MAX_BYTES - bytes.len();
        let mut line = Vec::new();
        // Read at most one byte beyond the remaining allowance to detect an
        // oversized record without allocating it or dropping its continuation.
        Read::take(&mut *reader, remaining as u64 + 1)
            .read_until(b'\n', &mut line)
            .map_err(|e| e.to_string())?;
        if line.is_empty() {
            break;
        }
        if line.len() > remaining {
            if bytes.is_empty() {
                return Err(format!(
                    "line {current} exceeds the {MAX_BYTES}-byte read bound; use byte_offset=0 and byte_limit to paginate without requiring complete lines"
                ));
            }
            byte_limited = true;
            break;
        }
        bytes.extend_from_slice(&line);
        current = current.checked_add(1).ok_or("line number overflow")?;
    }
    check_stop(stop)?;
    let has_more = byte_limited || !reader.fill_buf().map_err(|e| e.to_string())?.is_empty();
    let content = String::from_utf8(bytes)
        .map_err(|_| "read range is not UTF-8; use exec for binary data")?;
    Ok(Page {
        content,
        next_offset: current,
        has_more,
    })
}

fn skip_line(reader: &mut impl BufRead, stop: &CancellationToken) -> Result<bool, String> {
    let mut any = false;
    loop {
        check_stop(stop)?;
        let available = reader.fill_buf().map_err(|e| e.to_string())?;
        if available.is_empty() {
            return Ok(any);
        }
        any = true;
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |position| position + 1);
        reader.consume(consumed);
        if newline.is_some() {
            return Ok(true);
        }
    }
}

fn check_stop(stop: &CancellationToken) -> Result<(), String> {
    if stop.is_cancelled() {
        Err("cancelled during text read".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn ranges_match_lf_records_without_phantom_trailing_lines() {
        for text in ["", "\n\n\n", "last", "last\n", "\u{feff}α\r\n\n🦀last"] {
            let lines: Vec<_> = text.split_inclusive('\n').collect();
            for offset in 1..=lines.len() + 2 {
                for limit in 1..=4 {
                    let mut reader = BufReader::with_capacity(1, Cursor::new(text.as_bytes()));
                    let result =
                        select_lines(&mut reader, offset as u64, limit, &CancellationToken::new());
                    if offset > lines.len() + 1 {
                        assert!(result.is_err());
                    } else {
                        let page = result.unwrap();
                        let end = (offset - 1 + limit).min(lines.len());
                        assert_eq!(page.content, lines[offset - 1..end].concat());
                        assert_eq!(page.next_offset, end as u64 + 1);
                        assert_eq!(page.has_more, end < lines.len());
                    }
                }
            }
        }
    }

    #[test]
    fn byte_pages_preserve_utf8_and_report_actual_continuations() {
        let text = "\u{feff}α\r\n🙂last";
        let stop = CancellationToken::new();
        for limit in 4..=9 {
            let mut reader = Cursor::new(text.as_bytes());
            let mut offset = 0;
            let mut full = String::new();
            loop {
                let page = select_bytes(&mut reader, offset, limit, &stop).unwrap();
                assert!(page.content.len() <= limit);
                full.push_str(&page.content);
                if !page.has_more {
                    break;
                }
                assert!(page.next_offset > offset);
                offset = page.next_offset;
            }
            assert_eq!(full, text);
        }
        for offset in [1, 2, 4, 8, 9, 10] {
            assert!(select_bytes(&mut Cursor::new(text.as_bytes()), offset, 4, &stop).is_err());
        }
        let at_eof = select_bytes(
            &mut Cursor::new(text.as_bytes()),
            text.len() as u64,
            4,
            &stop,
        )
        .unwrap();
        assert!(at_eof.content.is_empty() && !at_eof.has_more);
        assert!(
            select_bytes(
                &mut Cursor::new(text.as_bytes()),
                text.len() as u64 + 1,
                4,
                &stop
            )
            .is_err()
        );
        for invalid in [&b"\xff"[..], &b"x\xf0\x9f"[..]] {
            assert!(select_bytes(&mut Cursor::new(invalid), 0, 4, &stop).is_err());
        }
    }

    #[test]
    fn prefix_scan_observes_cancellation_between_io_reads() {
        struct CancelRead(CancellationToken);
        impl Read for CancelRead {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                if self.0.is_cancelled() {
                    return Err(std::io::Error::other("continued I/O after cancellation"));
                }
                bytes.fill(b'x');
                self.0.cancel();
                Ok(bytes.len())
            }
        }
        let stop = CancellationToken::new();
        let mut reader = BufReader::with_capacity(8, CancelRead(stop.clone()));
        let result = select_lines(&mut reader, 2, 1, &stop);
        assert!(matches!(result, Err(reason) if reason == "cancelled during text read"));
    }

    #[test]
    fn byte_bound_preserves_complete_lines_and_oversized_continuation() {
        let text = format!("small\n{}\ntail", "x".repeat(MAX_BYTES));
        let page = select_lines(
            &mut text.as_bytes(),
            1,
            MAX_LINES,
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(page.content, "small\n");
        assert_eq!(page.next_offset, 2);
        assert!(page.has_more);
        assert!(select_lines(&mut text.as_bytes(), 2, 1, &CancellationToken::new()).is_err());
        let page = select_lines(&mut text.as_bytes(), 3, 1, &CancellationToken::new()).unwrap();
        assert_eq!(page.content, "tail");
        let exact = "x".repeat(MAX_BYTES);
        assert_eq!(
            select_lines(&mut exact.as_bytes(), 1, 1, &CancellationToken::new())
                .unwrap()
                .content,
            exact
        );
        assert!(select_lines(&mut &b"\xff\n"[..], 1, 1, &CancellationToken::new()).is_err());
    }
}
