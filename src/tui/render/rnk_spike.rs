//! Experimental RNK-backed renderer for the bottom UI area.
//!
//! This is a scoped spike path for input/progress/status rendering while
//! preserving Ion's existing chat-scrollback behavior.

use crate::tool::ToolMode;
use crate::tui::App;
use crate::tui::composer::{ComposerState, build_visual_lines};
use crate::tui::render::{CONTINUATION, INPUT_MARGIN, PROMPT, PROMPT_WIDTH};
use crate::tui::types::Mode;
use crate::tui::util::{format_cost, format_elapsed, format_tokens, truncate_to_display_width};
use crossterm::cursor::MoveTo;
use crossterm::execute;
use crossterm::terminal::{Clear, ClearType};
use rnk::components::{Box as RnkBox, Span, Text};
use rnk::core::{Color as RnkColor, FlexDirection, TextWrap};
use std::io::Write;
use std::sync::OnceLock;

const RNK_BOTTOM_UI_ENV: &str = "ION_RNK_BOTTOM_UI";
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn parse_env_bool(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "0" | "false" | "off" | "no"
    )
}

fn rnk_bottom_ui_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var(RNK_BOTTOM_UI_ENV)
            .map(|value| parse_env_bool(&value))
            .unwrap_or(false)
    })
}

fn render_rnk_line(
    text: &str,
    max_cells: usize,
    color: Option<RnkColor>,
    bold: bool,
    dim: bool,
) -> String {
    if max_cells == 0 {
        return String::new();
    }

    let clipped = truncate_to_display_width(text, max_cells);
    if clipped.is_empty() {
        return String::new();
    }

    let mut line = Text::new(clipped);
    if let Some(color) = color {
        line = line.color(color);
    }
    if bold {
        line = line.bold();
    }
    if dim {
        line = line.dim();
    }

    render_rnk_text_line(line, max_cells)
}

fn render_rnk_text_line(text: Text, max_cells: usize) -> String {
    let element = RnkBox::new()
        .flex_direction(FlexDirection::Row)
        .width(max_cells as u16)
        .child(text.wrap(TextWrap::Truncate).into_element())
        .into_element();
    let rendered = rnk::render_to_string_no_trim(&element, max_cells as u16);
    rendered.lines().next().unwrap_or_default().to_string()
}

fn paint_row<W: Write>(
    w: &mut W,
    row: u16,
    width: u16,
    text: &str,
    color: Option<RnkColor>,
    bold: bool,
    dim: bool,
) -> std::io::Result<()> {
    execute!(w, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
    let max_cells = width.saturating_sub(1) as usize;
    if max_cells == 0 {
        return Ok(());
    }
    let rendered = render_rnk_line(text, max_cells, color, bold, dim);
    write!(w, "{rendered}")?;
    Ok(())
}

fn paint_row_spans<W: Write>(
    w: &mut W,
    row: u16,
    width: u16,
    spans: Vec<Span>,
) -> std::io::Result<()> {
    execute!(w, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
    let max_cells = width.saturating_sub(1) as usize;
    if max_cells == 0 || spans.is_empty() {
        return Ok(());
    }
    let rendered = render_rnk_text_line(Text::spans(spans), max_cells);
    write!(w, "{rendered}")?;
    Ok(())
}

impl App {
    pub(crate) fn should_use_rnk_bottom_ui(&self, popup_active: bool) -> bool {
        rnk_bottom_ui_enabled()
            && self.mode == Mode::Input
            && !popup_active
            && !self.command_completer.is_active()
            && !self.file_completer.is_active()
    }

    pub(crate) fn rnk_progress_gap_rows(&self, popup_active: bool) -> u16 {
        u16::from(self.should_use_rnk_bottom_ui(popup_active))
    }

    pub(crate) fn render_bottom_ui_rnk<W: Write>(
        &mut self,
        w: &mut W,
        progress_row: u16,
        progress_height: u16,
        input_row: u16,
        input_height: u16,
        status_row: u16,
        width: u16,
    ) -> std::io::Result<()> {
        // Mirror existing input-box shape: top border + content + bottom border.
        let content_start = input_row.saturating_add(1);
        let content_height = input_height.saturating_sub(2);
        let progress_line_row = progress_row.saturating_add(progress_height.saturating_sub(1));

        for row in progress_row..progress_line_row {
            paint_row(w, row, width, "", None, false, false)?;
        }

        let (progress_text, progress_color) = self.progress_line_text(width);
        paint_row(
            w,
            progress_line_row,
            width,
            &progress_text,
            progress_color,
            false,
            false,
        )?;

        let border = "─".repeat(width.saturating_sub(1) as usize);
        paint_row(
            w,
            input_row,
            width,
            &border,
            Some(RnkColor::Cyan),
            false,
            false,
        )?;

        let input_lines = self.input_lines_for_height(width, content_height);
        for (idx, line) in input_lines.iter().enumerate() {
            let row = content_start.saturating_add(idx as u16);
            paint_row(w, row, width, line, None, false, false)?;
        }

        let border_row = content_start.saturating_add(content_height);
        paint_row(
            w,
            border_row,
            width,
            &border,
            Some(RnkColor::Cyan),
            false,
            false,
        )?;

        let status_spans = self.status_line_spans();
        paint_row_spans(w, status_row, width, status_spans)?;

        let (cursor_x, cursor_y) = self.input_state.cursor_pos;
        let scroll_offset = self.input_state.scroll_offset() as u16;
        let cursor_y = cursor_y.saturating_sub(scroll_offset);
        let cursor_col = cursor_x
            .saturating_add(PROMPT_WIDTH)
            .min(width.saturating_sub(1));
        let cursor_row = content_start
            .saturating_add(cursor_y)
            .min(content_start.saturating_add(content_height.saturating_sub(1)));
        execute!(w, MoveTo(cursor_col, cursor_row))?;
        Ok(())
    }

    fn input_lines_for_height(&mut self, width: u16, height: u16) -> Vec<String> {
        let visible_height = height as usize;
        if visible_height == 0 {
            return Vec::new();
        }

        let content = self.input_buffer.get_content();
        let content_width = width.saturating_sub(INPUT_MARGIN) as usize;
        let visual_lines = build_visual_lines(&content, content_width);

        if content_width > 0 {
            self.input_state.calculate_cursor_pos_with(
                &content,
                &visual_lines,
                self.input_buffer.len_chars(),
                content_width,
            );
        }

        let total_lines =
            ComposerState::visual_line_count_with(&content, &visual_lines, content_width);
        self.input_state
            .scroll_to_cursor(visible_height, total_lines);
        let scroll_offset = self.input_state.scroll_offset();
        let total_chars = content.chars().count();

        let mut out = Vec::with_capacity(visible_height);
        for row in 0..visible_height {
            let line_index = scroll_offset + row;
            if line_index >= total_lines {
                out.push(String::new());
                continue;
            }

            let (start, end) = if line_index < visual_lines.len() {
                visual_lines[line_index]
            } else {
                (total_chars, total_chars)
            };

            let chunk: String = content
                .chars()
                .skip(start)
                .take(end.saturating_sub(start))
                .filter(|&c| c != '\n')
                .collect();
            let chunk = truncate_to_display_width(&chunk, content_width);
            if line_index == 0 {
                out.push(format!("{PROMPT}{chunk}"));
            } else {
                out.push(format!("{CONTINUATION}{chunk}"));
            }
        }

        if content.is_empty() && !out.is_empty() {
            out[0] = PROMPT.to_string();
        }
        out
    }

    fn progress_line_text(&self, width: u16) -> (String, Option<RnkColor>) {
        let max_cells = width.saturating_sub(1) as usize;
        if max_cells == 0 {
            return (String::new(), None);
        }

        if self.is_running {
            let frame = (self.frame_count % SPINNER.len() as u64) as usize;
            let text = if let Some((reason, delay, started)) = &self.task.retry_status {
                let elapsed = started.elapsed().as_secs();
                let secs_left = delay.saturating_sub(elapsed);
                format!(" {} {reason} • retrying in {secs_left}s", SPINNER[frame])
            } else {
                let mut text = format!(" {} ", SPINNER[frame]);
                if let Some(tool) = &self.task.current_tool {
                    text.push_str(tool);
                } else if self.task.thinking_start.is_some() {
                    text.push_str("Thinking...");
                } else {
                    text.push_str("Ionizing...");
                }
                if let Some(start) = self.task.start_time {
                    let elapsed = start.elapsed().as_secs();
                    text.push_str(&format!(" ({elapsed}s • Esc to cancel)"));
                }
                text
            };
            let color = if self.task.retry_status.is_some() {
                Some(RnkColor::Yellow)
            } else {
                Some(RnkColor::Cyan)
            };
            return (truncate_to_display_width(&text, max_cells), color);
        }

        let Some(summary) = self.last_task_summary.as_ref() else {
            return (String::new(), None);
        };

        let (symbol, label, color) = if self.last_error.is_some() {
            ("✗", "Error", Some(RnkColor::Red))
        } else if summary.was_cancelled {
            ("⚠", "Canceled", Some(RnkColor::Yellow))
        } else {
            ("✓", "Completed", Some(RnkColor::Green))
        };

        let elapsed = format_elapsed(summary.elapsed.as_secs());
        let mut stats = vec![elapsed];
        if summary.input_tokens > 0 {
            stats.push(format!("↑ {}", format_tokens(summary.input_tokens)));
        }
        if summary.output_tokens > 0 {
            stats.push(format!("↓ {}", format_tokens(summary.output_tokens)));
        }
        if summary.cost > 0.0 {
            stats.push(format_cost(summary.cost));
        }

        (
            truncate_to_display_width(
                &format!(" {symbol} {label} ({})", stats.join(" • ")),
                max_cells,
            ),
            color,
        )
    }

    fn status_line_spans(&self) -> Vec<Span> {
        let model_name = self
            .session
            .model
            .split('/')
            .next_back()
            .unwrap_or(&self.session.model);

        let (mode_label, mode_color) = match self.tool_mode {
            ToolMode::Read => ("READ", RnkColor::Cyan),
            ToolMode::Write => ("WRITE", RnkColor::Yellow),
        };

        let mut spans = vec![
            Span::new(" ["),
            Span::new(mode_label).color(mode_color),
            Span::new("] • "),
            Span::new(model_name),
        ];

        let think_label = self.thinking_level.label();
        if !think_label.is_empty() {
            spans.push(Span::new(" "));
            spans.push(Span::new(think_label).color(RnkColor::Magenta));
        }

        if let Some((used, max)) = self.token_usage {
            spans.push(
                Span::new(format!(" • {}/{}", format_tokens(used), format_tokens(max))).dim(),
            );
            if max > 0 {
                let pct = (used * 100) / max;
                spans.push(Span::new(format!(" ({pct}%)")).dim());
            }
        }

        spans
    }
}

#[cfg(test)]
mod tests {
    use super::parse_env_bool;

    #[test]
    fn parse_env_bool_false_values() {
        for value in ["", "0", "false", "False", "off", "no"] {
            assert!(!parse_env_bool(value), "expected false for {value:?}");
        }
    }

    #[test]
    fn parse_env_bool_true_values() {
        for value in ["1", "true", "yes", "on", "anything"] {
            assert!(parse_env_bool(value), "expected true for {value:?}");
        }
    }
}
