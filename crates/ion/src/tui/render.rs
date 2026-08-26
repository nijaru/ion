use super::*;

/// Colors for the live viewport, chosen once from the theme setting.
/// Scrollback styling stays theme-independent (dim/red/yellow read on
/// both light and dark terminals).
#[derive(Clone, Copy)]
pub struct Palette {
    pub status_idle: Style,
    pub status_working: Style,
    pub tool_row: Style,
    pub composer: Style,
}

/// `Auto` follows the terminal preference, which has no portable query
/// in crossterm; it currently resolves to the dark palette.
pub fn palette(theme: Theme) -> Palette {
    match theme {
        Theme::Dark | Theme::Auto => Palette {
            status_idle: Style::new().dim(),
            status_working: Style::new().cyan(),
            tool_row: Style::new().dim(),
            composer: Style::new(),
        },
        Theme::Light => Palette {
            status_idle: Style::new().dark_gray(),
            status_working: Style::new().blue(),
            tool_row: Style::new().dark_gray(),
            composer: Style::new(),
        },
    }
}

/// Convert any borrowed Line to an owned `'static` one, folding the
/// line-level style into each span so wrapping cannot discard it.
fn clone_static(line: &Line<'_>) -> Line<'static> {
    Line::from(
        line.spans
            .iter()
            .map(|s| Span::styled(s.content.to_string(), line.style.patch(s.style)))
            .collect::<Vec<_>>(),
    )
}

/// Wrap one styled line to `width` columns (display width, char
/// boundaries). Styles carry over to the continuation rows.
pub(super) fn wrap_line(line: &Line<'_>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let total: usize = line.spans.iter().map(|s| s.content.width()).sum();
    if total <= width {
        return vec![clone_static(line)];
    }
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut cur: Vec<Span> = Vec::new();
    let mut cur_width = 0usize;
    for span in &line.spans {
        let style = span.style;
        let mut chunk = String::new();
        for grapheme in span.content.graphemes(true) {
            let cw = grapheme.width();
            if cur_width + cw > width && cur_width > 0 {
                if !chunk.is_empty() {
                    cur.push(Span::styled(std::mem::take(&mut chunk), style));
                }
                rows.push(Line::from(std::mem::take(&mut cur)));
                cur_width = 0;
            }
            chunk.push_str(grapheme);
            cur_width += cw;
        }
        if !chunk.is_empty() {
            cur.push(Span::styled(chunk, style));
        }
    }
    if !cur.is_empty() {
        rows.push(Line::from(cur));
    }
    if rows.is_empty() {
        rows.push(Line::from(String::new()));
    }
    rows
}

fn str_width(line: &Line<'_>) -> usize {
    line.spans.iter().map(|s| s.content.width()).sum()
}

/// Maximum live-band height: tool row(s), draft tail, status,
/// composer. The band is variable-height within this cap; growth
/// beyond the window scrolls physically (monotonic offset), shrink
/// blanks freed rows in place — reversible edits never duplicate
/// committed content into scrollback (TERMINAL.md, inline-first).
const LIVE_REGION_MAX_ROWS: usize = 6;

/// The live band below the committed transcript. Returns pre-wrapped
/// rows (at most LIVE_REGION_MAX_ROWS) plus the hardware cursor
/// position relative to the band; the composer occupies the last rows.
pub(super) fn build_live(
    state: &UiState,
    palette: &Palette,
    width: usize,
) -> (Vec<Line<'static>>, Option<(usize, u16)>) {
    // Composer first: it is anchored to the band's bottom and owns the
    // hardware cursor.
    let cursor_byte = char_offset_to_byte(&state.composer, state.cursor);
    let before = &state.composer[..cursor_byte];
    let after = &state.composer[cursor_byte..];
    let prompt = "\u{203a} ";
    let target_col = prompt.width() + before.width();
    let composer = Line::from(format!("{prompt}{before}{after}"));
    let composer_rows = wrap_line(&composer, width);
    let composer_len = composer_rows.len();

    let mut head: Vec<Line<'static>> = Vec::new();
    if let Some(latest) = state.tool_rows.last() {
        head.extend(wrap_line(
            &Line::from(latest.label.clone()).style(palette.tool_row),
            width,
        ));
        if state.tool_output_expanded {
            for line in latest.preview.iter().flat_map(|p| p.lines()) {
                head.extend(wrap_line(
                    &Line::from(format!("  {line}"))
                        .style(palette.tool_row)
                        .italic(),
                    width,
                ));
            }
        }
    }
    if !state.draft.is_empty() {
        head.extend(wrap_line(
            &Line::from(format!("ion \u{ab} {}", state.draft)),
            width,
        ));
    } else if let Some(last) = state
        .draft_thinking
        .lines()
        .last()
        .filter(|_| state.thinking_visible && !state.draft_thinking.is_empty())
    {
        head.extend(wrap_line(
            &Line::from(format!("\u{273b} {last}")).dim().italic(),
            width,
        ));
    }

    let status = match &state.status {
        UiStatus::Idle => {
            let mut text = String::from("idle \u{2014} type a prompt, esc quits");
            if let Some(model) = &state.model_name {
                text.push_str("  \u{b7}  ");
                text.push_str(model);
            }
            Line::from(text).style(palette.status_idle)
        }
        UiStatus::Working { operation } => {
            Line::from(format!("\u{25cf} {operation}")).style(palette.status_working)
        }
    };
    head.push(status);

    // Fit the head above the composer inside the band cap, keeping
    // the newest content when truncating.
    let budget = LIVE_REGION_MAX_ROWS.saturating_sub(composer_len);
    if head.len() > budget {
        head = head.split_off(head.len() - budget);
    }

    let mut lines: Vec<Line<'static>> = head;

    // Cursor position within the wrapped composer rows.
    let mut cursor = None;
    let mut walked = 0usize;
    for (i, row) in composer_rows.iter().enumerate() {
        let row_width = str_width(row);
        if target_col <= walked + row_width {
            cursor = Some((
                lines.len() + i,
                (target_col - walked).min(width.saturating_sub(1)) as u16,
            ));
            break;
        }
        walked += row_width;
    }
    lines.extend(composer_rows);

    (lines, cursor)
}

/// Committed transcript with its wrapped projection cached per width:
/// appending wraps only new lines; a resize rewraps once.
pub(super) struct Transcript {
    pub(super) raw: Vec<Line<'static>>,
    pub(super) wrapped: Vec<Line<'static>>,
    width: u16,
}

impl Transcript {
    pub(super) fn new(width: u16) -> Self {
        Self {
            raw: Vec::new(),
            wrapped: Vec::new(),
            width,
        }
    }

    fn push(&mut self, line: Line<'static>) {
        self.wrapped.extend(wrap_line(&line, self.width as usize));
        self.raw.push(line);
    }

    pub(super) fn extend(&mut self, lines: impl IntoIterator<Item = Line<'static>>) {
        for line in lines {
            self.push(line);
        }
    }

    pub(super) fn clear(&mut self) {
        self.raw.clear();
        self.wrapped.clear();
    }

    pub(super) fn rewrap_if_needed(&mut self, width: u16) {
        if width == self.width {
            return;
        }
        self.width = width;
        self.wrapped.clear();
        for line in &self.raw {
            self.wrapped.extend(wrap_line(line, width as usize));
        }
    }
}

pub(super) fn append_snapshot_entries(
    transcript: &mut Transcript,
    entries: &[ion_core::SessionEntry],
    resume_entry_count: usize,
    resume_session: Option<ion_core::SessionId>,
) {
    let boundary = if resume_session.is_some() {
        resume_entry_count.min(entries.len())
    } else {
        0
    };
    transcript.extend(entry_lines(&entries[..boundary]));
    if let Some(session_id) = resume_session {
        transcript.push(Line::from(format!("— resumed session {session_id} —")).dim());
    }
    transcript.extend(entry_lines(&entries[boundary..]));
}

pub(super) fn entry_lines(entries: &[ion_core::SessionEntry]) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for entry in entries {
        push_entry_lines(entry, &mut out);
    }
    out
}

fn push_entry_lines(entry: &ion_core::SessionEntry, out: &mut Vec<Line<'static>>) {
    let line = match entry {
        ion_core::SessionEntry::UserMessage { text } => Some(format!("you » {text}")),
        ion_core::SessionEntry::ModelChanged { model_ref } => {
            Some(format!("· model → {model_ref}"))
        }
        ion_core::SessionEntry::AssistantMessage { text } => Some(format!("ion « {text}")),
        ion_core::SessionEntry::ToolCall { call } => {
            let target = ion_core::target_from_arguments(&call.name, &call.arguments)
                .unwrap_or_else(|| format!("(call {})", call.call_id));
            Some(format!("· {} {target}…", call.name))
        }
        ion_core::SessionEntry::ToolResult { result } => Some(if result.is_ok() {
            format!("  = {}", result.model_text())
        } else {
            format!("  ! {}", result.model_text())
        }),
        ion_core::SessionEntry::Compaction { summary, .. } => {
            Some(format!("≡ compacted: {summary}"))
        }
    };
    if let Some(line) = line {
        for logical_line in line.split('\n') {
            out.push(Line::from(logical_line.to_owned()));
        }
    }
}
