//! One live human attachment lease over a supervised PTY program.
//!
//! The lease is an opaque owner token, not a saved credential or durable
//! attachment. Acquiring it serializes with the driver's input queue: delivered
//! prefixes are acknowledged, undelivered queued input is rejected, and model
//! writes/resize are refused until release. Model reads, screen projection and
//! process lifecycle remain available to the model throughout.
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use super::{
    Backend, Capture, ProcessOwner,
    driver::{InputRequest, ProcessState},
    screen::Dimensions,
};
use tokio_util::sync::CancellationToken;

/// Bound mirrors the model-facing write limit; human input is chunked by the
/// caller rather than silently truncated inside the driver.
const MAX_INPUT_BYTES: usize = 65_536;

pub(super) enum LeaseRequest {
    Acquire {
        token: Uuid,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Write {
        token: Uuid,
        data: Vec<u8>,
        reply: oneshot::Sender<super::driver::InputAck>,
    },
    Resize {
        token: Uuid,
        dimensions: Dimensions,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Release {
        token: Uuid,
        reply: Option<oneshot::Sender<()>>,
    },
}

/// One observed PTY program eligible for human attachment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalSummary {
    pub id: Uuid,
    pub rows: u16,
    pub columns: u16,
    pub attached: bool,
    pub running: bool,
}

/// Bounded screen observation returned to the attached view.
#[derive(Clone, Debug)]
pub struct AttachmentScreen {
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
}

#[derive(Clone, Debug)]
pub struct AttachmentPage {
    pub start_row: u16,
    pub visible_rows: u16,
    pub text: String,
    pub screen: AttachmentScreen,
}

#[derive(Debug)]
pub struct AttachmentWrite {
    pub bytes_written: usize,
    pub error: Option<String>,
    pub cancelled: bool,
}

/// A live input lease. Dropping it sends a best-effort release; explicit
/// [`Self::release`] should be used at known view boundaries.
pub struct TerminalAttachment {
    id: Uuid,
    token: Uuid,
    control: mpsc::Sender<LeaseRequest>,
    state: Arc<ProcessState>,
    released: AtomicBool,
}

impl TerminalAttachment {
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// Live means the driver has not exited or joined; it does not imply the
    /// program is still accepting terminal input.
    pub fn is_live(&self) -> bool {
        let status = self.state.status();
        status.started && status.exit.is_none() && !status.joined
    }

    pub fn is_attached(&self) -> bool {
        !self.released.load(Ordering::Acquire) && self.state.lease_owner() == Some(self.token)
    }

    pub fn page(&self, start_row: u16, rows: u16) -> Result<AttachmentPage, String> {
        let Capture::Pty { screen, .. } = &self.state.capture else {
            return Err("terminal attachment requires a PTY program".into());
        };
        let page = screen.page(start_row, rows)?;
        let meta = page.meta;
        Ok(AttachmentPage {
            start_row: page.start_row,
            visible_rows: page.visible_rows,
            text: page.text,
            screen: AttachmentScreen {
                revision: meta.revision,
                source_bytes: meta.source_bytes,
                rows: meta.rows,
                columns: meta.columns,
                cursor_row: meta.cursor_row,
                cursor_column: meta.cursor_column,
                cursor_visible: meta.cursor_visible,
                alternate_screen: meta.alternate_screen,
                application_cursor: meta.application_cursor,
                application_keypad: meta.application_keypad,
                bracketed_paste: meta.bracketed_paste,
            },
        })
    }

    pub async fn write(&self, data: Vec<u8>) -> AttachmentWrite {
        if let Some(error) = self.local_error(data.len()) {
            return AttachmentWrite {
                bytes_written: 0,
                error: Some(error),
                cancelled: false,
            };
        }
        let (reply, reply_rx) = oneshot::channel();
        if self
            .control
            .send(LeaseRequest::Write {
                token: self.token,
                data,
                reply,
            })
            .await
            .is_err()
        {
            return AttachmentWrite {
                bytes_written: 0,
                error: Some("terminal driver ended before the write was queued".into()),
                cancelled: false,
            };
        }
        match reply_rx.await {
            Ok(ack) => AttachmentWrite {
                bytes_written: ack.bytes_written,
                error: ack.error,
                cancelled: ack.cancelled,
            },
            Err(_) => AttachmentWrite {
                bytes_written: 0,
                error: Some("terminal driver ended without acknowledging the write".into()),
                cancelled: false,
            },
        }
    }

    pub async fn resize(&self, rows: u16, columns: u16) -> Result<(), String> {
        if self.released.load(Ordering::Acquire) {
            return Err("attachment lease was released".into());
        }
        let dimensions = Dimensions::new(rows, columns)?;
        let (reply, reply_rx) = oneshot::channel();
        self.control
            .send(LeaseRequest::Resize {
                token: self.token,
                dimensions,
                reply,
            })
            .await
            .map_err(|_| "terminal driver ended before resize was queued".to_owned())?;
        match reply_rx.await {
            Ok(result) => result,
            Err(_) => Err("terminal driver ended without acknowledging resize".into()),
        }
    }

    /// Detach explicitly without stopping the program. Awaiting the driver's
    /// acknowledgement makes the following model write deterministic.
    pub async fn release(self) {
        if !self.released.swap(true, Ordering::AcqRel) {
            let (reply, reply_rx) = oneshot::channel();
            if self
                .control
                .send(LeaseRequest::Release {
                    token: self.token,
                    reply: Some(reply),
                })
                .await
                .is_ok()
            {
                let _ = reply_rx.await;
            }
        }
    }

    #[cfg(test)]
    pub(super) fn stale_view(&self) -> Self {
        Self {
            id: self.id,
            token: Uuid::new_v4(),
            control: self.control.clone(),
            state: self.state.clone(),
            released: AtomicBool::new(false),
        }
    }

    fn local_error(&self, bytes: usize) -> Option<String> {
        if self.released.load(Ordering::Acquire) {
            return Some("attachment lease was released".into());
        }
        if !self.is_attached() {
            return Some("attachment lease is stale".into());
        }
        if bytes > MAX_INPUT_BYTES {
            return Some(format!("write exceeds {MAX_INPUT_BYTES} bytes"));
        }
        None
    }
}

impl Drop for TerminalAttachment {
    fn drop(&mut self) {
        if !self.released.swap(true, Ordering::AcqRel) {
            // The view's explicit release is awaited. This is only a fallback
            // for early returns; a full queue leaves the lease to process exit.
            let _ = self.control.try_send(LeaseRequest::Release {
                token: self.token,
                reply: None,
            });
        }
    }
}

impl ProcessOwner {
    /// Acquire the single live input lease for one PTY handle. Scope is the
    /// caller's Session UUID and the same canonical capture directory used by
    /// the process tools; no model tool origin is manufactured.
    pub async fn attach_terminal(
        &self,
        session: Uuid,
        cwd: &Path,
        id: Uuid,
    ) -> Result<TerminalAttachment, String> {
        let cwd = match cwd.canonicalize() {
            Ok(cwd) => cwd,
            Err(error) => return Err(format!("attachment cwd is unavailable: {error}")),
        };
        let entry = {
            let entries = self.entries.lock().await;
            match entries.get(&id) {
                Some(entry) if entry.session == session && entry.cwd == cwd => entry.clone(),
                _ => return Err(super::unavailable()),
            }
        };
        if entry.state.backend != Backend::Pty {
            return Err("terminal attachment requires a PTY program".into());
        }
        let Some(control) = entry.state.lease_control() else {
            return Err("process terminal is not live".into());
        };
        let token = Uuid::new_v4();
        let (reply, reply_rx) = oneshot::channel();
        control
            .send(LeaseRequest::Acquire { token, reply })
            .await
            .map_err(|_| "process terminal is not live".to_owned())?;
        match reply_rx.await {
            Ok(Ok(())) => Ok(TerminalAttachment {
                id,
                token,
                control,
                state: entry.state,
                released: AtomicBool::new(false),
            }),
            Ok(Err(error)) => Err(error),
            Err(_) => Err("terminal driver ended before granting attachment".into()),
        }
    }

    /// PTY programs in this Session and canonical capture directory. Handle
    /// IDs are the same opaque owner UUIDs used by the model tool.
    pub async fn list_terminals(&self, session: Uuid, cwd: &Path) -> Vec<TerminalSummary> {
        let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
        let entries = self.entries.lock().await;
        entries
            .values()
            .filter(|entry| {
                entry.session == session && entry.cwd == cwd && entry.state.backend == Backend::Pty
            })
            .map(|entry| {
                let Capture::Pty { screen, .. } = &entry.state.capture else {
                    unreachable!("PTY entries own a terminal projection");
                };
                let dimensions = screen.dimensions();
                let status = entry.state.status();
                TerminalSummary {
                    id: entry.state.id,
                    rows: dimensions.rows,
                    columns: dimensions.columns,
                    attached: entry.state.lease_owner().is_some(),
                    running: status.started && status.exit.is_none() && !status.joined,
                }
            })
            .collect()
    }
}

/// A lease write is an ordinary queued input request whose stop token belongs
/// to the attached view, so existing prefix acknowledgement stays authoritative.
pub(super) fn attachment_input(
    data: Vec<u8>,
    reply: oneshot::Sender<super::driver::InputAck>,
) -> InputRequest {
    InputRequest {
        data: Some(data),
        stop: CancellationToken::new(),
        reply,
    }
}
