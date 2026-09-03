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
    let logical_lines = split_styled_lines(line);
    if logical_lines.len() > 1 {
        return logical_lines
            .iter()
            .flat_map(|line| wrap_single_line(line, width))
            .collect();
    }
    wrap_single_line(&logical_lines[0], width)
}

fn split_styled_lines(line: &Line<'_>) -> Vec<Line<'static>> {
    let mut rows: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    for span in &line.spans {
        let style = line.style.patch(span.style);
        for (index, part) in span.content.split('\n').enumerate() {
            if index > 0 {
                rows.push(Vec::new());
            }
            if !part.is_empty() {
                rows.last_mut()
                    .expect("split line always has a current row")
                    .push(Span::styled(part.to_owned(), style));
            }
        }
    }
    rows.into_iter().map(Line::from).collect()
}

fn wrap_single_line(line: &Line<'_>, width: usize) -> Vec<Line<'static>> {
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

#[derive(Debug, Clone)]
struct ComposerWrappedRow {
    line: Line<'static>,
    source_start: usize,
    source_end: usize,
}

fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn composer_wrapped_rows(
    logical_line: &str,
    prefix: &str,
    width: usize,
) -> Vec<ComposerWrappedRow> {
    let available = width.saturating_sub(prefix.width()).max(1);
    let wrapped = wrap_line(&Line::from(logical_line.to_owned()), available);
    let mut source_offset = 0;
    wrapped
        .into_iter()
        .enumerate()
        .map(|(row_index, content)| {
            let text = line_text(&content);
            while source_offset < logical_line.chars().count()
                && logical_line.chars().nth(source_offset) == Some(' ')
                && !text.starts_with(' ')
            {
                source_offset += 1;
            }
            let source_start = source_offset;
            source_offset += text.chars().count();
            let source_end = source_offset;
            let row_prefix = if source_start == 0 && row_index == 0 {
                prefix
            } else {
                "  "
            };
            let mut spans = vec![Span::raw(row_prefix.to_owned())];
            spans.extend(content.spans);
            ComposerWrappedRow {
                line: Line::from(spans),
                source_start,
                source_end,
            }
        })
        .collect()
}

fn composer_cursor_in_rows(
    logical_line: &str,
    rows: &[ComposerWrappedRow],
    local_cursor: usize,
    prefix: &str,
) -> (usize, usize) {
    for (row_index, row) in rows.iter().enumerate() {
        if local_cursor < row.source_start {
            return (row_index, prefix.width());
        }
        if local_cursor <= row.source_end {
            let content_start = char_offset_to_byte(logical_line, row.source_start);
            let cursor_byte = char_offset_to_byte(logical_line, local_cursor);
            return (
                row_index,
                prefix.width() + logical_line[content_start..cursor_byte].width(),
            );
        }
    }
    let row = rows.last().expect("composer always has one row");
    (rows.len() - 1, row.line.width())
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
        let wrapped = composer_wrapped_rows(logical_line, prefix, width);
        let line_length = logical_line.chars().count();
        for local_cursor in 0..=line_length {
            let (row, screen_column) =
                composer_cursor_in_rows(logical_line, &wrapped, local_cursor, prefix);
            positions.push(ComposerCursorPosition {
                cursor: text_offset + local_cursor,
                row: visual_offset + row,
                column: screen_column.saturating_sub(prefix.width()),
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
        let wrapped = composer_wrapped_rows(logical_line, prefix, width);
        let line_length = logical_line.chars().count();
        if state.cursor >= text_offset && state.cursor <= text_offset + line_length {
            let local_cursor = state.cursor - text_offset;
            let (row_index, screen_column) =
                composer_cursor_in_rows(logical_line, &wrapped, local_cursor, prefix);
            cursor = Some((
                rows.len() + row_index,
                screen_column.min(width.saturating_sub(1)) as u16,
            ));
        }
        rows.extend(wrapped.into_iter().map(|row| row.line));
        text_offset += line_length + 1;
    }
    (rows, cursor)
}

fn limit_composer_rows(
    (mut rows, mut cursor): (Vec<Line<'static>>, Option<(usize, u16)>),
    limit: usize,
) -> (Vec<Line<'static>>, Option<(usize, u16)>) {
    let limit = limit.max(1);
    if rows.len() > limit {
        let cursor_row = cursor.map_or(rows.len() - 1, |(row, _)| row);
        let start = cursor_row
            .saturating_add(1)
            .saturating_sub(limit)
            .min(rows.len() - limit);
        rows = rows.split_off(start);
        cursor = cursor.map(|(row, column)| (row.saturating_sub(start).min(limit - 1), column));
    }
    (rows, cursor)
}

/// Maximum virtual live-band height. `Screen` reserves this stable band;
/// this renderer returns only the meaningful bottom-aligned rows so idle
/// sessions do not display trailing blank space.
pub(super) const LIVE_REGION_MAX_ROWS: usize = 10;
const LIVE_REGION_BASE_ROWS: usize = 7;
const LIVE_CHROME_ROWS: usize = 5;
const MAX_MODEL_SELECTOR_OPTIONS: usize = 2;

pub(super) fn live_region_height(state: &UiState) -> usize {
    if state.model_selector.is_some()
        || state.session_selector.is_some()
        || state.thinking_selector.is_some()
        || state.file_selector.is_some()
    {
        LIVE_REGION_MAX_ROWS
    } else {
        LIVE_REGION_BASE_ROWS
    }
}

fn selector_row(line: Line<'static>, width: usize) -> Line<'static> {
    wrap_line(&line, width)
        .into_iter()
        .next()
        .unwrap_or_default()
}

fn compact_model_selector_lines(
    state: &UiState,
    palette: &Palette,
    width: usize,
) -> Vec<Line<'static>> {
    let models = state.filtered_model_catalog();
    let selected = state
        .model_selector
        .as_ref()
        .map_or(0, |selector| selector.selected);
    let selected_line = models.get(selected).map_or_else(
        || "  no matching models".to_owned(),
        |model| format!("  > {model}"),
    );
    vec![
        selector_row(Line::from(selected_line).style(palette.system_note), width),
        selector_row(
            Line::from("  ↑/↓ · enter · esc").style(palette.system_note),
            width,
        ),
    ]
}

fn model_selector_lines(state: &UiState, palette: &Palette, width: usize) -> Vec<Line<'static>> {
    let query = &state.composer;
    let models = state.filtered_model_catalog();
    let selected = state
        .model_selector
        .as_ref()
        .map_or(0, |selector| selector.selected);
    let mut lines = vec![selector_row(
        Line::from(if query.is_empty() {
            "select model".to_owned()
        } else {
            format!("select model · {query}")
        })
        .style(palette.system_note),
        width,
    )];
    if models.is_empty() {
        lines.push(selector_row(
            Line::from("  no matching models").style(palette.system_note),
            width,
        ));
    } else {
        let start = selected
            .saturating_sub(MAX_MODEL_SELECTOR_OPTIONS / 2)
            .min(models.len().saturating_sub(MAX_MODEL_SELECTOR_OPTIONS));
        let end = (start + MAX_MODEL_SELECTOR_OPTIONS).min(models.len());
        for (index, model) in models.iter().enumerate().take(end).skip(start) {
            let marker = if index == selected { ">" } else { " " };
            let current = state
                .current_model_reference()
                .is_some_and(|current| current == *model);
            let suffix = if current { "  (current)" } else { "" };
            lines.push(selector_row(
                Line::from(format!("  {marker} {model}{suffix}")).style(palette.system_note),
                width,
            ));
        }
    }
    lines.push(selector_row(
        Line::from("  ↑/↓ · enter · esc").style(palette.system_note),
        width,
    ));
    lines
}

fn session_selector_lines(state: &UiState, palette: &Palette, width: usize) -> Vec<Line<'static>> {
    let query = &state.composer;
    let rows = state.filtered_session_rows();
    let selected = state
        .session_selector
        .as_ref()
        .map_or(0, |selector| selector.selected);
    let mut lines = vec![selector_row(
        Line::from(if query.is_empty() {
            "select session".to_owned()
        } else {
            format!("select session · {query}")
        })
        .style(palette.system_note),
        width,
    )];
    if rows.is_empty() {
        lines.push(selector_row(
            Line::from("  no matching sessions").style(palette.system_note),
            width,
        ));
    } else {
        let start = selected
            .saturating_sub(MAX_MODEL_SELECTOR_OPTIONS / 2)
            .min(rows.len().saturating_sub(MAX_MODEL_SELECTOR_OPTIONS));
        let end = (start + MAX_MODEL_SELECTOR_OPTIONS).min(rows.len());
        for (index, row) in rows.iter().enumerate().take(end).skip(start) {
            let marker = if index == selected { ">" } else { " " };
            let current = state.session_id == Some(row.id);
            let suffix = if current { "  (current)" } else { "" };
            lines.push(selector_row(
                Line::from(format!("  {marker} {}{suffix}", row.label)).style(palette.system_note),
                width,
            ));
        }
    }
    lines.push(selector_row(
        Line::from("  ↑/↓ · enter · esc").style(palette.system_note),
        width,
    ));
    lines
}

fn file_selector_lines(state: &UiState, palette: &Palette, width: usize) -> Vec<Line<'static>> {
    let query = &state.composer;
    let rows = state.filtered_file_rows();
    let selected = state
        .file_selector
        .as_ref()
        .map_or(0, |selector| selector.selected);
    let mut lines = vec![selector_row(
        Line::from(if query.is_empty() {
            "reference a file".to_owned()
        } else {
            format!("reference a file · {query}")
        })
        .style(palette.system_note),
        width,
    )];
    if rows.is_empty() {
        lines.push(selector_row(
            Line::from("  no matching files").style(palette.system_note),
            width,
        ));
    } else {
        let start = selected
            .saturating_sub(MAX_MODEL_SELECTOR_OPTIONS / 2)
            .min(rows.len().saturating_sub(MAX_MODEL_SELECTOR_OPTIONS));
        let end = (start + MAX_MODEL_SELECTOR_OPTIONS).min(rows.len());
        for (index, row) in rows.iter().enumerate().take(end).skip(start) {
            let marker = if index == selected { ">" } else { " " };
            lines.push(selector_row(
                Line::from(format!("  {marker} {row}")).style(palette.system_note),
                width,
            ));
        }
    }
    lines.push(selector_row(
        Line::from("  ↑/↓ · enter · esc").style(palette.system_note),
        width,
    ));
    lines
}

fn thinking_selector_lines(state: &UiState, palette: &Palette, width: usize) -> Vec<Line<'static>> {
    let query = &state.composer;
    let levels = state.filtered_thinking_levels();
    let selected = state
        .thinking_selector
        .as_ref()
        .map_or(0, |selector| selector.selected);
    let mut lines = vec![selector_row(
        Line::from(if query.is_empty() {
            "select thinking".to_owned()
        } else {
            format!("select thinking · {query}")
        })
        .style(palette.system_note),
        width,
    )];
    if levels.is_empty() {
        lines.push(selector_row(
            Line::from("  no matching levels").style(palette.system_note),
            width,
        ));
    } else {
        for (index, level) in levels.iter().enumerate() {
            let marker = if index == selected { ">" } else { " " };
            let current = state.thinking_level.as_deref() == Some(level.as_str());
            let suffix = if current { "  (current)" } else { "" };
            lines.push(selector_row(
                Line::from(format!("  {marker} {level}{suffix}")).style(palette.system_note),
                width,
            ));
        }
    }
    lines.push(selector_row(
        Line::from("  ↑/↓ · enter · esc").style(palette.system_note),
        width,
    ));
    lines
}

/// The live band below the committed transcript. Returns at most
/// LIVE_REGION_MAX_ROWS rows plus the hardware cursor position relative
/// to the band; `Screen` bottom-aligns it inside the stable virtual band.
#[cfg(test)]
pub(super) fn build_live(
    state: &UiState,
    palette: &Palette,
    width: usize,
) -> (Vec<Line<'static>>, Option<(usize, u16)>) {
    build_live_at_height(state, palette, width, live_region_height(state))
}

/// Render the live band for the currently available terminal height. Very
/// small terminals cannot fit the normal footer/chrome, so keep the editor
/// and, for the picker, its selected model and controls usable instead of
/// clipping the picker off the top of the physical window.
pub(super) fn build_live_at_height(
    state: &UiState,
    palette: &Palette,
    width: usize,
    band_height: usize,
) -> (Vec<Line<'static>>, Option<(usize, u16)>) {
    let band_height = band_height.clamp(1, LIVE_REGION_MAX_ROWS);
    if band_height < LIVE_CHROME_ROWS + 1 {
        let head = if state.model_selector.is_some() {
            compact_model_selector_lines(state, palette, width)
        } else {
            Vec::new()
        };
        let (composer_rows, composer_cursor) = limit_composer_rows(
            composer_rows(state, width),
            band_height.saturating_sub(head.len()).max(1),
        );
        let cursor = composer_cursor.map(|(row, column)| (head.len() + row, column));
        let mut lines = head;
        lines.extend(composer_rows);
        return (lines, cursor);
    }
    // Composer first: it is anchored to the band's bottom and owns the
    // hardware cursor. Long drafts keep the viewport around the cursor.
    let (composer_rows, composer_cursor) = limit_composer_rows(
        composer_rows(state, width),
        band_height.saturating_sub(LIVE_CHROME_ROWS),
    );
    let composer_len = composer_rows.len();

    let mut head: Vec<Line<'static>> = Vec::new();
    if state.model_selector.is_some() {
        head.extend(model_selector_lines(state, palette, width));
    } else if state.session_selector.is_some() {
        head.extend(session_selector_lines(state, palette, width));
    } else if state.thinking_selector.is_some() {
        head.extend(thinking_selector_lines(state, palette, width));
    } else if state.file_selector.is_some() {
        head.extend(file_selector_lines(state, palette, width));
    } else if state.hotkeys_visible {
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
    // Provisional shell passthrough output streams above the composer
    // while the command runs; the durable settlement replaces it.
    if !state.shell_output.is_empty() {
        let preview: Vec<&str> = state.shell_output.lines().rev().take(8).collect::<Vec<_>>();
        for line in preview.iter().rev() {
            head.extend(wrap_line(&Line::from(format!("  {line}")).dim(), width));
        }
    }
    // A durable queued follow-up stays visible above the composer
    // (pi parity: queued messages remain inspectable and restorable).
    if let Some(queued) = &state.queued_prompt {
        head.extend(wrap_line(
            &Line::from(format!("queued \u{21bb} {queued}")).dim(),
            width,
        ));
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
    let budget = band_height
        .saturating_sub(LIVE_CHROME_ROWS)
        .saturating_sub(composer_len);
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

    // Shell chrome: keep one empty row between transcript/live output and
    // the composer, then a dim rule immediately above the editable input.
    lines.push(Line::default());
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
    // At narrow widths there may not be room for both segments: keep the
    // usage prefix and elide the provider/model with an ellipsis rather
    // than gluing the two segments together (2026-09-01 dogfood finding).
    let content_width = width.saturating_sub(1);
    let usage_prefix = usage_label.as_deref().unwrap_or("");
    let left_width = usage_prefix.width() + usize::from(!usage_prefix.is_empty()) + 1;
    let pad = content_width
        .saturating_sub(left_width)
        .saturating_sub(provider_model.width())
        .saturating_sub(1);
    let (visible_provider, provider_width) = if pad == 0 {
        // No room for the provider plus a separating space: show an
        // ellipsis if even that fits inside the remaining columns.
        let remaining = content_width.saturating_sub(left_width).saturating_sub(1);
        if remaining >= 1 {
            ("\u{2026}".to_owned(), 1)
        } else {
            (String::new(), 0)
        }
    } else {
        (provider_model.clone(), provider_model.width())
    };
    let pad = content_width
        .saturating_sub(left_width)
        .saturating_sub(provider_width)
        .saturating_sub(1);
    let usage_text = if usage_prefix.is_empty() {
        " ".to_owned()
    } else {
        format!(" {usage_prefix} ")
    };
    lines.push(
        Line::from(format!(
            "{usage_text}{}{}",
            " ".repeat(pad),
            visible_provider
        ))
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
        ion_core::SessionEntry::AgentMessage { from, text } => {
            let rendered = format!("[Message from {from}]\n{text}");
            for (i, logical_line) in rendered.split('\n').enumerate() {
                let prefix = if i == 0 { "> " } else { "  " };
                out.push(Line::from(format!("{prefix}{logical_line}")).style(palette.user_entry));
            }
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
        ion_core::SessionEntry::ShellExecution {
            command,
            output,
            exit_code,
            cancelled,
            exclude_from_context,
        } => {
            let prefix = if *exclude_from_context { "!!" } else { "!" };
            out.push(Line::from(format!("{prefix}{command}")).style(palette.user_marker));
            for logical_line in output.split('\n') {
                if logical_line.is_empty() {
                    continue;
                }
                out.push(Line::from(format!("  {logical_line}")).style(palette.system_note));
            }
            if *cancelled {
                out.push(Line::from("  (command cancelled)").style(palette.tool_error));
            } else if *exit_code != Some(0) {
                out.push(
                    Line::from(format!("  (exit {}) ", exit_code.unwrap_or(-1)))
                        .style(palette.tool_error),
                );
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
