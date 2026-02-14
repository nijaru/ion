//! RNK-backed renderer for the bottom UI area.

use crate::tool::ToolMode;
use crate::tui::composer::{build_visual_lines, ComposerState};
use crate::tui::render::{CONTINUATION, INPUT_MARGIN, PROMPT, PROMPT_WIDTH};
use crate::tui::rnk_text::render_truncated_text_line;
use crate::tui::util::{format_cost, format_elapsed, format_tokens, truncate_to_display_width};
use crate::tui::App;
use crossterm::cursor::MoveTo;
use crossterm::execute;
use crossterm::terminal::{Clear, ClearType};
use rnk::components::{Span, Text};
use rnk::core::Color as RnkColor;
use std::io::Write;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub(crate) struct BottomUiFrame {
    pub progress_row: u16,
    pub progress_height: u16,
    pub input_row: u16,
    pub input_height: u16,
    pub status_row: u16,
    pub width: u16,
    pub show_progress_status: bool,
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

    render_truncated_text_line(line, max_cells)
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
    let rendered = render_truncated_text_line(Text::spans(spans), max_cells);
    write!(w, "{rendered}")?;
    Ok(())
}

impl App {
    pub(crate) fn progress_gap_rows(&self) -> u16 {
        0
    }

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

    fn status_line_spans(&self, width: u16) -> Vec<Span> {
        let max_cells = width.saturating_sub(1) as usize;
        if max_cells == 0 {
            return vec![];
        }

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
            ToolMode::Read => ("READ", RnkColor::Cyan),
            ToolMode::Write => ("WRITE", RnkColor::Yellow),
        };
        let think = self.thinking_level.label();
        let branch = self.git_branch.as_deref();
        let cost_text = format_cost(self.session_cost);

        let (pct_text, detail_text) = match self.token_usage {
            Some((used, max)) if max > 0 => {
                let pct = (used * 100) / max;
                (
                    format!("{pct}%"),
                    Some(format!("({}/{})", format_tokens(used), format_tokens(max))),
                )
            }
            Some((used, _)) if used > 0 => (format_tokens(used), None),
            _ => (String::new(), None),
        };

        // Segment widths (each includes its own " • " separator prefix).
        // Drop order: detail → model → diff stats → branch.
        // Always shown: mode, short %, cost, project.
        let mode_w = mode_label.len() + 3; // " [MODE]"
        let think_w = if think.is_empty() { 0 } else { 1 + think.len() };
        let model_seg = 3 + model_name.len() + think_w; // " • model think"
        let think_seg = if think.is_empty() { 0 } else { 3 + think.len() }; // standalone
        let pct_seg = if pct_text.is_empty() {
            0
        } else {
            3 + pct_text.len()
        };
        let detail_extra = detail_text.as_ref().map_or(0, |d| 1 + d.len());
        let cost_seg = 3 + cost_text.len();
        let branch_extra = branch.map_or(0, |b| 3 + b.len()); // " • b"
        let diff_stat = self.git_diff_stat;
        let diff_texts = diff_stat.map(|(ins, del)| (format!("+{ins}"), format!("-{del}")));
        let diff_extra = diff_texts
            .as_ref()
            .map_or(0, |(i, d)| 2 + i.len() + d.len()); // " +N/-M"
        let proj_seg = 3 + project.len();

        // Total width at each drop level.
        let w0 =
            mode_w + model_seg + pct_seg + detail_extra + cost_seg + proj_seg + branch_extra + diff_extra;
        let w1 = mode_w + model_seg + pct_seg + cost_seg + proj_seg + branch_extra + diff_extra;
        let w2 = mode_w + think_seg + pct_seg + cost_seg + proj_seg + branch_extra + diff_extra;
        let w3 = mode_w + think_seg + pct_seg + cost_seg + proj_seg + branch_extra;

        let (show_model, show_detail, show_branch, show_diff) = if w0 <= max_cells {
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

        // Build spans.
        let mut spans = vec![
            Span::new(" ["),
            Span::new(mode_label).color(mode_color),
            Span::new("]"),
        ];

        if show_model {
            spans.push(Span::new(" • "));
            spans.push(Span::new(model_name));
            if !think.is_empty() {
                spans.push(Span::new(" "));
                spans.push(Span::new(think).color(RnkColor::Magenta));
            }
        } else if !think.is_empty() {
            spans.push(Span::new(" • "));
            spans.push(Span::new(think).color(RnkColor::Magenta));
        }

        if !pct_text.is_empty() {
            let pct_color = match self.token_usage {
                Some((used, max)) if max > 0 => {
                    let pct = (used * 100) / max;
                    if pct >= 80 {
                        Some(RnkColor::Red)
                    } else if pct >= 50 {
                        Some(RnkColor::Yellow)
                    } else {
                        Some(RnkColor::Green)
                    }
                }
                _ => None,
            };
            spans.push(Span::new(" • ").dim());
            let mut pct_span = Span::new(&pct_text);
            if let Some(c) = pct_color {
                pct_span = pct_span.color(c);
            }
            spans.push(pct_span);
            if show_detail
                && let Some(ref detail) = detail_text
            {
                spans.push(Span::new(format!(" {detail}")).dim());
            }
        }

        spans.push(Span::new(format!(" • {cost_text}")).dim());

        spans.push(Span::new(" • ").dim());
        spans.push(Span::new(project));
        if show_branch
            && let Some(b) = branch
        {
            spans.push(Span::new(" • ").dim());
            spans.push(Span::new(b).color(RnkColor::Cyan));
            if show_diff
                && let Some((ref ins, ref del)) = diff_texts
            {
                spans.push(Span::new(format!(" {ins}")).color(RnkColor::Green));
                spans.push(Span::new("/").dim());
                spans.push(Span::new(del).color(RnkColor::Red));
            }
        }

        spans
    }
}
