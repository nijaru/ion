//! Direct-crossterm renderer for the bottom UI area.

use crate::tool::ToolMode;
use crate::tui::ansi::{self, Color};
use crate::tui::composer::{build_visual_lines, ComposerState};
#[cfg(test)]
use crate::tui::render::buffer as buf_mod;
use crate::tui::render::{CONTINUATION, INPUT_MARGIN, PROMPT, PROMPT_WIDTH};
use crate::tui::util::{display_width, format_cost, format_elapsed, format_tokens, render_token_bar, truncate_to_display_width};
use crate::tui::App;
use crossterm::cursor::MoveTo;
use crossterm::execute;
use crossterm::terminal::{Clear, ClearType};
use std::io::Write;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Render a plain-text row into a pre-rendered ANSI string (no terminal I/O).
fn format_row_content(width: u16, text: &str, fg: Option<Color>, bold: bool, dim: bool) -> String {
    let max_cells = width.saturating_sub(1) as usize;
    if max_cells == 0 {
        return String::new();
    }
    ansi::render_line(text, max_cells, fg, bold, dim)
}

/// Render a span list into a pre-rendered ANSI string (no terminal I/O).
fn format_spans_content(width: u16, spans: Vec<ansi::Span>) -> String {
    let max_cells = width.saturating_sub(1) as usize;
    if max_cells == 0 || spans.is_empty() {
        return String::new();
    }
    ansi::render_spans(&spans)
}

pub(crate) struct BottomUiFrame {
    pub progress_row: u16,
    pub progress_height: u16,
    pub input_row: u16,
    pub input_height: u16,
    pub status_row: u16,
    pub width: u16,
    pub show_progress_status: bool,
}

fn paint_row<W: Write>(
    w: &mut W,
    row: u16,
    width: u16,
    text: &str,
    fg: Option<Color>,
    bold: bool,
    dim: bool,
) -> std::io::Result<()> {
    execute!(w, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
    let content = format_row_content(width, text, fg, bold, dim);
    if !content.is_empty() {
        write!(w, "{content}")?;
    }
    Ok(())
}

fn paint_row_spans<W: Write>(
    w: &mut W,
    row: u16,
    width: u16,
    spans: Vec<ansi::Span>,
) -> std::io::Result<()> {
    execute!(w, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
    let content = format_spans_content(width, spans);
    if !content.is_empty() {
        write!(w, "{content}")?;
    }
    Ok(())
}

impl App {
    pub(crate) fn render_bottom_ui<W: Write>(
        &mut self,
        w: &mut W,
        frame: BottomUiFrame,
    ) -> std::io::Result<()> {
        let BottomUiFrame {
            progress_row,
            progress_height,
            input_row,
            input_height,
            status_row,
            width,
            show_progress_status,
        } = frame;
        // Mirror existing input-box shape: top border + content + bottom border.
        let content_start = input_row.saturating_add(1);
        let content_height = input_height.saturating_sub(2);
        let progress_line_row = progress_row.saturating_add(progress_height.saturating_sub(1));

        for row in progress_row..progress_line_row {
            paint_row(w, row, width, "", None, false, false)?;
        }

        if show_progress_status {
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
        } else {
            paint_row(w, progress_line_row, width, "", None, false, false)?;
        }

        let border = "─".repeat(width.saturating_sub(1) as usize);
        paint_row(
            w,
            input_row,
            width,
            &border,
            Some(Color::DarkCyan),
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
            Some(Color::DarkCyan),
            false,
            false,
        )?;

        if show_progress_status {
            let status_spans = self.status_line_spans(width);
            paint_row_spans(w, status_row, width, status_spans)?;
        } else {
            paint_row(w, status_row, width, "", None, false, false)?;
        }

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

    /// Render the bottom UI into a [`buf_mod::Buffer`] for snapshot testing.
    ///
    /// Returns `(buffer, cursor_col, cursor_row)` where cursor coordinates are
    /// absolute terminal positions (same as `render_bottom_ui`).
    #[cfg(test)]
    pub(crate) fn render_bottom_ui_to_buffer(
        &mut self,
        frame: BottomUiFrame,
    ) -> (buf_mod::Buffer, u16, u16) {
        let BottomUiFrame {
            progress_row,
            progress_height,
            input_row,
            input_height,
            status_row,
            width,
            show_progress_status,
        } = frame;

        let origin = progress_row;
        let buf_height = status_row.saturating_sub(origin) as usize + 1;
        let mut buffer = buf_mod::Buffer::new(width as usize, buf_height);

        let content_start = input_row.saturating_add(1);
        let content_height = input_height.saturating_sub(2);
        let progress_line_row =
            progress_row.saturating_add(progress_height.saturating_sub(1));

        // Progress gap rows (empty)
        for row in progress_row..progress_line_row {
            buffer.set_row(
                (row - origin) as usize,
                format_row_content(width, "", None, false, false),
            );
        }

        // Progress line
        let progress_content = if show_progress_status {
            let (text, color) = self.progress_line_text(width);
            format_row_content(width, &text, color, false, false)
        } else {
            format_row_content(width, "", None, false, false)
        };
        buffer.set_row((progress_line_row - origin) as usize, progress_content);

        // Top border
        let border = "─".repeat(width.saturating_sub(1) as usize);
        buffer.set_row(
            (input_row - origin) as usize,
            format_row_content(width, &border, Some(Color::DarkCyan), false, false),
        );

        // Input lines
        let input_lines = self.input_lines_for_height(width, content_height);
        for (idx, line) in input_lines.iter().enumerate() {
            let row = content_start.saturating_add(idx as u16);
            buffer.set_row(
                (row - origin) as usize,
                format_row_content(width, line, None, false, false),
            );
        }

        // Bottom border
        let border_row = content_start.saturating_add(content_height);
        buffer.set_row(
            (border_row - origin) as usize,
            format_row_content(width, &border, Some(Color::DarkCyan), false, false),
        );

        // Status line
        let status_content = if show_progress_status {
            let spans = self.status_line_spans(width);
            format_spans_content(width, spans)
        } else {
            format_row_content(width, "", None, false, false)
        };
        buffer.set_row((status_row - origin) as usize, status_content);

        // Cursor position (absolute)
        let (cursor_x, cursor_y) = self.input_state.cursor_pos;
        let scroll_offset = self.input_state.scroll_offset() as u16;
        let cursor_y = cursor_y.saturating_sub(scroll_offset);
        let cursor_col = cursor_x
            .saturating_add(PROMPT_WIDTH)
            .min(width.saturating_sub(1));
        let cursor_row = content_start
            .saturating_add(cursor_y)
            .min(content_start.saturating_add(content_height.saturating_sub(1)));

        (buffer, cursor_col, cursor_row)
    }

    fn input_lines_for_height(&mut self, width: u16, height: u16) -> Vec<String> {
        let visible_height = height as usize;
        if visible_height == 0 {
            return Vec::new();
        }

        let content = self.input_buffer.get_content();
        let content_width = width.saturating_sub(INPUT_MARGIN) as usize;
        let visual_lines = build_visual_lines(&content, content_width);

        let total_lines =
            ComposerState::visual_line_count_with(&content, &visual_lines, content_width);
        if content_width > 0 {
            self.input_state.calculate_cursor_pos_with(
                &content,
                &visual_lines,
                self.input_buffer.len_chars(),
                content_width,
            );
            self.input_state
                .scroll_to_cursor(visible_height, total_lines);
        }
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

    fn progress_line_text(&self, width: u16) -> (String, Option<Color>) {
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
                Some(Color::DarkYellow)
            } else {
                Some(Color::DarkCyan)
            };
            return (truncate_to_display_width(&text, max_cells), color);
        }

        let Some(summary) = self.last_task_summary.as_ref() else {
            return (String::new(), None);
        };

        let (symbol, label, color) = if self.last_error.is_some() {
            ("✗", "Error", Some(Color::DarkRed))
        } else if summary.was_cancelled {
            ("⚠", "Canceled", Some(Color::DarkYellow))
        } else {
            ("✓", "Completed", Some(Color::DarkGreen))
        };

        let elapsed = format_elapsed(summary.elapsed.as_secs());
        let mut stats = vec![elapsed];
        if summary.input_tokens > 0 {
            stats.push(format!("↑ {}", format_tokens(summary.input_tokens)));
        }
        if summary.output_tokens > 0 {
            stats.push(format!("↓ {}", format_tokens(summary.output_tokens)));
        }
        if !self.api_provider.is_oauth() && summary.cost > 0.0 {
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

    fn status_line_spans(&self, width: u16) -> Vec<ansi::Span> {
        let max_cells = width.saturating_sub(1) as usize;
        if max_cells == 0 {
            return vec![];
        }

        let sl = &self.config.status_line;

        let model_name = self
            .session
            .model
            .split('/')
            .next_back()
            .unwrap_or(&self.session.model);
        let project = self
            .session
            .working_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("~");
        let (mode_label, mode_color) = match self.tool_mode {
            ToolMode::Read => ("READ", Color::DarkCyan),
            ToolMode::Write => ("WRITE", Color::DarkYellow),
        };
        let think = self.thinking_level.label();
        let branch = if sl.show_branch { self.git_branch.as_deref() } else { None };
        let is_subscription = self.api_provider.is_oauth();
        let cost_text = if sl.show_cost && !is_subscription {
            Some(format_cost(self.session_cost))
        } else {
            None
        };

        let (pct_text, detail_text) = if sl.show_tokens {
            match self.token_usage {
                Some((used, max)) if max > 0 => {
                    let pct = (used * 100) / max;
                    (
                        format!("{pct}%"),
                        Some(format!("({}/{})", format_tokens(used), format_tokens(max))),
                    )
                }
                Some((used, _)) if used > 0 => (format_tokens(used), None),
                _ => (String::new(), None),
            }
        } else {
            (String::new(), None)
        };

        // Segment widths (each includes its own " • " separator prefix).
        // Drop order: detail → model → diff stats → branch.
        // Always shown: mode, project. Cost/tokens/model/branch subject to config flags.
        let mode_w = mode_label.len() + 3; // " [MODE]" — mode_label is static ASCII
        let think_w = if think.is_empty() { 0 } else { 1 + display_width(think) };
        let model_seg = if sl.show_model { 3 + display_width(model_name) + think_w } else { 0 };
        let think_seg = if think.is_empty() { 0 } else { 3 + display_width(think) }; // standalone
        // Build bar display: "██████ 45%" instead of plain "45%"
        let pct_display = if pct_text.is_empty() {
            String::new()
        } else {
            match self.token_usage {
                Some((used, max)) if max > 0 => {
                    let pct_val = (used * 100) / max;
                    format!("{} {pct_text}", render_token_bar(pct_val as u64, 6))
                }
                _ => pct_text.clone(),
            }
        };

        let pct_seg = if pct_display.is_empty() {
            0
        } else {
            3 + display_width(&pct_display)
        };
        let detail_extra = detail_text.as_ref().map_or(0, |d| 1 + d.len()); // ASCII numbers
        let cost_seg = cost_text.as_ref().map_or(0, |c| 3 + c.len()); // ASCII "$0.12"
        let branch_extra = branch.map_or(0, |b| 3 + display_width(b)); // " • branch"
        let diff_stat = if sl.show_git_diff { self.git_diff_stat } else { None };
        let diff_texts = diff_stat.map(|(ins, del)| (format!("+{ins}"), format!("-{del}")));
        let diff_extra = diff_texts
            .as_ref()
            .map_or(0, |(i, d)| 2 + i.len() + d.len()); // " +N/-M" — ASCII
        let proj_seg = 3 + display_width(project);

        // Total width at each drop level.
        let w0 =
            mode_w + model_seg + pct_seg + detail_extra + cost_seg + proj_seg + branch_extra + diff_extra;
        let w1 = mode_w + model_seg + pct_seg + cost_seg + proj_seg + branch_extra + diff_extra;
        let w2 = mode_w + think_seg + pct_seg + cost_seg + proj_seg + branch_extra + diff_extra;
        let w3 = mode_w + think_seg + pct_seg + cost_seg + proj_seg + branch_extra;

        let (show_model_adaptive, show_detail, show_branch_adaptive, show_diff_adaptive) = if w0 <= max_cells {
            (true, true, true, true)
        } else if w1 <= max_cells {
            (true, false, true, true)
        } else if w2 <= max_cells {
            (false, false, true, true)
        } else if w3 <= max_cells {
            (false, false, true, false)
        } else {
            (false, false, false, false)
        };

        // Combine config flags with adaptive width flags.
        let show_model_final = sl.show_model && show_model_adaptive;
        let show_branch_final = show_branch_adaptive;
        let show_diff_final = sl.show_git_diff && show_diff_adaptive;

        // Build spans.
        let mut spans = vec![
            ansi::Span::new(" ["),
            ansi::Span::new(mode_label).color(mode_color),
            ansi::Span::new("]"),
        ];

        if show_model_final {
            spans.push(ansi::Span::new(" • "));
            spans.push(ansi::Span::new(model_name));
            if !think.is_empty() {
                spans.push(ansi::Span::new(" "));
                spans.push(ansi::Span::new(think).color(Color::DarkMagenta));
            }
        } else if !think.is_empty() {
            spans.push(ansi::Span::new(" • "));
            spans.push(ansi::Span::new(think).color(Color::DarkMagenta));
        }

        if !pct_display.is_empty() {
            let pct_color = match self.token_usage {
                Some((used, max)) if max > 0 => {
                    let pct = (used * 100) / max;
                    if pct >= 80 {
                        Some(Color::DarkRed)
                    } else if pct >= 50 {
                        Some(Color::DarkYellow)
                    } else {
                        Some(Color::DarkGreen)
                    }
                }
                _ => None,
            };
            spans.push(ansi::Span::new(" • ").dim());
            let mut pct_span = ansi::Span::new(pct_display.as_str());
            if let Some(c) = pct_color {
                pct_span = pct_span.color(c);
            }
            spans.push(pct_span);
            if show_detail
                && let Some(ref detail) = detail_text
            {
                spans.push(ansi::Span::new(format!(" {detail}")).dim());
            }
        }

        if let Some(ref cost_text) = cost_text {
            spans.push(ansi::Span::new(format!(" • {cost_text}")).dim());
        }

        spans.push(ansi::Span::new(" • ").dim());
        spans.push(ansi::Span::new(project));
        if show_branch_final
            && let Some(b) = branch
        {
            spans.push(ansi::Span::new(" • ").dim());
            spans.push(ansi::Span::new(b).color(Color::DarkCyan));
            if show_diff_final
                && let Some((ref ins, ref del)) = diff_texts
            {
                spans.push(ansi::Span::new(format!(" {ins}")).color(Color::DarkGreen));
                spans.push(ansi::Span::new("/").dim());
                spans.push(ansi::Span::new(del).color(Color::DarkRed));
            }
        }

        spans
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::render::buffer::{strip_ansi, Buffer};

    // --- format_row_content --------------------------------------------------

    #[test]
    fn test_format_row_content_zero_width() {
        let out = format_row_content(0, "hello", None, false, false);
        assert_eq!(out, "");
    }

    #[test]
    fn test_format_row_content_empty_text() {
        let out = format_row_content(80, "", None, false, false);
        assert_eq!(strip_ansi(&out), "");
    }

    #[test]
    fn test_format_row_content_plain_text() {
        let out = format_row_content(80, "hello world", None, false, false);
        assert_eq!(strip_ansi(&out), "hello world");
    }

    #[test]
    fn test_format_row_content_border() {
        // Border string uses box-drawing chars; color styling applied.
        let border = "─".repeat(39);
        let out = format_row_content(40, &border, Some(Color::DarkCyan), false, false);
        assert_eq!(strip_ansi(&out), border);
    }

    #[test]
    fn test_format_row_content_truncates_to_max_cells() {
        // width 6 -> max_cells 5
        let out = format_row_content(6, "abcdefghij", None, false, false);
        assert_eq!(strip_ansi(&out), "abcde");
    }

    // --- format_spans_content ------------------------------------------------

    #[test]
    fn test_format_spans_content_empty_spans() {
        let out = format_spans_content(80, vec![]);
        assert_eq!(out, "");
    }

    #[test]
    fn test_format_spans_content_zero_width() {
        let out = format_spans_content(0, vec![ansi::Span::new("hello")]);
        assert_eq!(out, "");
    }

    #[test]
    fn test_format_spans_content_plain() {
        let spans = vec![
            ansi::Span::new(" ["),
            ansi::Span::new("READ"),
            ansi::Span::new("]"),
        ];
        let out = format_spans_content(80, spans);
        assert_eq!(strip_ansi(&out), " [READ]");
    }

    #[test]
    fn test_format_spans_content_styled() {
        // Styling is stripped by strip_ansi; only text content matters.
        let spans = vec![
            ansi::Span::new(" ["),
            ansi::Span::new("WRITE").color(Color::DarkYellow),
            ansi::Span::new("]"),
            ansi::Span::new(" • ").dim(),
            ansi::Span::new("project"),
        ];
        let out = format_spans_content(80, spans);
        assert_eq!(strip_ansi(&out), " [WRITE] • project");
    }

    // --- Buffer snapshot via to_plain_lines ----------------------------------

    #[test]
    fn test_buffer_snapshot_input_box() {
        // Simulate a minimal input box: top border, one content row, bottom border.
        let width = 20u16;
        let border = "─".repeat(19);
        let mut buf = Buffer::new(width as usize, 3);
        buf.set_row(
            0,
            format_row_content(width, &border, Some(Color::DarkCyan), false, false),
        );
        buf.set_row(1, format_row_content(width, "› hello", None, false, false));
        buf.set_row(
            2,
            format_row_content(width, &border, Some(Color::DarkCyan), false, false),
        );

        let lines = buf.to_plain_lines();
        assert_eq!(lines[0], border, "top border");
        assert_eq!(lines[1], "› hello", "input content");
        assert_eq!(lines[2], border, "bottom border");
    }

    #[test]
    fn test_buffer_snapshot_status_line() {
        let width = 40u16;
        let spans = vec![
            ansi::Span::new(" ["),
            ansi::Span::new("READ").color(Color::DarkCyan),
            ansi::Span::new("]"),
            ansi::Span::new(" • ").dim(),
            ansi::Span::new("myproject"),
        ];
        let content = format_spans_content(width, spans);

        let mut buf = Buffer::new(width as usize, 1);
        buf.set_row(0, content);

        let lines = buf.to_plain_lines();
        assert_eq!(lines[0], " [READ] • myproject");
    }
}
