//! One resident committed page, with source-relative row navigation.
use std::{collections::BTreeMap, future::Future, pin::Pin};

use crate::transcript_detail::DetailView;

use ion_core::{
    Session, SessionError, TranscriptCursor, TranscriptItem, TranscriptPage, TranscriptProjection,
    TranscriptSource,
};

use super::{block_presenter, conversation};

#[derive(Clone, Copy)]
enum Load {
    Latest,
    Older(TranscriptCursor),
    Newer(TranscriptCursor),
}

type Read<T> = Pin<Box<dyn Future<Output = Result<Option<T>, SessionError>> + Send>>;

enum Inspection {
    Page,
    Requested {
        cursor: TranscriptCursor,
        source: TranscriptSource,
    },
    Reading {
        source: TranscriptSource,
        read: Read<TranscriptProjection>,
    },
    Ready(TranscriptProjection),
    Failed(&'static str),
}

pub(super) enum Completion {
    Current,
    LoadedPage { at_start: bool },
    Stale,
    Expanded(TranscriptSource),
}

#[derive(Default)]
enum Purpose {
    #[default]
    Transcript,
    Archive {
        restore: Option<Box<DetailView>>,
    },
}

#[derive(Default)]
pub(super) struct HistoryViewport {
    pub(super) view: block_presenter::ConversationView,
    pub(super) presenter: block_presenter::BlockPresenter,
    pub(super) page: Option<conversation::Conversation>,
    pub(super) page_rows: u16,
    pub(super) tool_starts: BTreeMap<u64, usize>,
    purpose: Purpose,
    read: Option<(Load, u64, Read<TranscriptPage>)>,
    inspection: Option<Inspection>,
    requested: Option<Load>,
    current: Option<TranscriptCursor>,
    older: Option<TranscriptCursor>,
    newer: Vec<TranscriptCursor>,
    latest_revision: Option<u64>,
    error: bool,
}

impl HistoryViewport {
    pub(super) fn archive() -> Self {
        Self {
            purpose: Purpose::Archive { restore: None },
            ..Self::default()
        }
    }

    pub(super) fn is_archive(&self) -> bool {
        matches!(self.purpose, Purpose::Archive { .. })
    }

    pub(super) fn page_first_source(&self) -> Option<TranscriptSource> {
        self.page.as_ref()?.items().find_map(TranscriptItem::source)
    }

    pub(super) fn turn_archive_page(&mut self, later: bool) {
        if !self.pending() && self.requested.is_none() && self.inspection.is_none() {
            self.requested = if later {
                self.newer.last().copied().map(Load::Newer)
            } else {
                self.older.map(Load::Older)
            };
        }
    }

    pub(super) fn expand_archive(&mut self, view: &mut DetailView) {
        if self.pending() || self.requested.is_some() || self.inspection.is_some() {
            return;
        }
        let Some(source) = view.top_source() else {
            return;
        };
        let Some(request) = self.expansion(Some(source)) else {
            return;
        };
        let Purpose::Archive { restore } = &mut self.purpose else {
            return;
        };
        *restore = Some(Box::new(std::mem::replace(
            view,
            DetailView::at_source(source),
        )));
        self.inspection = Some(request);
    }

    pub(super) fn restore_archive(&mut self) -> Option<DetailView> {
        let Purpose::Archive { restore } = &mut self.purpose else {
            return None;
        };
        let view = *restore.take()?;
        self.inspection = None;
        Some(view)
    }

    pub(super) fn archive_at_page(&self) -> bool {
        self.is_archive() && self.inspection.is_none()
    }

    pub(super) fn scroll(&mut self, delta: isize) {
        if self.read.is_some() || self.requested.is_some() {
            return;
        }
        let crossing = if delta < 0 && self.view.at_start() {
            self.older.map(Load::Older)
        } else if delta > 0 && self.view.at_end() {
            self.newer.last().copied().map(Load::Newer)
        } else {
            None
        };
        if let Some(load) = crossing {
            self.requested = Some(load);
        } else {
            self.view.scroll(delta);
        }
    }

    pub(super) fn follow_end(&mut self) {
        if !matches!(self.read, Some((Load::Latest, _, _))) {
            self.read = None;
            self.requested = Some(Load::Latest);
        }
        self.error = false;
    }

    pub(super) fn open_inspection(&mut self) -> Option<TranscriptSource> {
        self.read = None;
        self.requested = None;
        let source = self.view.top_source();
        self.inspection = Some(self.expansion(source).unwrap_or(Inspection::Page));
        source
    }

    fn expansion(&self, source: Option<TranscriptSource>) -> Option<Inspection> {
        match (self.current, source, &self.page) {
            (Some(cursor), Some(source), Some(page)) if page.items().any(|item| {
                matches!(item, TranscriptItem::Oversized { source: omitted } if *omitted == source)
            }) => Some(Inspection::Requested { cursor, source }),
            _ => None,
        }
    }

    pub(super) fn close_inspection(&mut self) {
        self.inspection = None;
    }

    pub(super) fn inspection_items(&self) -> impl Iterator<Item = &TranscriptItem> {
        let expanded = match &self.inspection {
            Some(Inspection::Ready(projection)) => Some(projection.items.as_slice()),
            _ => None,
        };
        self.page
            .iter()
            .filter(move |_| expanded.is_none())
            .flat_map(|page| page.items())
            .chain(expanded.into_iter().flatten())
    }

    pub(super) fn inspection_status(&self) -> Option<&'static str> {
        match &self.inspection {
            Some(Inspection::Requested { .. } | Inspection::Reading { .. }) => {
                Some("Loading committed evidence…")
            }
            Some(Inspection::Failed(status)) => Some(status),
            None if self.is_archive() && self.error => {
                Some("History read failed · reopen /tool all to retry")
            }
            None if self.is_archive() && self.pending() => Some("Loading committed history…"),
            _ => None,
        }
    }

    pub(super) fn start(&mut self, session: &std::sync::Arc<Session>, revision: u64, busy: bool) {
        if let Some(inspection) = &mut self.inspection {
            if let Inspection::Requested { cursor, source } = *inspection {
                *inspection = match session.reader() {
                    Ok(reader) => Inspection::Reading {
                        source,
                        read: Box::pin(
                            async move { reader.transcript_evidence(cursor, source).await },
                        ),
                    },
                    Err(_) => {
                        Inspection::Failed("Could not read committed evidence; close and retry")
                    }
                };
            }
            return;
        }
        if self.read.is_some() {
            return;
        }
        let load = self.requested.take().or_else(|| {
            (!self.error
                && (!self.is_archive() || self.current.is_none())
                && self.newer.is_empty()
                && !self.view.is_reading()
                && (self.page.is_none() || !busy)
                && self.latest_revision != Some(revision))
            .then_some(Load::Latest)
        });
        let Some(load) = load else { return };
        let cursor = match load {
            Load::Latest => None,
            Load::Older(cursor) | Load::Newer(cursor) => Some(cursor),
        };
        match session.reader() {
            Ok(reader) => {
                self.read = Some((
                    load,
                    revision,
                    Box::pin(async move { reader.transcript_page(cursor).await }),
                ));
            }
            Err(_) => self.error = true,
        }
    }

    pub(super) fn pending(&self) -> bool {
        self.read.is_some() || matches!(self.inspection, Some(Inspection::Reading { .. }))
    }

    pub(super) fn status(&self) -> &'static str {
        if self.error {
            " · read failed; End retries"
        } else if self.pending() {
            " · loading"
        } else if self.older.is_some() {
            " · older history"
        } else {
            ""
        }
    }

    pub(super) async fn complete(&mut self) -> Completion {
        if let Some(Inspection::Reading { source, read }) = &mut self.inspection {
            let source = *source;
            return match read.as_mut().await {
                Ok(Some(projection)) => {
                    self.inspection = Some(Inspection::Ready(projection));
                    Completion::Expanded(source)
                }
                Ok(None) => Completion::Stale,
                Err(error) => {
                    self.inspection = Some(Inspection::Failed(match error {
                        SessionError::EntryTooLarge => {
                            "Evidence exceeds inspection limit; use /export"
                        }
                        _ => "Could not read committed evidence; close and retry",
                    }));
                    Completion::Current
                }
            };
        }
        let (load, revision, read) = self.read.as_mut().expect("pending history page");
        let result = read.as_mut().await;
        let (load, revision) = (*load, *revision);
        self.read = None;
        let page = match result {
            Ok(Some(page)) => page,
            Ok(None) => return Completion::Stale,
            Err(_) => {
                self.error = true;
                return Completion::Current;
            }
        };
        self.error = false;
        match load {
            Load::Latest => {
                self.newer.clear();
                self.latest_revision = Some(revision);
            }
            Load::Older(_) => self
                .newer
                .push(self.current.expect("older cursor came from a page")),
            Load::Newer(_) => {
                self.newer.pop();
            }
        }
        self.current = Some(page.cursor);
        self.older = page.older;
        self.tool_starts = page.tool_starts;
        self.page = Some(conversation::Conversation::history_page(page.projection));
        self.presenter = block_presenter::BlockPresenter::default();
        self.view = block_presenter::ConversationView::default();
        let at_start = matches!(load, Load::Newer(_));
        if at_start {
            self.view.start_at_top();
        }
        Completion::LoadedPage { at_start }
    }
}
