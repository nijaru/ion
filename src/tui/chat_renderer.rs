use crate::tui::highlight;
use crate::tui::message_list::{MessagePart, Sender};
use crate::tui::{own_line, strip_ansi, QUEUED_PREVIEW_LINES};
use ratatui::prelude::*;
use ratatui::style::Modifier;

pub struct ChatRenderer;

impl ChatRenderer {
    pub fn build_lines(
        entries: &[crate::tui::message_list::MessageEntry],
        queued: Option<&Vec<String>>,
        wrap_width: usize,
    ) -> Vec<Line<'static>> {
        let mut chat_lines = Vec::new();

        for entry in entries {
            match entry.sender {
                Sender::User => {
                    let mut combined = String::new();
                    for part in &entry.parts {
                        if let MessagePart::Text(text) = part {
                            combined.push_str(text);
                        }
                    }
                    let prefix = "> ";
                    let prefix_len = prefix.chars().count();
                    let available_width = wrap_width.saturating_sub(prefix_len).max(1);
                    let prefix_style = Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::DIM);
                    let text_style = Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::DIM);

                    for line in combined.lines() {
                        for chunk in wrap_line(line, available_width) {
                            chat_lines.push(Line::from(vec![
                                Span::styled(prefix, prefix_style),
                                Span::styled(chunk, text_style),
                            ]));
                        }
                    }
                }
                Sender::Agent => {
                    for part in &entry.parts {
                        match part {
                            MessagePart::Text(text) => {
                                let highlighted_lines =
                                    highlight::highlight_markdown_with_code(text);
                                for line in highlighted_lines {
                                    let mut padded = vec![Span::raw(" ")];
                                    padded.extend(line.spans);
                                    chat_lines.push(Line::from(padded));
                                }
                            }
                            MessagePart::Thinking(thinking) => {
                                let highlighted_lines =
                                    highlight::highlight_markdown_with_code(thinking);
                                for line in highlighted_lines {
                                    let mut padded = vec![Span::raw(" ")];
                                    padded.extend(line.spans.iter().map(|span| {
                                        Span::styled(
                                            span.content.to_string(),
                                            span.style.add_modifier(Modifier::DIM),
                                        )
                                    }));
                                    chat_lines.push(Line::from(padded));
                                }
                            }
                        }
                    }
                }
                Sender::Tool => {
                    let content = entry.content_as_markdown();
                    let has_error = content
                        .lines()
                        .any(|line| line.starts_with("⎿ Error:") || line.starts_with("  Error:"));
                    let tool_prefix = if has_error {
                        Span::styled("• ", Style::default().fg(Color::Red))
                    } else {
                        Span::raw("• ")
                    };
                    let mut lines = content.lines();

                    let mut syntax_name: Option<&str> = None;
                    let mut is_edit_tool = false;

                    if let Some(first_line) = lines.next() {
                        if let Some(paren_pos) = first_line.find('(') {
                            let tool_name = &first_line[..paren_pos];
                            let args = &first_line[paren_pos..];

                            if tool_name == "read" || tool_name == "grep" {
                                let path = args
                                    .trim_start_matches('(')
                                    .split(&[',', ')'][..])
                                    .next()
                                    .unwrap_or("");
                                syntax_name = highlight::detect_syntax(path);
                            } else if tool_name == "edit" || tool_name == "write" {
                                is_edit_tool = true;
                            }

                            chat_lines.push(Line::from(vec![
                                tool_prefix.clone(),
                                Span::styled(tool_name.to_string(), Style::default().bold()),
                                Span::raw(args.to_string()),
                            ]));
                        } else {
                            chat_lines.push(Line::from(vec![
                                tool_prefix.clone(),
                                Span::styled(first_line.to_string(), Style::default().bold()),
                            ]));
                        }
                    }

                    for line in lines {
                        let is_diff_line = is_edit_tool
                            && (line.starts_with('+')
                                || line.starts_with('-')
                                || line.starts_with('@')
                                || line.starts_with(' '));

                        if line.starts_with("⎿ Error:") || line.starts_with("  Error:") {
                            chat_lines.push(Line::from(vec![
                                Span::raw("  "),
                                Span::styled(line.to_string(), Style::default().fg(Color::Red)),
                            ]));
                        } else if line.starts_with("⎿") || line.starts_with("  … +") {
                            chat_lines.push(Line::from(vec![
                                Span::raw("  "),
                                Span::styled(line.to_string(), Style::default().dim()),
                            ]));
                        } else if is_diff_line {
                            let mut highlighted = highlight::highlight_diff_line(line);
                            highlighted.spans.insert(0, Span::raw("    "));
                            chat_lines.push(highlighted);
                        } else if line.contains("\x1b[") {
                            use ansi_to_tui::IntoText;
                            if let Ok(ansi_text) = line.as_bytes().into_text() {
                                for ansi_line in ansi_text.lines {
                                    let mut padded = vec![Span::raw("  ")];
                                    padded.extend(ansi_line.spans.iter().map(|span| {
                                        Span::styled(span.content.to_string(), span.style)
                                    }));
                                    chat_lines.push(Line::from(padded));
                                }
                            } else {
                                chat_lines.push(Line::from(vec![
                                    Span::raw("  "),
                                    Span::raw(strip_ansi(line)),
                                ]));
                            }
                        } else if let Some(syntax) = syntax_name {
                            let code_line = line.strip_prefix("  ").unwrap_or(line);
                            let mut highlighted = highlight::highlight_line(code_line, syntax);
                            highlighted.spans.insert(0, Span::raw("    "));
                            chat_lines.push(own_line(highlighted));
                        } else {
                            chat_lines.push(Line::from(vec![
                                Span::raw("  "),
                                Span::styled(line.to_string(), Style::default().dim()),
                            ]));
                        }
                    }
                }
                Sender::System => {
                    let content = entry.content_as_markdown();
                    if content.lines().count() <= 1 {
                        let trimmed = content.trim();
                        if trimmed.starts_with("Error:") {
                            chat_lines.push(Line::from(vec![Span::styled(
                                trimmed.to_string(),
                                Style::default().fg(Color::Red),
                            )]));
                        } else {
                            let text = format!("[{}]", trimmed);
                            chat_lines.push(Line::from(vec![Span::styled(
                                text,
                                Style::default().dim(),
                            )]));
                        }
                    } else {
                        let md = tui_markdown::from_str(content);
                        for line in &md.lines {
                            let mut padded = vec![Span::raw(" ")];
                            padded.extend(line.spans.iter().map(|span| {
                                Span::styled(span.content.to_string(), span.style)
                            }));
                            chat_lines.push(Line::from(padded));
                        }
                    }
                }
            }
            chat_lines.push(Line::from(""));
        }

        if let Some(queue) = queued {
            let prefix_style = Style::default().dim();
            let queued_style = Style::default().dim().italic();
            for queued in queue.iter() {
                let lines: Vec<&str> = queued.lines().collect();
                let shown = lines.len().min(QUEUED_PREVIEW_LINES);
                for (idx, line) in lines.iter().take(shown).enumerate() {
                    let prefix = if idx == 0 { " > " } else { "   " };
                    chat_lines.push(Line::from(vec![
                        Span::styled(prefix, prefix_style),
                        Span::styled((*line).to_string(), queued_style),
                    ]));
                }
                if lines.len() > shown {
                    chat_lines.push(Line::from(vec![
                        Span::styled("   ", prefix_style),
                        Span::styled("…", queued_style),
                    ]));
                }
                chat_lines.push(Line::from(""));
            }
        }

        if !chat_lines.is_empty() {
            chat_lines.push(Line::from(""));
        }

        chat_lines
    }
}

fn wrap_line(line: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    if line.is_empty() {
        return vec![String::new()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;

    for ch in line.chars() {
        current.push(ch);
        current_len += 1;
        if current_len >= width {
            chunks.push(current);
            current = String::new();
            current_len = 0;
        }
    }

    if !current.is_empty() || chunks.is_empty() {
        chunks.push(current);
    }

    chunks
}
