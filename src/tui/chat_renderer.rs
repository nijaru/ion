use crate::tui::highlight;
use crate::tui::message_list::{MessagePart, Sender};
use crate::tui::terminal::{LineBuilder, StyledLine, StyledSpan};
use crate::tui::{sanitize_for_display, QUEUED_PREVIEW_LINES};
use crossterm::style::Color;

pub struct ChatRenderer;

impl ChatRenderer {
    pub fn build_lines(
        entries: &[crate::tui::message_list::MessageEntry],
        queued: Option<&Vec<String>>,
        wrap_width: usize,
    ) -> Vec<StyledLine> {
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
                    // Sanitize (tabs, control chars) and trim
                    let combined = sanitize_for_display(combined.trim());
                    let prefix = "> ";
                    let prefix_len = prefix.chars().count();
                    let available_width = wrap_width.saturating_sub(prefix_len).max(1);

                    let mut first_line = true;
                    for line in combined.lines() {
                        let line_width = if first_line {
                            available_width
                        } else {
                            wrap_width.max(1)
                        };
                        let chunks = wrap_line(line, line_width);
                        for (idx, chunk) in chunks.into_iter().enumerate() {
                            if first_line && idx == 0 {
                                chat_lines.push(
                                    LineBuilder::new()
                                        .styled(StyledSpan::colored(prefix, Color::Cyan).with_dim())
                                        .styled(StyledSpan::colored(chunk, Color::Cyan).with_dim())
                                        .build(),
                                );
                            } else {
                                chat_lines.push(StyledLine::new(vec![StyledSpan::colored(
                                    chunk,
                                    Color::Cyan,
                                )
                                .with_dim()]));
                            }
                            first_line = false;
                        }
                    }
                }
                Sender::Agent => {
                    for part in &entry.parts {
                        match part {
                            MessagePart::Text(text) => {
                                // Sanitize (tabs, control chars) and trim
                                let sanitized = sanitize_for_display(text.trim());
                                let highlighted_lines =
                                    highlight::highlight_markdown_with_code(&sanitized);
                                for mut line in highlighted_lines {
                                    line.prepend(StyledSpan::raw(" "));
                                    chat_lines.push(line);
                                }
                            }
                            MessagePart::Thinking(_) => {
                                // Don't render thinking content in chat
                                // Progress bar shows "thinking" or "thought for Xs" instead
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
                        StyledSpan::colored("• ", Color::Red)
                    } else {
                        StyledSpan::raw("• ")
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

                            chat_lines.push(StyledLine::new(vec![
                                tool_prefix.clone(),
                                StyledSpan::bold(tool_name.to_string()),
                                StyledSpan::raw(args.to_string()),
                            ]));
                        } else {
                            chat_lines.push(StyledLine::new(vec![
                                tool_prefix.clone(),
                                StyledSpan::bold(first_line.to_string()),
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
                            chat_lines.push(StyledLine::new(vec![
                                StyledSpan::raw("  "),
                                StyledSpan::colored(line.to_string(), Color::Red),
                            ]));
                        } else if line.starts_with("⎿") || line.starts_with("  … +") {
                            chat_lines.push(StyledLine::new(vec![
                                StyledSpan::raw("  "),
                                StyledSpan::dim(line.to_string()),
                            ]));
                        } else if is_diff_line {
                            let mut highlighted = highlight::highlight_diff_line(line);
                            highlighted.prepend(StyledSpan::raw("    "));
                            chat_lines.push(highlighted);
                        } else if line.contains("\x1b[") {
                            // Parse ANSI escape sequences
                            let parsed = parse_ansi_line(line);
                            let mut padded = StyledLine::new(vec![StyledSpan::raw("  ")]);
                            padded.extend(parsed);
                            chat_lines.push(padded);
                        } else if let Some(syntax) = syntax_name {
                            let code_line = line.strip_prefix("  ").unwrap_or(line);
                            let mut highlighted = highlight::highlight_line(code_line, syntax);
                            highlighted.prepend(StyledSpan::raw("    "));
                            chat_lines.push(highlighted);
                        } else {
                            chat_lines.push(StyledLine::new(vec![
                                StyledSpan::raw("  "),
                                StyledSpan::dim(line.to_string()),
                            ]));
                        }
                    }
                }
                Sender::System => {
                    let content = entry.content_as_markdown().trim();
                    if content.lines().count() <= 1 {
                        if content.starts_with("Error:") {
                            chat_lines.push(StyledLine::colored(content.to_string(), Color::Red));
                        } else {
                            let text = format!("[{}]", content);
                            chat_lines.push(StyledLine::dim(text));
                        }
                    } else {
                        // Use our new markdown renderer for multi-line system messages
                        let md_lines = highlight::render_markdown(content);
                        for mut line in md_lines {
                            line.prepend(StyledSpan::raw(" "));
                            chat_lines.push(line);
                        }
                    }
                }
            }
            chat_lines.push(StyledLine::empty());
        }

        if let Some(queue) = queued {
            for queued_msg in queue.iter() {
                let lines: Vec<&str> = queued_msg.lines().collect();
                let shown = lines.len().min(QUEUED_PREVIEW_LINES);
                for (idx, line) in lines.iter().take(shown).enumerate() {
                    let prefix = if idx == 0 { " > " } else { "   " };
                    chat_lines.push(StyledLine::new(vec![
                        StyledSpan::dim(prefix),
                        StyledSpan::dim((*line).to_string()).with_italic(),
                    ]));
                }
                if lines.len() > shown {
                    chat_lines.push(StyledLine::new(vec![
                        StyledSpan::dim("   "),
                        StyledSpan::dim("…").with_italic(),
                    ]));
                }
                chat_lines.push(StyledLine::empty());
            }
        }

        chat_lines
    }
}

/// Parse ANSI escape sequences and convert to StyledLine.
/// Simple SGR parser that handles common formatting codes.
fn parse_ansi_line(input: &str) -> StyledLine {
    use crossterm::style::{Attribute, ContentStyle};

    let mut spans = Vec::new();
    let mut current_style = ContentStyle::default();
    let mut current_text = String::new();
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Check for CSI sequence
            if chars.peek() == Some(&'[') {
                chars.next(); // consume '['

                // Flush current text
                if !current_text.is_empty() {
                    spans.push(StyledSpan::new(
                        std::mem::take(&mut current_text),
                        current_style,
                    ));
                }

                // Parse the SGR parameters
                let mut params = String::new();
                while let Some(&pc) = chars.peek() {
                    if pc.is_ascii_digit() || pc == ';' {
                        params.push(chars.next().unwrap());
                    } else {
                        break;
                    }
                }

                // Get the command character
                if let Some(cmd) = chars.next()
                    && cmd == 'm'
                {
                    // SGR sequence - apply styles
                    for param in params.split(';') {
                        match param {
                            "0" | "" => current_style = ContentStyle::default(),
                            "1" => current_style.attributes.set(Attribute::Bold),
                            "2" => current_style.attributes.set(Attribute::Dim),
                            "3" => current_style.attributes.set(Attribute::Italic),
                            "4" => current_style.attributes.set(Attribute::Underlined),
                            "7" => current_style.attributes.set(Attribute::Reverse),
                            "9" => current_style.attributes.set(Attribute::CrossedOut),
                            "22" => {
                                current_style.attributes.unset(Attribute::Bold);
                                current_style.attributes.unset(Attribute::Dim);
                            }
                            "23" => current_style.attributes.unset(Attribute::Italic),
                            "24" => current_style.attributes.unset(Attribute::Underlined),
                            "27" => current_style.attributes.unset(Attribute::Reverse),
                            "29" => current_style.attributes.unset(Attribute::CrossedOut),
                            "30" => current_style.foreground_color = Some(Color::Black),
                            "31" => current_style.foreground_color = Some(Color::DarkRed),
                            "32" => current_style.foreground_color = Some(Color::DarkGreen),
                            "33" => current_style.foreground_color = Some(Color::DarkYellow),
                            "34" => current_style.foreground_color = Some(Color::DarkBlue),
                            "35" => current_style.foreground_color = Some(Color::DarkMagenta),
                            "36" => current_style.foreground_color = Some(Color::DarkCyan),
                            "37" => current_style.foreground_color = Some(Color::Grey),
                            "39" => current_style.foreground_color = None,
                            "90" => current_style.foreground_color = Some(Color::DarkGrey),
                            "91" => current_style.foreground_color = Some(Color::Red),
                            "92" => current_style.foreground_color = Some(Color::Green),
                            "93" => current_style.foreground_color = Some(Color::Yellow),
                            "94" => current_style.foreground_color = Some(Color::Blue),
                            "95" => current_style.foreground_color = Some(Color::Magenta),
                            "96" => current_style.foreground_color = Some(Color::Cyan),
                            "97" => current_style.foreground_color = Some(Color::White),
                            _ => {} // Ignore unknown codes
                        }
                    }
                }
                // Ignore other CSI sequences
            } else {
                current_text.push(c);
            }
        } else {
            current_text.push(c);
        }
    }

    // Flush remaining text
    if !current_text.is_empty() {
        spans.push(StyledSpan::new(current_text, current_style));
    }

    StyledLine::new(spans)
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
