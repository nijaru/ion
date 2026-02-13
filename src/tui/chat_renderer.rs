use crate::tui::highlight;
use crate::tui::message_list::{MessagePart, Sender};
use crate::tui::terminal::{Color, LineBuilder, StyledLine, StyledSpan, TextStyle};
use crate::tui::{QUEUED_PREVIEW_LINES, sanitize_for_display};
use unicode_width::UnicodeWidthChar;

pub struct ChatRenderer;

impl ChatRenderer {
    #[allow(clippy::too_many_lines)]
    pub fn build_lines(
        entries: &[crate::tui::message_list::MessageEntry],
        queued: Option<&Vec<String>>,
        wrap_width: usize,
    ) -> Vec<StyledLine> {
        let mut chat_lines = Vec::new();

        for entry in entries {
            let mut entry_lines = Vec::new();
            match entry.sender {
                Sender::User => {
                    let mut combined = String::new();
                    for part in &entry.parts {
                        if let MessagePart::Text(text) = part {
                            combined.push_str(text);
                        }
                    }
                    // Sanitize (tabs, control chars) without trimming content
                    let combined = sanitize_for_display(&combined);
                    let prefix = "› ";
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
                                entry_lines.push(
                                    LineBuilder::new()
                                        .styled(StyledSpan::colored(prefix, Color::Cyan).with_dim())
                                        .styled(StyledSpan::colored(chunk, Color::Cyan).with_dim())
                                        .build(),
                                );
                            } else {
                                entry_lines.push(StyledLine::new(vec![
                                    StyledSpan::colored(chunk, Color::Cyan).with_dim(),
                                ]));
                            }
                            first_line = false;
                        }
                    }
                }
                Sender::Agent => {
                    let mut first_line = true;
                    for part in &entry.parts {
                        match part {
                            MessagePart::Text(text) => {
                                // Sanitize (tabs, control chars) without trimming content
                                let sanitized = sanitize_for_display(text);
                                // Account for 2-char prefix when wrapping
                                let content_width = wrap_width.saturating_sub(2);
                                let highlighted_lines = highlight::highlight_markdown_with_width(
                                    &sanitized,
                                    content_width,
                                );
                                for mut line in highlighted_lines {
                                    if first_line {
                                        line.prepend(StyledSpan::raw("• "));
                                        first_line = false;
                                    } else {
                                        line.prepend(StyledSpan::raw("  "));
                                    }
                                    entry_lines.push(line);
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

                            entry_lines.push(StyledLine::new(vec![
                                tool_prefix.clone(),
                                StyledSpan::bold(tool_name.to_string()),
                                StyledSpan::raw(args.to_string()),
                            ]));
                        } else {
                            entry_lines.push(StyledLine::new(vec![
                                tool_prefix.clone(),
                                StyledSpan::bold(first_line.to_string()),
                            ]));
                        }
                    }

                    for line in lines {
                        if line.trim().is_empty() {
                            continue;
                        }
                        let is_diff_line = is_edit_tool
                            && (line.starts_with('+')
                                || line.starts_with('-')
                                || line.starts_with('@')
                                || line.starts_with(' '));

                        if line.starts_with("⎿ Error:") || line.starts_with("  Error:") {
                            entry_lines.push(StyledLine::new(vec![
                                StyledSpan::raw("  "),
                                StyledSpan::colored(line.to_string(), Color::Red),
                            ]));
                        } else if line.starts_with("⎿") || line.starts_with("  … +") {
                            entry_lines.push(StyledLine::new(vec![
                                StyledSpan::raw("  "),
                                StyledSpan::dim(line.to_string()),
                            ]));
                        } else if is_diff_line {
                            let mut highlighted = highlight::highlight_diff_line(line);
                            highlighted.prepend(StyledSpan::raw("    "));
                            entry_lines.push(highlighted);
                        } else if line.contains("\x1b[") {
                            // Parse ANSI escape sequences
                            let parsed = parse_ansi_line(line);
                            let mut padded = StyledLine::new(vec![StyledSpan::raw("  ")]);
                            padded.extend(parsed);
                            entry_lines.push(padded);
                        } else if let Some(syntax) = syntax_name {
                            let code_line = line.strip_prefix("  ").unwrap_or(line);
                            let mut highlighted = highlight::highlight_line(code_line, syntax);
                            highlighted.prepend(StyledSpan::raw("    "));
                            entry_lines.push(highlighted);
                        } else {
                            entry_lines.push(StyledLine::new(vec![
                                StyledSpan::raw("  "),
                                StyledSpan::dim(line.to_string()),
                            ]));
                        }
                    }
                }
                Sender::System => {
                    let content = entry.content_as_markdown();
                    if content.lines().count() <= 1 {
                        if content.starts_with("Error:") {
                            entry_lines.push(StyledLine::colored(content.to_string(), Color::Red));
                        } else if content.starts_with("Warning:") {
                            entry_lines
                                .push(StyledLine::colored(content.to_string(), Color::Yellow));
                        } else {
                            let text = format!("[{content}]");
                            entry_lines.push(StyledLine::dim(text));
                        }
                    } else {
                        // Use our new markdown renderer for multi-line system messages
                        let md_lines = highlight::render_markdown(content);
                        for mut line in md_lines {
                            line.prepend(StyledSpan::raw("  "));
                            entry_lines.push(line);
                        }
                    }
                }
            }
            trim_leading_blank_lines(&mut entry_lines);
            trim_trailing_empty_lines(&mut entry_lines);
            chat_lines.extend(entry_lines);
            chat_lines.push(StyledLine::empty());
        }

        if let Some(queue) = queued {
            for queued_msg in queue {
                let mut entry_lines = Vec::new();
                let lines: Vec<&str> = queued_msg.lines().collect();
                let shown = lines.len().min(QUEUED_PREVIEW_LINES);
                for (idx, line) in lines.iter().take(shown).enumerate() {
                    let prefix = if idx == 0 { " › " } else { "   " };
                    entry_lines.push(StyledLine::new(vec![
                        StyledSpan::dim(prefix),
                        StyledSpan::dim((*line).to_string()).with_italic(),
                    ]));
                }
                if lines.len() > shown {
                    entry_lines.push(StyledLine::new(vec![
                        StyledSpan::dim("   "),
                        StyledSpan::dim("…").with_italic(),
                    ]));
                }
                trim_leading_blank_lines(&mut entry_lines);
                trim_trailing_empty_lines(&mut entry_lines);
                chat_lines.extend(entry_lines);
                chat_lines.push(StyledLine::empty());
            }
        }

        collapse_blank_runs(&mut chat_lines);

        if wrap_width == 0 {
            return chat_lines;
        }

        let mut wrapped = Vec::new();
        for line in chat_lines {
            if line.is_empty() {
                wrapped.push(line);
                continue;
            }
            let text_width: usize = styled_line_text(&line)
                .chars()
                .filter_map(UnicodeWidthChar::width)
                .sum();
            if text_width <= wrap_width {
                wrapped.push(line);
                continue;
            }
            wrapped.extend(wrap_styled_line(&line, wrap_width));
        }

        wrapped
    }
}

/// Parse ANSI escape sequences and convert to `StyledLine`.
/// Simple SGR parser that handles common formatting codes.
fn parse_ansi_line(input: &str) -> StyledLine {
    let mut spans = Vec::new();
    let mut current_style = TextStyle::default();
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
                        if let Some(next_char) = chars.next() {
                            params.push(next_char);
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }

                // Get the command character
                if let Some(cmd) = chars.next()
                    && cmd == 'm'
                {
                    // SGR sequence - apply styles
                    let parts: Vec<&str> = if params.is_empty() {
                        vec!["0"]
                    } else {
                        params.split(';').collect()
                    };

                    let mut i = 0usize;
                    while i < parts.len() {
                        match parts[i] {
                            "0" | "" => current_style = TextStyle::default(),
                            "1" => current_style.bold = true,
                            "2" => current_style.dim = true,
                            "3" => current_style.italic = true,
                            "4" => current_style.underlined = true,
                            "7" => current_style.reverse = true,
                            "9" => current_style.crossed_out = true,
                            "22" => {
                                current_style.bold = false;
                                current_style.dim = false;
                            }
                            "23" => current_style.italic = false,
                            "24" => current_style.underlined = false,
                            "27" => current_style.reverse = false,
                            "29" => current_style.crossed_out = false,
                            "30" => current_style.foreground_color = Some(Color::Black),
                            "31" => current_style.foreground_color = Some(Color::DarkRed),
                            "32" => current_style.foreground_color = Some(Color::DarkGreen),
                            "33" => current_style.foreground_color = Some(Color::DarkYellow),
                            "34" => current_style.foreground_color = Some(Color::DarkBlue),
                            "35" => current_style.foreground_color = Some(Color::DarkMagenta),
                            "36" => current_style.foreground_color = Some(Color::DarkCyan),
                            "37" => current_style.foreground_color = Some(Color::Grey),
                            "39" => current_style.foreground_color = None,
                            "40" => current_style.background_color = Some(Color::Black),
                            "41" => current_style.background_color = Some(Color::DarkRed),
                            "42" => current_style.background_color = Some(Color::DarkGreen),
                            "43" => current_style.background_color = Some(Color::DarkYellow),
                            "44" => current_style.background_color = Some(Color::DarkBlue),
                            "45" => current_style.background_color = Some(Color::DarkMagenta),
                            "46" => current_style.background_color = Some(Color::DarkCyan),
                            "47" => current_style.background_color = Some(Color::Grey),
                            "49" => current_style.background_color = None,
                            "90" => current_style.foreground_color = Some(Color::DarkGrey),
                            "91" => current_style.foreground_color = Some(Color::Red),
                            "92" => current_style.foreground_color = Some(Color::Green),
                            "93" => current_style.foreground_color = Some(Color::Yellow),
                            "94" => current_style.foreground_color = Some(Color::Blue),
                            "95" => current_style.foreground_color = Some(Color::Magenta),
                            "96" => current_style.foreground_color = Some(Color::Cyan),
                            "97" => current_style.foreground_color = Some(Color::White),
                            "100" => current_style.background_color = Some(Color::DarkGrey),
                            "101" => current_style.background_color = Some(Color::Red),
                            "102" => current_style.background_color = Some(Color::Green),
                            "103" => current_style.background_color = Some(Color::Yellow),
                            "104" => current_style.background_color = Some(Color::Blue),
                            "105" => current_style.background_color = Some(Color::Magenta),
                            "106" => current_style.background_color = Some(Color::Cyan),
                            "107" => current_style.background_color = Some(Color::White),
                            "38" | "48" => {
                                let is_fg = parts[i] == "38";
                                if i + 1 < parts.len() && parts[i + 1] == "5" && i + 2 < parts.len()
                                {
                                    if let Ok(code) = parts[i + 2].parse::<u8>() {
                                        if is_fg {
                                            current_style.foreground_color =
                                                Some(Color::AnsiValue(code));
                                        } else {
                                            current_style.background_color =
                                                Some(Color::AnsiValue(code));
                                        }
                                    }
                                    i += 2;
                                } else if i + 1 < parts.len()
                                    && parts[i + 1] == "2"
                                    && i + 4 < parts.len()
                                {
                                    let parsed = (
                                        parts[i + 2].parse::<u8>(),
                                        parts[i + 3].parse::<u8>(),
                                        parts[i + 4].parse::<u8>(),
                                    );
                                    if let (Ok(r), Ok(g), Ok(b)) = parsed {
                                        let color = Color::Rgb { r, g, b };
                                        if is_fg {
                                            current_style.foreground_color = Some(color);
                                        } else {
                                            current_style.background_color = Some(color);
                                        }
                                    }
                                    i += 4;
                                }
                            }
                            _ => {}
                        }
                        i += 1;
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

    let line_width: usize = line.chars().filter_map(UnicodeWidthChar::width).sum();
    if line_width <= width {
        return vec![line.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;

    for word in split_words(line) {
        let word_width: usize = word.chars().filter_map(UnicodeWidthChar::width).sum();

        if current_width + word_width <= width {
            current.push_str(word);
            current_width += word_width;
        } else if word_width <= width && !current.is_empty() {
            // Word fits on a fresh line - trim trailing space from current
            chunks.push(current.trim_end().to_string());
            current = word.to_string();
            current_width = word_width;
        } else {
            // Word wider than line or first word - character-break
            for ch in word.chars() {
                let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                if current_width + cw > width && !current.is_empty() {
                    chunks.push(current);
                    current = String::new();
                    current_width = 0;
                }
                current.push(ch);
                current_width += cw;
            }
        }
    }
    if !current.is_empty() || chunks.is_empty() {
        chunks.push(current);
    }

    chunks
}

/// Split text into alternating word and space segments.
fn split_words(text: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut in_space = text.starts_with(' ');

    for (i, ch) in text.char_indices() {
        let is_space = ch == ' ';
        if is_space != in_space {
            if start < i {
                segments.push(&text[start..i]);
            }
            start = i;
            in_space = is_space;
        }
    }
    if start < text.len() {
        segments.push(&text[start..]);
    }
    segments
}

fn trim_trailing_empty_lines(lines: &mut Vec<StyledLine>) {
    while lines.last().is_some_and(line_is_blank) {
        lines.pop();
    }
}

fn trim_leading_blank_lines(lines: &mut Vec<StyledLine>) {
    let count = lines.iter().take_while(|l| line_is_blank(l)).count();
    if count > 0 {
        lines.drain(..count);
    }
}

fn line_is_blank(line: &StyledLine) -> bool {
    if line.spans.is_empty() {
        return true;
    }
    line.spans
        .iter()
        .all(|span| span.content.chars().all(char::is_whitespace))
}

fn collapse_blank_runs(lines: &mut Vec<StyledLine>) {
    let mut out = Vec::with_capacity(lines.len());
    let mut prev_blank = false;
    for line in lines.iter() {
        let blank = line_is_blank(line);
        if blank && prev_blank {
            continue;
        }
        out.push(line.clone());
        prev_blank = blank;
    }
    *lines = out;
}

fn styled_line_text(line: &StyledLine) -> String {
    let mut out = String::new();
    for span in &line.spans {
        out.push_str(&span.content);
    }
    out
}

fn continuation_indent_width(text: &str) -> usize {
    let indent = text.chars().take_while(|c| *c == ' ').count();
    let trimmed = text[indent..].trim_end();

    if trimmed.starts_with("* ")
        || trimmed.starts_with("- ")
        || trimmed.starts_with("+ ")
        || trimmed.starts_with("> ")
    {
        return indent + 2;
    }

    if trimmed.starts_with('#') {
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if trimmed.chars().nth(hashes) == Some(' ') {
            return indent + hashes + 1;
        }
    }

    let mut digits = 0usize;
    for ch in trimmed.chars() {
        if ch.is_ascii_digit() {
            digits += 1;
        } else {
            break;
        }
    }
    if digits > 0 {
        let rest = trimmed.chars().skip(digits).collect::<String>();
        if rest.starts_with(". ") {
            return indent + digits + 2;
        }
    }

    indent
}

fn wrap_styled_line(line: &StyledLine, width: usize) -> Vec<StyledLine> {
    if width == 0 || line.is_empty() {
        return vec![line.clone()];
    }

    let text = styled_line_text(line);
    let text_width: usize = text.chars().filter_map(UnicodeWidthChar::width).sum();
    if text_width <= width {
        return vec![line.clone()];
    }

    let indent_width = continuation_indent_width(&text).min(width.saturating_sub(1));
    let indent_prefix = " ".repeat(indent_width);

    // Flatten spans into (char, style) pairs
    let flat: Vec<(char, TextStyle)> = line
        .spans
        .iter()
        .flat_map(|span| span.content.chars().map(move |ch| (ch, span.style)))
        .collect();

    // Split into word/space segments, tracking char indices into flat
    struct Segment {
        start: usize,
        end: usize,
        width: usize,
        is_space: bool,
    }
    let mut segments: Vec<Segment> = Vec::new();
    let mut seg_start = 0;
    let mut seg_width = 0usize;
    let mut in_space = flat.first().is_some_and(|(ch, _)| *ch == ' ');

    for (i, &(ch, _)) in flat.iter().enumerate() {
        let is_space = ch == ' ';
        if is_space != in_space && seg_start < i {
            segments.push(Segment {
                start: seg_start,
                end: i,
                width: seg_width,
                is_space: in_space,
            });
            seg_start = i;
            seg_width = 0;
            in_space = is_space;
        }
        seg_width += UnicodeWidthChar::width(ch).unwrap_or(0);
    }
    if seg_start < flat.len() {
        segments.push(Segment {
            start: seg_start,
            end: flat.len(),
            width: seg_width,
            is_space: in_space,
        });
    }

    // Greedily place segments on lines
    let mut lines: Vec<StyledLine> = Vec::new();
    let mut current_chars: Vec<(char, TextStyle)> = Vec::new();
    let mut current_width = 0usize;

    let effective_width = if indent_width > 0 {
        width.saturating_sub(indent_width)
    } else {
        width
    };

    for seg in &segments {
        if seg.is_space {
            if current_width + seg.width <= width {
                current_chars.extend_from_slice(&flat[seg.start..seg.end]);
                current_width += seg.width;
            }
            // Drop spaces at line break
            continue;
        }

        // Word segment
        if current_width + seg.width <= width {
            current_chars.extend_from_slice(&flat[seg.start..seg.end]);
            current_width += seg.width;
        } else if seg.width <= effective_width && !current_chars.is_empty() {
            // Trim trailing spaces from current line
            while current_chars.last().is_some_and(|(ch, _)| *ch == ' ') {
                current_chars.pop();
            }
            lines.push(chars_to_styled_line(&current_chars));
            current_chars.clear();
            current_width = 0;
            if indent_width > 0 {
                for ich in indent_prefix.chars() {
                    current_chars.push((ich, TextStyle::default()));
                }
                current_width = indent_width;
            }
            current_chars.extend_from_slice(&flat[seg.start..seg.end]);
            current_width += seg.width;
        } else {
            // Word wider than line - character-break
            for &(ch, style) in &flat[seg.start..seg.end] {
                let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                if current_width + cw > width && !current_chars.is_empty() {
                    lines.push(chars_to_styled_line(&current_chars));
                    current_chars.clear();
                    current_width = 0;
                    if indent_width > 0 {
                        for ich in indent_prefix.chars() {
                            current_chars.push((ich, TextStyle::default()));
                        }
                        current_width = indent_width;
                    }
                }
                current_chars.push((ch, style));
                current_width += cw;
            }
        }
    }

    if !current_chars.is_empty() {
        lines.push(chars_to_styled_line(&current_chars));
    }

    if lines.is_empty() {
        vec![StyledLine::empty()]
    } else {
        lines
    }
}

fn chars_to_styled_line(chars: &[(char, TextStyle)]) -> StyledLine {
    let mut spans: Vec<StyledSpan> = Vec::new();
    for &(ch, style) in chars {
        if let Some(last) = spans.last_mut()
            && last.style == style
        {
            last.content.push(ch);
        } else {
            spans.push(StyledSpan::new(ch.to_string(), style));
        }
    }
    StyledLine::new(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(line: &StyledLine) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_str())
            .collect::<String>()
    }

    #[test]
    fn parse_ansi_line_resets_styles() {
        let line = parse_ansi_line("\x1b[31merr\x1b[0m ok");
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[0].content, "err");
        assert_eq!(line.spans[0].style.foreground_color, Some(Color::DarkRed));
        assert_eq!(line.spans[1].content, " ok");
        assert_eq!(line.spans[1].style, TextStyle::default());
    }

    #[test]
    fn parse_ansi_line_supports_rgb_and_ansi256() {
        let line = parse_ansi_line("\x1b[38;2;1;2;3mA\x1b[48;5;42mB\x1b[0mC");
        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[0].content, "A");
        assert_eq!(
            line.spans[0].style.foreground_color,
            Some(Color::Rgb { r: 1, g: 2, b: 3 })
        );
        assert_eq!(line.spans[0].style.background_color, None);

        assert_eq!(line.spans[1].content, "B");
        assert_eq!(
            line.spans[1].style.foreground_color,
            Some(Color::Rgb { r: 1, g: 2, b: 3 })
        );
        assert_eq!(
            line.spans[1].style.background_color,
            Some(Color::AnsiValue(42))
        );

        assert_eq!(line.spans[2].content, "C");
        assert_eq!(line.spans[2].style, TextStyle::default());
    }

    #[test]
    fn wrap_styled_line_preserves_styles() {
        let line = StyledLine::new(vec![
            StyledSpan::colored("abc", Color::Green),
            StyledSpan::colored("def", Color::Red),
        ]);
        let wrapped = wrap_styled_line(&line, 4);

        assert_eq!(wrapped.len(), 2);
        assert_eq!(line_text(&wrapped[0]), "abcd");
        assert_eq!(line_text(&wrapped[1]), "ef");

        assert_eq!(
            wrapped[0].spans[0].style.foreground_color,
            Some(Color::Green)
        );
        assert_eq!(wrapped[0].spans[1].style.foreground_color, Some(Color::Red));
        assert_eq!(wrapped[1].spans[0].style.foreground_color, Some(Color::Red));
    }
}
