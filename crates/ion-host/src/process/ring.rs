//! Raw bounded rolling capture with monotonic byte cursors.
use std::{collections::VecDeque, sync::Mutex};

pub(super) const CAPTURE_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub(super) struct Ring {
    inner: Mutex<RingState>,
}

#[derive(Default)]
struct RingState {
    bytes: VecDeque<u8>,
    total: u64,
    eof: bool,
    error: Option<String>,
}

#[derive(Debug)]
pub(super) struct Window {
    pub bytes: Vec<u8>,
    pub first_available: u64,
    pub offset: u64,
    pub next_offset: u64,
    pub total_bytes: u64,
    pub gap_bytes: u64,
    pub complete: bool,
}

pub(super) struct Meta {
    pub total_bytes: u64,
    pub complete: bool,
    pub capture_error: Option<String>,
}

impl Ring {
    pub(super) fn append(&self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }
        let mut state = self.lock();
        state.total = state.total.saturating_add(chunk.len() as u64);
        let overflow = state
            .bytes
            .len()
            .saturating_add(chunk.len())
            .saturating_sub(CAPTURE_BYTES);
        let drop_existing = overflow.min(state.bytes.len());
        state.bytes.drain(..drop_existing);
        state.bytes.extend(&chunk[overflow - drop_existing..]);
    }

    pub(super) fn mark_eof(&self) {
        self.lock().eof = true;
    }

    pub(super) fn mark_error(&self, error: impl Into<String>) {
        let mut state = self.lock();
        let error = error.into();
        match &mut state.error {
            Some(previous) => {
                previous.push_str("; ");
                previous.push_str(&error);
            }
            None => state.error = Some(error),
        }
    }

    pub(super) fn meta(&self) -> Meta {
        let state = self.lock();
        Meta {
            total_bytes: state.total,
            complete: state.eof,
            capture_error: state.error.clone(),
        }
    }

    /// Copy one requested window. A future offset is a caller error; an evicted
    /// offset starts at the oldest retained byte and reports the gap.
    pub(super) fn read(
        &self,
        offset: u64,
        limit: usize,
        limit_max: usize,
    ) -> Result<Window, String> {
        let state = self.lock();
        if offset > state.total {
            return Err(format!(
                "offset {offset} is beyond captured bytes ({} total)",
                state.total
            ));
        }
        let retained = state.bytes.len() as u64;
        let first_available = state.total.saturating_sub(retained);
        let (actual, gap) = if offset < first_available {
            (first_available, first_available - offset)
        } else {
            (offset, 0)
        };
        let start = usize::try_from(actual - first_available).unwrap_or(usize::MAX);
        let available = usize::try_from(state.total - actual).unwrap_or(usize::MAX);
        let take = available.min(limit.min(limit_max));
        let bytes: Vec<u8> = state.bytes.iter().skip(start).take(take).copied().collect();
        Ok(Window {
            next_offset: actual.saturating_add(bytes.len() as u64),
            bytes,
            first_available,
            offset: actual,
            total_bytes: state.total,
            gap_bytes: gap,
            complete: state.eof,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RingState> {
        match self.inner.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_capture_reports_eviction_gaps_and_monotonic_cursors() {
        let ring = Ring::default();
        ring.append(&[b'a'; CAPTURE_BYTES]);
        ring.append(&[b'b'; 100]);
        let meta = ring.meta();
        assert_eq!(meta.total_bytes, CAPTURE_BYTES as u64 + 100);
        assert!(!meta.complete);

        let window = ring.read(0, 16, CAPTURE_BYTES).unwrap();
        assert_eq!(window.gap_bytes, 100);
        assert_eq!(window.first_available, 100);
        assert_eq!(window.offset, 100);
        assert_eq!(window.next_offset, 116);
        assert_eq!(window.total_bytes, CAPTURE_BYTES as u64 + 100);
        assert_eq!(window.bytes.len(), 16);
        assert!(window.bytes.iter().all(|byte| *byte == b'a'));

        let tail = ring
            .read(CAPTURE_BYTES as u64 + 90, 32, CAPTURE_BYTES)
            .unwrap();
        assert_eq!(tail.gap_bytes, 0);
        assert_eq!(tail.offset, CAPTURE_BYTES as u64 + 90);
        assert_eq!(tail.bytes, vec![b'b'; 10]);
        assert_eq!(tail.next_offset, tail.total_bytes);

        let empty = ring.read(tail.total_bytes, 32, CAPTURE_BYTES).unwrap();
        assert!(empty.bytes.is_empty());
        assert_eq!(empty.next_offset, empty.total_bytes);
        assert!(
            ring.read(tail.total_bytes + 1, 32, CAPTURE_BYTES)
                .unwrap_err()
                .contains("beyond captured bytes")
        );
    }

    #[test]
    fn eof_is_only_observed_completion_and_large_appends_keep_the_tail() {
        let ring = Ring::default();
        ring.append(&[b'x'; CAPTURE_BYTES + 10]);
        assert_eq!(
            ring.read(0, CAPTURE_BYTES, CAPTURE_BYTES).unwrap().bytes,
            vec![b'x'; CAPTURE_BYTES]
        );
        assert!(!ring.meta().complete);
        ring.mark_eof();
        assert!(ring.meta().complete);
        ring.mark_error("read fault");
        assert_eq!(ring.meta().capture_error.as_deref(), Some("read fault"));
    }
}
