//! Bounded LF-delimited text ranges; native read inputs use one-based lines.
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
};

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{MAX_FILE_BYTES, hex_digest};

pub(super) const MAX_LINES: usize = 2000;
const MAX_BYTES: usize = 64 * 1024;

pub(super) fn read(
    mut file: File,
    size: u64,
    path: &str,
    offset: u64,
    limit: usize,
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
        let page = select_lines(&mut all.as_slice(), offset, limit, stop)?;
        (page, Some(hex_digest(&all)), all.len() as u64)
    } else {
        (
            select_lines(&mut BufReader::new(file), offset, limit, stop)?,
            None,
            size,
        )
    };
    Ok(json!({
        "path": path, "content": page.content, "start_line": offset,
        "next_line": page.next_line, "has_more": page.has_more,
        "file_bytes": file_bytes, "base_digest": digest,
    }))
}

struct Page {
    content: String,
    next_line: u64,
    has_more: bool,
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
                    "line {current} exceeds the {MAX_BYTES}-byte read bound; use exec with an explicitly bounded byte read for this oversized line"
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
        next_line: current,
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
                        assert_eq!(page.next_line, end as u64 + 1);
                        assert_eq!(page.has_more, end < lines.len());
                    }
                }
            }
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
        assert_eq!(page.next_line, 2);
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
