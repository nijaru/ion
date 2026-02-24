use crate::tui::highlight;
use crate::tui::message_list::{MessagePart, Sender};
use crate::tui::terminal::{Color, LineBuilder, StyledLine, StyledSpan, TextStyle};
use crate::tui::text;
use crate::tui::{QUEUED_PREVIEW_LINES, sanitize_for_display};
use unicode_width::UnicodeWidthChar;

pub struct ChatRenderer;

impl ChatRenderer {
    pub fn build_lines(
        entries: &[crate::tui::message_list::MessageEntry],
        queued: Option<&Vec<String>>,
        wrap_width: usize,
    ) -> Vec<StyledLine> {
        let mut chat_lines = Vec::new();

        for entry in entries {
            let mut entry_lines = match entry.sender {
                Sender::User => {
                    let mut combined = String::new();
                    for part in &entry.parts {
                        if let MessagePart::Text(t) = part {
                            combined.push_str(t);
                        }
                    }
                    render_user_message(&sanitize_for_display(&combined), wrap_width)
                }
                Sender::Agent => {
                    let mut lines = Vec::new();
                    let mut first_segment = true;
                    for part in &entry.parts {
                        if let MessagePart::Text(t) = part {
                            let sanitized = sanitize_for_display(t);
                            let segment = render_agent_text(&sanitized, wrap_width, first_segment);
                            first_segment = first_segment && segment.is_empty();
                            lines.extend(segment);
                        }
                        // Thinking blocks are not rendered in chat
                    }
                    lines
                }
                Sender::Tool => render_tool_entry(&entry.content_as_markdown()),
                Sender::System => render_system_message(&entry.content_as_markdown()),
            };

            trim_leading_blank_lines(&mut entry_lines);
            trim_trailing_empty_lines(&mut entry_lines);
            chat_lines.extend(entry_lines);
            chat_lines.push(StyledLine::empty());
        }

        if let Some(queue) = queued {
            for queued_msg in queue {
                let mut entry_lines = render_queued_preview(queued_msg);
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
            if line.display_width() <= wrap_width {
                wrapped.push(line);
                continue;
            }
            wrapped.extend(wrap_styled_line(&line, wrap_width));
        }

        wrapped
    }
}

/// Render a user message to styled lines.
///
/// Prefixes the first visual line with `› ` (dim cyan). Continuation lines
/// are indented to align with the prefix.
pub(crate) fn render_user_message(text: &str, wrap_width: usize) -> Vec<StyledLine> {
    let prefix = "› ";
    let prefix_len = prefix.chars().count(); // 2
    let available_width = wrap_width.saturating_sub(prefix_len).max(1);

    let mut lines = Vec::new();
    let mut first_line = true;

    for logical_line in text.lines() {
        let line_width = if first_line { available_width } else { wrap_width.max(1) };
        let chunks = text::wrap_text(logical_line, line_width);
        for (idx, chunk) in chunks.into_iter().enumerate() {
            if first_line && idx == 0 {
                lines.push(
                    LineBuilder::new()
                        .styled(StyledSpan::colored(prefix, Color::Cyan).with_dim())
                        .styled(StyledSpan::colored(chunk, Color::Cyan).with_dim())
                        .build(),
                );
            } else {
                lines.push(StyledLine::new(vec![
                    StyledSpan::colored(chunk, Color::Cyan).with_dim(),
                ]));
            }
            first_line = false;
        }
    }
    lines
}

/// Render one text segment from an agent message.
///
/// `has_prefix` controls whether to prepend the `• ` bullet on the first line.
/// Pass `true` for the first segment; `false` for continuation segments.
pub(crate) fn render_agent_text(
    text: &str,
    wrap_width: usize,
    has_prefix: bool,
) -> Vec<StyledLine> {
    let content_width = wrap_width.saturating_sub(2);
    let highlighted_lines = highlight::highlight_markdown_with_width(text, content_width);

    let mut lines = Vec::new();
    let mut first_line = has_prefix;

    for mut line in highlighted_lines {
        if first_line {
            line.prepend(StyledSpan::raw("• "));
            first_line = false;
        } else if !line.is_empty() {
            line.prepend(StyledSpan::raw("  "));
        }
        lines.push(line);
    }
    lines
}

/// Render a tool entry (call header + output lines).
pub(crate) fn render_tool_entry(content: &str) -> Vec<StyledLine> {
    let mut lines = Vec::new();
    let mut content_lines = content.lines();

    let mut syntax_name: Option<&str> = None;
    let mut is_edit_tool = false;

    if let Some(first_line) = content_lines.next() {
        if let Some(paren_pos) = first_line.find('(') {
            let tool_name = &first_line[..paren_pos];
            let args = &first_line[paren_pos..];

            if tool_name == "read" {
                let path = args
                    .trim_start_matches('(')
                    .split(&[',', ')'][..])
                    .next()
                    .unwrap_or("");
                syntax_name = highlight::detect_syntax(path);
            } else if tool_name == "edit" || tool_name == "write" {
                is_edit_tool = true;
            }

            let inner = args.trim_start_matches('(').trim_end_matches(')');
            lines.push(StyledLine::new(vec![
                StyledSpan::raw("• "),
                StyledSpan::bold(tool_name.to_string()),
                StyledSpan::raw("("),
                StyledSpan::colored(inner.to_string(), Color::Cyan),
                StyledSpan::raw(")"),
            ]));
        } else {
            lines.push(StyledLine::new(vec![
                StyledSpan::raw("• "),
                StyledSpan::bold(first_line.to_string()),
            ]));
        }
    }

    for line in content_lines {
        if line.trim().is_empty() {
            continue;
        }
        let is_diff_line = is_edit_tool
            && (line.starts_with('+')
                || line.starts_with('-')
                || line.starts_with('@')
                || line.starts_with(' '));

        if line.starts_with(" ✓") || line.starts_with(" ✗") || line.starts_with(" ⎿") {
            lines.push(StyledLine::new(vec![
                StyledSpan::raw("  "),
                StyledSpan::dim(line.to_string()),
            ]));
        } else if line.starts_with("⎿") || line.starts_with("  … +") {
            lines.push(StyledLine::new(vec![
                StyledSpan::raw("  "),
                StyledSpan::dim(line.to_string()),
            ]));
        } else if is_diff_line {
            let mut highlighted = highlight::highlight_diff_line(line);
            highlighted.prepend(StyledSpan::raw("    "));
            lines.push(highlighted);
        } else if line.contains("\x1b[") {
            let parsed = parse_ansi_line(line);
            let mut padded = StyledLine::new(vec![StyledSpan::raw("  ")]);
            padded.extend(parsed);
            lines.push(padded);
        } else if let Some(syntax) = syntax_name {
            let code_line = line.strip_prefix("  ").unwrap_or(line);
            let mut highlighted = highlight::highlight_line(code_line, syntax);
            highlighted.prepend(StyledSpan::raw("    "));
            lines.push(highlighted);
        } else {
            lines.push(StyledLine::new(vec![
                StyledSpan::raw("  "),
                StyledSpan::dim(line.to_string()),
            ]));
        }
    }

    lines
}

/// Render a system message to styled lines.
pub(crate) fn render_system_message(content: &str) -> Vec<StyledLine> {
    if content.lines().count() <= 1 {
        if content.starts_with("Error:") {
            vec![StyledLine::colored(content.to_string(), Color::Red)]
        } else if content.starts_with("Warning:") {
            vec![StyledLine::colored(content.to_string(), Color::Yellow)]
        } else {
            vec![StyledLine::dim(format!("[{content}]"))]
        }
    } else {
        let md_lines = highlight::render_markdown(content);
        md_lines
            .into_iter()
            .map(|mut line| {
                line.prepend(StyledSpan::raw("  "));
                line
            })
            .collect()
    }
}

/// Render a queued (pending) message preview.
pub(crate) fn render_queued_preview(text: &str) -> Vec<StyledLine> {
    let lines: Vec<&str> = text.lines().collect();
    let shown = lines.len().min(QUEUED_PREVIEW_LINES);
    let mut out = Vec::new();
    for (idx, line) in lines.iter().take(shown).enumerate() {
        let prefix = if idx == 0 { " › " } else { "   " };
        out.push(StyledLine::new(vec![
            StyledSpan::dim(prefix),
            StyledSpan::dim((*line).to_string()).with_italic(),
        ]));
    }
    if lines.len() > shown {
        out.push(StyledLine::new(vec![
            StyledSpan::dim("   "),
            StyledSpan::dim("…").with_italic(),
        ]));
    }
    out
}

/// Parse ANSI SGR escape sequences into a `StyledLine`.
fn parse_ansi_line(input: &str) -> StyledLine {
    let mut spans = Vec::new();
    let mut current_style = TextStyle::default();
    let mut current_text = String::new();
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();

                if !current_text.is_empty() {
                    spans.push(StyledSpan::new(
                        std::mem::take(&mut current_text),
                        current_style,
                    ));
                }

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

                if let Some(cmd) = chars.next()
                    && cmd == 'm'
                {
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
            }
        } else {
            current_text.push(c);
        }
    }

    if !current_text.is_empty() {
        spans.push(StyledSpan::new(current_text, current_style));
    }

    StyledLine::new(spans)
}

fn wrap_styled_line(line: &StyledLine, width: usize) -> Vec<StyledLine> {
    if width == 0 || line.is_empty() {
        return vec![line.clone()];
    }

    let text = line.plain_text();
    let text_width: usize = text.chars().filter_map(UnicodeWidthChar::width).sum();
    if text_width <= width {
        return vec![line.clone()];
    }

    let indent_width = continuation_indent_width(&text).min(width.saturating_sub(1));
    let indent_prefix = " ".repeat(indent_width);

    let flat: Vec<(char, TextStyle)> = line
        .spans
        .iter()
        .flat_map(|span| span.content.chars().map(move |ch| (ch, span.style)))
        .collect();

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

    let mut out: Vec<StyledLine> = Vec::new();
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
            continue;
        }

        if current_width + seg.width <= width {
            current_chars.extend_from_slice(&flat[seg.start..seg.end]);
            current_width += seg.width;
        } else if seg.width <= effective_width && !current_chars.is_empty() {
            while current_chars.last().is_some_and(|(ch, _)| *ch == ' ') {
                current_chars.pop();
            }
            out.push(chars_to_styled_line(&current_chars));
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
            for &(ch, style) in &flat[seg.start..seg.end] {
                let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                if current_width + cw > width && !current_chars.is_empty() {
                    out.push(chars_to_styled_line(&current_chars));
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
        out.push(chars_to_styled_line(&current_chars));
    }

    if out.is_empty() {
        vec![StyledLine::empty()]
    } else {
        out
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
    line.spans.is_empty()
        || line
            .spans
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

fn continuation_indent_width(text: &str) -> usize {
    let indent = text.chars().take_while(|c| *c == ' ').count();
    let trimmed = text[indent..].trim_end();
    indent + marker_indent_width(trimmed)
}

fn marker_indent_width(trimmed: &str) -> usize {
    if trimmed.starts_with("* ")
        || trimmed.starts_with("- ")
        || trimmed.starts_with("+ ")
        || trimmed.starts_with("> ")
    {
        return 2;
    }

    if let Some(rest) = trimmed.strip_prefix("• ") {
        return 2 + marker_indent_width(rest);
    }

    if trimmed.starts_with('#') {
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if trimmed.chars().nth(hashes) == Some(' ') {
            return hashes + 1;
        }
    }

    let digits: usize = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && trimmed[digits..].starts_with(". ") {
        return digits + 2;
    }

    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::message_list::MessageEntry;

    fn user_entry(text: &str) -> MessageEntry {
        MessageEntry::new(Sender::User, text.to_string())
    }

    fn agent_entry(text: &str) -> MessageEntry {
        MessageEntry::new(Sender::Agent, text.to_string())
    }

    fn tool_entry(text: &str) -> MessageEntry {
        MessageEntry::new(Sender::Tool, text.to_string())
    }

    fn system_entry(text: &str) -> MessageEntry {
        MessageEntry::new(Sender::System, text.to_string())
    }

    // --- render_user_message ---

    #[test]
    fn user_message_has_prefix() {
        let lines = render_user_message("hello world", 80);
        assert!(lines[0].plain_text().starts_with("› "));
    }

    #[test]
    fn user_message_content_follows_prefix() {
        let lines = render_user_message("hello", 80);
        assert_eq!(lines[0].plain_text(), "› hello");
    }

    #[test]
    fn user_message_wraps_long_line() {
        let text = "word ".repeat(20);
        let lines = render_user_message(text.trim(), 20);
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(line.display_width() <= 20, "line too wide: {:?}", line.plain_text());
        }
    }

    #[test]
    fn user_message_multiline_first_gets_prefix() {
        let lines = render_user_message("line one\nline two", 80);
        assert!(lines[0].plain_text().starts_with("› "));
        assert!(!lines[1].plain_text().starts_with("› "));
    }

    // --- render_agent_text ---

    #[test]
    fn agent_text_first_segment_gets_bullet() {
        let lines = render_agent_text("hello", 80, true);
        assert!(lines[0].plain_text().starts_with("• "));
    }

    #[test]
    fn agent_text_continuation_gets_indent() {
        let lines = render_agent_text("hello\nworld", 80, true);
        assert!(lines[0].plain_text().starts_with("• "));
        assert!(lines[1].plain_text().starts_with("  "));
    }

    #[test]
    fn agent_text_not_first_segment_no_bullet() {
        let lines = render_agent_text("hello", 80, false);
        assert!(!lines[0].plain_text().starts_with("• "));
    }

    // --- render_tool_entry ---

    #[test]
    fn tool_entry_header_has_bullet() {
        let lines = render_tool_entry("read(/foo/bar.rs)");
        assert!(lines[0].plain_text().starts_with("• "));
    }

    #[test]
    fn tool_entry_header_bold_name() {
        let lines = render_tool_entry("read(/foo/bar.rs)");
        // First span is "• ", second is bold "read"
        assert_eq!(lines[0].spans[1].content, "read");
        assert!(lines[0].spans[1].style.bold);
    }

    #[test]
    fn tool_entry_result_lines_indented() {
        let lines = render_tool_entry("bash(echo hi)\n ✓ exit 0");
        let result_line = lines.iter().find(|l| l.plain_text().contains("exit 0")).unwrap();
        assert!(result_line.plain_text().starts_with("  "));
    }

    // --- render_system_message ---

    #[test]
    fn system_error_is_red() {
        let lines = render_system_message("Error: something failed");
        assert_eq!(lines[0].spans[0].style.foreground_color, Some(Color::Red));
    }

    #[test]
    fn system_warning_is_yellow() {
        let lines = render_system_message("Warning: something odd");
        assert_eq!(lines[0].spans[0].style.foreground_color, Some(Color::Yellow));
    }

    #[test]
    fn system_plain_gets_brackets() {
        let lines = render_system_message("session started");
        assert!(lines[0].plain_text().contains('['));
        assert!(lines[0].plain_text().contains(']'));
    }

    // --- build_lines integration ---

    #[test]
    fn build_lines_user_has_prefix() {
        let entries = vec![user_entry("hello world")];
        let lines = ChatRenderer::build_lines(&entries, None, 80);
        assert!(lines[0].plain_text().starts_with("› "));
    }

    #[test]
    fn build_lines_agent_has_bullet() {
        let entries = vec![agent_entry("first line\nsecond line")];
        let lines = ChatRenderer::build_lines(&entries, None, 80);
        assert!(lines[0].plain_text().starts_with("• "));
        assert!(lines[1].plain_text().starts_with("  "));
    }

    #[test]
    fn build_lines_tool_has_bullet() {
        let entries = vec![tool_entry("read(/foo.rs)")];
        let lines = ChatRenderer::build_lines(&entries, None, 80);
        assert!(lines[0].plain_text().starts_with("• "));
    }

    #[test]
    fn build_lines_entries_separated_by_blank() {
        let entries = vec![user_entry("a"), user_entry("b")];
        let lines = ChatRenderer::build_lines(&entries, None, 80);
        let blank_count = lines.iter().filter(|l| l.is_empty()).count();
        assert!(blank_count >= 1);
    }

    // --- wrap_styled_line ---

    #[test]
    fn wrap_styled_line_preserves_styles() {
        let line = StyledLine::new(vec![
            StyledSpan::colored("abc", Color::Green),
            StyledSpan::colored("def", Color::Red),
        ]);
        let wrapped = wrap_styled_line(&line, 4);

        assert_eq!(wrapped.len(), 2);
        assert_eq!(wrapped[0].plain_text(), "abcd");
        assert_eq!(wrapped[1].plain_text(), "ef");
        assert_eq!(wrapped[0].spans[0].style.foreground_color, Some(Color::Green));
        assert_eq!(wrapped[0].spans[1].style.foreground_color, Some(Color::Red));
        assert_eq!(wrapped[1].spans[0].style.foreground_color, Some(Color::Red));
    }

    #[test]
    fn wrapped_bullet_line_keeps_two_space_continuation_indent() {
        let line = StyledLine::new(vec![StyledSpan::raw(
            "• this line should wrap and keep aligned continuation".to_string(),
        )]);
        let wrapped = wrap_styled_line(&line, 20);
        assert!(wrapped.len() > 1);
        assert!(wrapped[1].plain_text().starts_with("  "));
    }

    #[test]
    fn wrapped_prefixed_markdown_list_keeps_four_space_continuation_indent() {
        let line = StyledLine::new(vec![StyledSpan::raw(
            "• - this list item should wrap and align under list content".to_string(),
        )]);
        let wrapped = wrap_styled_line(&line, 26);
        assert!(wrapped.len() > 1);
        assert!(wrapped[1].plain_text().starts_with("    "));
    }

    #[test]
    fn wrapped_prefixed_ordered_list_keeps_nested_continuation_indent() {
        let line = StyledLine::new(vec![StyledSpan::raw(
            "• 10. this ordered list item should also wrap cleanly".to_string(),
        )]);
        let wrapped = wrap_styled_line(&line, 24);
        assert!(wrapped.len() > 1);
        assert!(wrapped[1].plain_text().starts_with("      "));
    }

    // --- parse_ansi_line ---

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
            line.spans[1].style.background_color,
            Some(Color::AnsiValue(42))
        );
        assert_eq!(line.spans[2].content, "C");
        assert_eq!(line.spans[2].style, TextStyle::default());
    }
}
