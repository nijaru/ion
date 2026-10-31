//! Stage cells and bytes together; only qualified physical output commits them.
use std::io::{self, Write};

use crate::Screen;
use crossterm::SynchronizedUpdate;

/// Evidence of a flushed, geometry-qualified physical publication. Only this
/// publication owner can construct it; layout cannot grant authority.
pub struct PaintReceipt {
    geometry: (u16, u16),
    fullscreen: bool,
}

impl PaintReceipt {
    #[must_use]
    pub fn geometry(&self) -> (u16, u16) {
        self.geometry
    }

    #[must_use]
    pub fn is_fullscreen(&self) -> bool {
        self.fullscreen
    }
}

pub(crate) struct PreparedPaint {
    screen: Screen,
    bytes: Vec<u8>,
    geometry: (u16, u16),
    fullscreen: bool,
}

impl PreparedPaint {
    pub(crate) fn new(
        screen: &Screen,
        fullscreen: bool,
        synchronized: bool,
        paint: impl FnOnce(&mut Screen, &mut Vec<u8>) -> io::Result<()>,
    ) -> io::Result<Self> {
        let geometry = if fullscreen {
            screen.fullscreen_size()
        } else {
            screen.primary_size()
        };
        let mut staged = screen.staged();
        let mut bytes = Vec::new();
        if synchronized {
            bytes.sync_update(|out| paint(&mut staged, out))??;
        } else {
            paint(&mut staged, &mut bytes)?;
        }
        Ok(Self {
            screen: staged,
            bytes,
            geometry,
            fullscreen,
        })
    }

    pub(crate) fn publish(
        self,
        screen: &mut Screen,
        current_geometry: (u16, u16),
        out: &mut impl Write,
    ) -> io::Result<Option<PaintReceipt>> {
        if current_geometry != self.geometry {
            screen.note_resize();
            return Ok(None);
        }
        out.write_all(&self.bytes)?;
        out.flush()?;
        *screen = self.screen;
        Ok(Some(PaintReceipt {
            geometry: self.geometry,
            fullscreen: self.fullscreen,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Frame;
    use ratatui::text::Line;

    fn prepare(screen: &Screen, fullscreen: bool) -> PreparedPaint {
        let rows = [Line::raw("Alpha")];
        PreparedPaint::new(screen, fullscreen, true, |screen, out| {
            if fullscreen {
                screen.draw_fullscreen(out, &rows, None)
            } else {
                screen.ensure_live_height(out, 4)?;
                screen.draw(
                    out,
                    &Frame {
                        live: &rows,
                        cursor: Some((0, 0)),
                    },
                )
            }
        })
        .unwrap()
    }

    #[test]
    fn geometry_rejection_emits_nothing_and_never_commits_staged_surface() {
        for fullscreen in [false, true] {
            let mut screen = Screen::new(20, 0, 8);
            let prepared = prepare(&screen, fullscreen);
            let mut out = Vec::new();
            assert!(
                prepared
                    .publish(&mut screen, (21, 8), &mut out)
                    .unwrap()
                    .is_none()
            );
            assert!(out.is_empty());
            assert_eq!(screen.live_height(), 1);
            assert!(screen.reconciliation_pending());
            let receipt = prepare(&screen, fullscreen)
                .publish(&mut screen, (20, 8), &mut out)
                .unwrap()
                .unwrap();
            assert_eq!(receipt.geometry(), (20, 8));
            assert_eq!(receipt.is_fullscreen(), fullscreen);
            let text = String::from_utf8(out).unwrap();
            assert!(text.contains("Alpha"));
            assert!(text.starts_with("\x1b[?2026h"));
            assert!(text.ends_with("\x1b[?2026l"));
        }
    }

    struct FailedOutput {
        flush: bool,
    }
    impl Write for FailedOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.flush {
                Ok(bytes.len())
            } else {
                Err(io::Error::other("write failed"))
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("flush failed"))
        }
    }

    #[test]
    fn write_or_flush_failure_cannot_return_a_receipt_or_commit_the_surface() {
        for fullscreen in [false, true] {
            for flush in [false, true] {
                let mut screen = Screen::new(20, 0, 8);
                let result = prepare(&screen, fullscreen).publish(
                    &mut screen,
                    (20, 8),
                    &mut FailedOutput { flush },
                );
                assert!(result.is_err());
                assert_eq!(screen.live_height(), 1);
                // The rejected physical frame did not advance the cell cache.
                let mut out = Vec::new();
                assert!(
                    prepare(&screen, fullscreen)
                        .publish(&mut screen, (20, 8), &mut out)
                        .unwrap()
                        .is_some()
                );
                assert!(String::from_utf8(out).unwrap().contains("Alpha"));
            }
        }
    }
}
