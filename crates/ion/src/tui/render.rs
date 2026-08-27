use super::*;

/// Colors for the live viewport, chosen once from the theme setting.
/// Scrollback styling stays theme-independent (dim/red/yellow read on
/// both light and dark terminals).
#[derive(Clone, Copy)]
pub struct Palette {
    pub status_idle: Style,
    pub status_working: Style,
    pub tool_row: Style,
    pub tool_error: Style,
    pub user_entry: Style,
    pub user_marker: Style,
    pub system_note: Style,
    pub assistant: Style,
    pub separator: Style,
    pub status_segment: Style,
    pub composer: Style,
}

/// `Auto` follows the terminal preference, which has no portable query
/// in crossterm; it currently resolves to the dark palette.
pub fn palette(theme: Theme) -> Palette {
    match theme {
        Theme::Dark | Theme::Auto => Palette {
            status_idle: Style::new().dim(),
            status_working: Style::new().cyan(),
            tool_row: Style::new().green(),
            tool_error: Style::new().red(),
            user_entry: Style::new().cyan(),
            user_marker: Style::new().cyan().bold(),
            system_note: Style::new().dim(),
            assistant: Style::new(),
            separator: Style::new().dark_gray(),
            status_segment: Style::new().dim(),
            composer: Style::new(),
        },
        Theme::Light => Palette {
            status_idle: Style::new().dark_gray(),
            status_working: Style::new().blue(),
            tool_row: Style::new().green(),
            tool_error: Style::new().red(),
            user_entry: Style::new().blue(),
            user_marker: Style::new().blue().bold(),
            system_note: Style::new().dark_gray(),
            assistant: Style::new(),
            separator: Style::new().dark_gray(),
            status_segment: Style::new().dark_gray(),
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
    // Flatten to (grapheme, style) so a break decision can look back for
    // a word boundary without per-span bookkeeping.
    let mut items: Vec<(String, ratatui::style::Style)> = Vec::new();
    for span in &line.spans {
        for grapheme in span.content.graphemes(true) {
            items.push((grapheme.to_owned(), span.style));
        }
    }
    let mut rows: Vec<Line<'static>> = Vec::new();
    while !items.is_empty() {
        // Greedy fill; prefer breaking after the last space when the
        // row overflows so words are not split mid-token.
        let mut taken_width = 0usize;
        let mut take = 0usize;
        let mut last_space: Option<usize> = None;
        for (i, (grapheme, _)) in items.iter().enumerate() {
            let gw = grapheme.width();
            if taken_width + gw > width {
                break;
            }
            taken_width += gw;
            take = i + 1;
            if grapheme == " " && i > 0 {
                last_space = Some(take);
            }
        }
        // Do not backtrack to a space when the word being cut is itself
        // wider than a line; hard-breaking through it is the only sane
        // layout (otherwise every row would break early at the prefix).
        if last_space.is_some() && take < items.len() {
            let mut rest = take;
            while matches!(items.get(rest), Some((g, _)) if g == " ") {
                rest += 1;
            }
            let mut word_width = 0usize;
            while matches!(items.get(rest), Some((g, _)) if g != " ") {
                word_width += items[rest].0.width();
                rest += 1;
            }
            if word_width >= width {
                last_space = None;
            }
        }
        let mut row_items: Vec<(String, ratatui::style::Style)> = if take == items.len() {
            std::mem::take(&mut items)
        } else if let Some(break_at) = last_space {
            items.drain(..break_at).collect()
        } else {
            // Unbreakable overflow (long path/URL): hard-break.
            items.drain(..take.max(1)).collect()
        };
        while row_items.last().map(|(g, _)| g == " ").unwrap_or(false) {
            row_items.pop();
        }
        let mut spans: Vec<Span> = Vec::new();
        for (text, style) in row_items {
            match spans.last_mut() {
                Some(last) if last.style == style => {
                    let mut owned = last.content.to_string();
                    owned.push_str(&text);
                    last.content = std::borrow::Cow::Owned(owned);
                }
                _ => spans.push(Span::styled(text, style)),
            }
        }
        rows.push(Line::from(spans));
    }
    if rows.is_empty() {
        rows.push(Line::from(String::new()));
    }
    rows
}

fn str_width(line: &Line<'_>) -> usize {
    line.spans.iter().map(|s| s.content.width()).sum()
}

fn wrapped_cursor_position(
    logical_line: &str,
    prefix: &str,
    wrapped: &[Line<'_>],
    local_cursor: usize,
) -> (usize, usize) {
    let cursor_byte = char_offset_to_byte(logical_line, local_cursor);
    let target_col = prefix.width() + logical_line[..cursor_byte].width();
    let mut walked = 0;
    for (row_index, row) in wrapped.iter().enumerate() {
        let row_width = str_width(row);
        if target_col <= walked + row_width {
            return (row_index, target_col - walked);
        }
        walked += row_width;
    }
    (wrapped.len().saturating_sub(1), walked)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ComposerCursorPosition {
    pub(super) cursor: usize,
    pub(super) row: usize,
    /// Display column within the logical content, excluding the prompt.
    pub(super) column: usize,
}

/// Map every composer cursor boundary to the wrapped row and display column
/// used by vertical movement. This deliberately uses the same wrapping and
/// cursor mapping as `composer_rows` so navigation cannot drift from paint.
pub(super) fn composer_cursor_positions(
    composer: &str,
    width: usize,
) -> Vec<ComposerCursorPosition> {
    let width = width.max(1);
    let mut positions = Vec::new();
    let mut text_offset = 0;
    let mut visual_offset = 0;
    for (line_index, logical_line) in composer.split('\n').enumerate() {
        let prefix = if line_index == 0 { "> " } else { "  " };
        let wrapped = wrap_line(&Line::from(format!("{prefix}{logical_line}")), width);
        let line_length = logical_line.chars().count();
        for local_cursor in 0..=line_length {
            let (row, screen_column) =
                wrapped_cursor_position(logical_line, prefix, &wrapped, local_cursor);
            positions.push(ComposerCursorPosition {
                cursor: text_offset + local_cursor,
                row: visual_offset + row,
                column: if row == 0 {
                    screen_column.saturating_sub(prefix.width())
                } else {
                    screen_column
                },
            });
        }
        text_offset += line_length + 1;
        visual_offset += wrapped.len();
    }
    positions
}

/// Wrap the multiline composer and locate its cursor by logical line. The
/// prompt is shown once and continuation lines align beneath the draft.
fn composer_rows(state: &UiState, width: usize) -> (Vec<Line<'static>>, Option<(usize, u16)>) {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut cursor = None;
    let mut text_offset = 0;
    for (line_index, logical_line) in state.composer.split('\n').enumerate() {
        let prefix = if line_index == 0 { "> " } else { "  " };
        let wrapped = wrap_line(&Line::from(format!("{prefix}{logical_line}")), width);
        let line_length = logical_line.chars().count();
        if state.cursor >= text_offset && state.cursor <= text_offset + line_length {
            let local_cursor = state.cursor - text_offset;
            let (row_index, screen_column) =
                wrapped_cursor_position(logical_line, prefix, &wrapped, local_cursor);
            cursor = Some((
                rows.len() + row_index,
                screen_column.min(width.saturating_sub(1)) as u16,
            ));
        }
        rows.extend(wrapped);
        text_offset += line_length + 1;
    }
    (rows, cursor)
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
    let (composer_rows, composer_cursor) = composer_rows(state, width);
    let composer_len = composer_rows.len();

    let mut head: Vec<Line<'static>> = Vec::new();
    if state.hotkeys_visible {
        head.extend(help::hotkey_lines(&state.keymap, palette));
    }
    if let Some(latest) = state.tool_rows.last() {
        let preview: Option<Vec<&str>> = state.tool_output_expanded.then(|| {
            latest
                .progress
                .as_deref()
                .or(latest.preview.as_deref())
                .into_iter()
                .flat_map(str::lines)
                .collect()
        });
        let rendered = tool_row_line(
            &latest.tool,
            latest.target.as_deref(),
            latest.state,
            preview.as_deref(),
        );
        for line in wrap_line(&rendered, width) {
            head.push(line);
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

    // The parked approval prompt sits at the band bottom, right above
    // the composer, and owns the keyboard while visible (§17.4).
    if let Some(prompt) = &state.approval {
        let mut spans = vec![Span::styled(
            format!("\u{26a0} approve `{}`", prompt.tool),
            Style::new().yellow().bold(),
        )];
        if let Some(target) = &prompt.target {
            spans.push(Span::styled(format!(" {target}"), Style::new().yellow()));
        }
        spans.push(Span::styled(
            "  y approve \u{00b7} n deny",
            Style::new().yellow(),
        ));
        head.extend(wrap_line(&Line::from(spans), width));
    }

    // Fit the head above the composer inside the band cap, keeping
    // the newest content when truncating.
    let budget = LIVE_REGION_MAX_ROWS.saturating_sub(composer_len);
    if head.len() > budget {
        head = head.split_off(head.len() - budget);
    }

    // Footer (pi parity): line 1 is `dir (branch)` plus the live
    // progress; line 2 is the provider/model label, right-aligned.
    let footer_hint = state.hint.clone();
    let mut footer_left = match (&state.cwd_label, &state.branch) {
        (Some(dir), Some(branch)) => format!("{dir} ({branch})"),
        (Some(dir), None) => dir.clone(),
        (None, Some(branch)) => format!("({branch})"),
        (None, None) => String::new(),
    };
    if let Some(hint) = &footer_hint {
        footer_left = hint.clone();
    } else if let UiStatus::Working { operation } = &state.status {
        if !footer_left.is_empty() {
            footer_left.push_str("  ");
        }
        footer_left.push_str(&format!("\u{25cf} {operation}"));
    }
    let provider_model = match &state.model_name {
        Some(model) => match &state.model_provider {
            Some(provider) => format!("({provider}) {model}"),
            None => model.clone(),
        },
        None => "(scripted)".to_owned(),
    };
    let usage_label = state.usage.map(|usage| {
        let context = usage
            .input
            .saturating_add(usage.output)
            .saturating_add(usage.cache_read)
            .saturating_add(usage.cache_write);
        format!(
            "ctx {context} · in {} · out {} · cache {}/{}",
            usage.input, usage.output, usage.cache_read, usage.cache_write
        )
    });

    let mut lines: Vec<Line<'static>> = std::mem::take(&mut head);

    // Shell chrome (Go parity): a dim rule above the composer and one
    // between the composer and the status line.
    lines.push(separator_line(width, palette));

    // Cursor position within the wrapped composer rows.
    let cursor = composer_cursor.map(|(row, column)| (lines.len() + row, column));
    lines.extend(composer_rows);

    lines.push(separator_line(width, palette));
    let footer_style = if footer_hint.is_some() {
        Style::new().yellow()
    } else {
        palette.status_segment
    };
    lines.push(Line::from(format!(" {footer_left}")).style(footer_style));

    // Right-align provider/model while keeping usage on the left. The
    // assembled line ends at the same column as the rule (width - 1).
    let content_width = width.saturating_sub(1);
    let usage_prefix = usage_label.as_deref().unwrap_or("");
    let left_width = usage_prefix.width() + usize::from(!usage_prefix.is_empty()) + 1;
    let pad = content_width
        .saturating_sub(left_width)
        .saturating_sub(provider_model.width())
        .saturating_sub(1);
    let usage_text = if usage_prefix.is_empty() {
        " ".to_owned()
    } else {
        format!(" {usage_prefix} ")
    };
    lines.push(
        Line::from(format!("{usage_text}{}{}", " ".repeat(pad), provider_model))
            .style(palette.status_segment),
    );

    (lines, cursor)
}

/// One tool line (pi style): bold tool name, dim target, red failure
/// marker; `preview_lines` renders the expanded output block dim.
/// One tool line: mid-size dot colored by state (yellow running,
/// green success, red error), bold tool name, dim target; expanded
/// preview lines render dim.
pub(super) fn tool_row_line(
    row_tool: &str,
    row_target: Option<&str>,
    state: ToolState,
    preview_lines: Option<&[&str]>,
) -> Line<'static> {
    let (marker, dot_style) = match state {
        ToolState::Running => (String::new(), Style::new()),
        ToolState::Ok => (String::new(), Style::new().green()),
        ToolState::Error => ("\u{2717} ".to_owned(), Style::new().red()),
    };
    let mut spans: Vec<Span> = vec![Span::styled(format!("{marker}\u{2022} "), dot_style)];
    spans.push(Span::styled(row_tool.to_owned(), Style::new().bold()));
    if let Some(target) = row_target {
        spans.push(Span::styled(format!(" {target}"), Style::new().dim()));
    }
    if let Some(lines) = preview_lines {
        for line in lines {
            spans.push(Span::raw("\n"));
            spans.push(Span::styled(format!("  {line}"), Style::new().dim()));
        }
    }
    Line::from(spans)
}

/// Dim full-width rule used as shell chrome above/below the live band.
pub(super) fn separator_line(width: usize, palette: &Palette) -> Line<'static> {
    Line::from("\u{2500}".repeat(width.saturating_sub(1).max(1))).style(palette.separator)
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
    palette: &Palette,
) {
    let boundary = if resume_session.is_some() {
        resume_entry_count.min(entries.len())
    } else {
        0
    };
    transcript.extend(entry_lines(&entries[..boundary], palette));
    if let Some(session_id) = resume_session {
        transcript.push(
            Line::from(format!("— resumed session {session_id} —")).style(palette.system_note),
        );
    }
    transcript.extend(entry_lines(&entries[boundary..], palette));
}

pub(super) fn entry_lines(
    entries: &[ion_core::SessionEntry],
    palette: &Palette,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for entry in entries {
        push_entry_lines(entry, &mut out, palette);
    }
    out
}

fn push_entry_lines(
    entry: &ion_core::SessionEntry,
    out: &mut Vec<Line<'static>>,
    palette: &Palette,
) {
    match entry {
        // User turns carry the composer prompt marker in faint text;
        // continuation lines indent by the prompt width (Go parity).
        ion_core::SessionEntry::UserMessage { text } => {
            for (i, logical_line) in text.split('\n').enumerate() {
                let prefix = if i == 0 { "> " } else { "  " };
                out.push(Line::from(format!("{prefix}{logical_line}")).style(palette.user_entry));
            }
        }
        ion_core::SessionEntry::ModelChanged { model_ref } => {
            out.push(Line::from(format!("• model → {model_ref}")).style(palette.system_note));
        }
        ion_core::SessionEntry::AssistantMessage { text } => {
            let total = text.lines().count();
            for (i, logical_line) in text.split('\n').enumerate() {
                if logical_line.is_empty() && (i == 0 || i + 1 == total) {
                    continue;
                }
                out.push(Line::from(logical_line.to_owned()).style(palette.assistant));
            }
        }
        ion_core::SessionEntry::ToolCall { call } => {
            let target = ion_core::target_from_arguments(&call.name, &call.arguments)
                .unwrap_or_else(|| format!("(call {})", call.call_id));
            out.push(Line::from(format!("• {} → {target}", call.name)).style(palette.tool_row));
        }
        ion_core::SessionEntry::ToolResult { result } => {
            if result.is_ok() {
                for logical_line in result.model_text().split('\n') {
                    if logical_line.is_empty() {
                        continue;
                    }
                    out.push(Line::from(format!("  {logical_line}")).style(palette.system_note));
                }
            } else {
                out.push(
                    Line::from(format!("\u{2717} {}", result.model_text()))
                        .style(palette.tool_error),
                );
            }
        }
        ion_core::SessionEntry::Compaction { summary, .. } => {
            out.push(Line::from(format!("• compacted: {summary}")).style(palette.system_note));
        }
    }
}
