//! Composer inspection has its own source. Committed snapshots are opened on
//! demand; the live source stays attached to in-flight activity.
use std::{num::NonZeroUsize, path::Path};

use anyhow::Result;
use ion_app::SessionControl;
use ion_core::{
    Session, SessionError, ToolEvidence, ToolEvidenceCursor, ToolEvidenceTarget,
    TranscriptActivityIdentity, TranscriptItem, TranscriptProjection,
};

use super::{EditorDraft, Frontend, Picker, PickerItem, PickerValue, session_io::ReadOutput};
use crate::{
    transcript_detail::{DetailView, result_contains, tools},
    transcript_render::{activity_subject, state_label},
};

pub(super) struct Inspection {
    pub(super) view: DetailView,
    pub(super) source: Source,
}

pub(super) enum Source {
    Live,
    HistoryPage,
    Committed(TranscriptProjection),
    Recorded(Box<ToolEvidence>),
}

impl Inspection {
    pub(super) fn live(view: DetailView) -> Self {
        Self {
            view,
            source: Source::Live,
        }
    }

    pub(super) fn page(view: DetailView) -> Self {
        Self {
            view,
            source: Source::HistoryPage,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Position {
    cursor: ToolEvidenceCursor,
    number: NonZeroUsize,
}

impl Inspection {
    pub(super) fn position(&self) -> Option<Position> {
        match &self.source {
            Source::Recorded(tool) => Some(Position {
                cursor: tool.cursor,
                number: tool.number,
            }),
            _ => None,
        }
    }
}

pub(super) enum Select {
    Conversation,
    Latest,
    Number(usize),
    Identity(TranscriptActivityIdentity),
}

impl Select {
    pub(super) fn view<'a>(
        self,
        items: impl Iterator<Item = &'a TranscriptItem>,
    ) -> Result<DetailView, &'static str> {
        let candidates = tools(items);
        let selection = match self {
            Self::Conversation => return Ok(DetailView::new(None)),
            Self::Latest => candidates.enumerate().last(),
            Self::Number(number) => number
                .checked_sub(1)
                .and_then(|index| candidates.enumerate().nth(index)),
            Self::Identity(identity) => candidates
                .enumerate()
                .find(|(_, activity)| activity.identity == identity),
        };
        let (index, activity) =
            selection.ok_or("Tool result not found; use /tools to list tools")?;
        Ok(DetailView::new(Some((
            NonZeroUsize::new(index + 1).expect("one-based tool number"),
            activity.identity,
        ))))
    }
}

pub(super) fn open(
    control: &SessionControl<EditorDraft>,
    ui: &mut Frontend,
    selection: Select,
) -> Result<()> {
    if control.is_busy() {
        ui.open_live_details(selection);
        Ok(())
    } else {
        open_committed(control.session(), ui, selection)
    }
}

pub(super) fn open_committed(
    session: &std::sync::Arc<Session>,
    ui: &mut Frontend,
    selection: Select,
) -> Result<()> {
    let target = match selection {
        Select::Conversation => {
            let reader = session.reader()?;
            return ui.session_io.start(session, async move {
                Ok(reader
                    .map_presentation(None, |snapshot| ReadOutput::Inspection {
                        inspection: Box::new(Inspection {
                            view: DetailView::new(None),
                            source: Source::Committed(snapshot.transcript),
                        }),
                        from: None,
                    })
                    .await?)
            });
        }
        Select::Latest => Some(ToolEvidenceTarget::Latest),
        Select::Number(number) => NonZeroUsize::new(number).map(ToolEvidenceTarget::Number),
        Select::Identity(TranscriptActivityIdentity::Tool(occurrence)) => {
            Some(ToolEvidenceTarget::Occurrence(occurrence))
        }
        Select::Identity(TranscriptActivityIdentity::Child { .. }) => None,
    };
    if let Some(target) = target {
        read_tool(ui, session, target, None)
    } else {
        ui.status = "Tool result not found; use /tools to list tools".into();
        Ok(())
    }
}

pub(super) fn adjacent(
    ui: &mut Frontend,
    session: &std::sync::Arc<Session>,
    later: bool,
) -> Result<()> {
    let Some(inspection) = &ui.details else {
        return Ok(());
    };
    let Source::Recorded(tool) = &inspection.source else {
        return Ok(());
    };
    let neighbor = if later {
        tool.number
            .get()
            .checked_add(1)
            .filter(|number| *number <= tool.total)
    } else {
        tool.number.get().checked_sub(1)
    };
    let Some(neighbor) = neighbor.and_then(NonZeroUsize::new) else {
        return Ok(());
    };
    let from = inspection.position();
    read_tool(ui, session, ToolEvidenceTarget::Number(neighbor), from)
}

fn read_tool(
    ui: &mut Frontend,
    session: &std::sync::Arc<Session>,
    target: ToolEvidenceTarget,
    from: Option<Position>,
) -> Result<()> {
    let reader = session.reader()?;
    ui.session_io.start(session, async move {
        let tool = match reader
            .tool_evidence(from.map(|pos| pos.cursor), target)
            .await
        {
            Ok(Some(tool)) => tool,
            Ok(None) => {
                return Ok(ReadOutput::Status(
                    "Tool result not found; use /tools to list tools".into(),
                ));
            }
            Err(SessionError::EntryTooLarge) => {
                return Ok(ReadOutput::Status(
                    "Tool evidence exceeds inspection limit; use /export".into(),
                ));
            }
            Err(error) => return Err(error.into()),
        };
        Ok(ReadOutput::Inspection {
            inspection: Box::new(Inspection {
                view: DetailView::new(Some((tool.number, tool.activity.identity))),
                source: Source::Recorded(Box::new(tool)),
            }),
            from,
        })
    })
}

pub(super) fn list(
    control: &SessionControl<EditorDraft>,
    ui: &mut Frontend,
    query: Option<String>,
) -> Result<()> {
    if control.is_busy() {
        let output = picker(ui.conversation.items(), &ui.cwd, query.as_deref(), false);
        match output {
            Ok(picker) => ui.pickers.open(picker),
            Err(status) => ui.status = status.into(),
        }
        return Ok(());
    }
    let session = control.session();
    let reader = session.reader()?;
    let cwd = ui.cwd.clone();
    ui.session_io.start(session, async move {
        Ok(reader
            .map_presentation(None, move |snapshot| {
                match picker(
                    snapshot.transcript.items.iter(),
                    &cwd,
                    query.as_deref(),
                    true,
                ) {
                    Ok(picker) => ReadOutput::Picker(Box::new(picker)),
                    Err(status) => ReadOutput::Status(status.into()),
                }
            })
            .await?)
    })
}

fn picker<'a>(
    items: impl Iterator<Item = &'a TranscriptItem>,
    cwd: &Path,
    query: Option<&str>,
    committed: bool,
) -> Result<Picker, &'static str> {
    let mut items: Vec<_> = tools(items)
        .enumerate()
        .filter(|(_, activity)| {
            query.is_none_or(|query| {
                activity
                    .result
                    .as_ref()
                    .is_some_and(|result| result_contains(&result.value, query))
            })
        })
        .map(|(index, activity)| {
            let mut label = format!(
                "{} · {} · {}",
                index + 1,
                activity.name,
                state_label(activity.state)
            );
            if let Some(subject) = activity_subject(activity, cwd) {
                label.push_str(" · ");
                label.push_str(&crate::display_text::fit_line(&subject, 200));
            }
            PickerItem {
                label,
                detail: None,
                value: if committed {
                    PickerValue::CommittedTool(activity.identity)
                } else {
                    PickerValue::LiveTool(activity.identity)
                },
            }
        })
        .collect();
    if items.is_empty() {
        return Err(if query.is_some() {
            "No matching recorded tool output (raw captures not searched)"
        } else {
            "No tools in this session"
        });
    }
    items.reverse();
    Ok(Picker {
        title: if query.is_some() {
            "Recorded tool output"
        } else {
            "Recorded tools"
        },
        query: String::new(),
        selected: 0,
        items,
    })
}
