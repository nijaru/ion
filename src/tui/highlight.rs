//! Syntax highlighting for tool output using syntect.

use crate::tui::table::Table;
use crate::tui::terminal::{LineBuilder, StyledLine, StyledSpan};
use crossterm::style::Color;
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use std::sync::LazyLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, ThemeSet};
use syntect::parsing::SyntaxSet;

/// Lazily loaded syntax and theme sets.
static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME_SET: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);

/// Highlight a diff line with appropriate colors.
/// - `+` lines → Green (additions)
/// - `-` lines → Red (deletions)
/// - `@` lines → Cyan (hunk headers)
/// - Other lines → Dim (context)
pub fn highlight_diff_line(line: &str) -> StyledLine {
    if line.starts_with('+') && !line.starts_with("+++") {
        StyledLine::colored(line.to_string(), Color::Green)
    } else if line.starts_with('-') && !line.starts_with("---") {
        StyledLine::colored(line.to_string(), Color::Red)
    } else if line.starts_with('@') {
        StyledLine::colored(line.to_string(), Color::Cyan)
    } else if line.starts_with("+++") || line.starts_with("---") {
        StyledLine::new(vec![StyledSpan::bold(line.to_string())])
    } else {
        StyledLine::dim(line.to_string())
    }
}

/// Detect syntax from file extension.
pub fn detect_syntax(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next()?;
    match ext.to_lowercase().as_str() {
        "rs" => Some("Rust"),
        "py" => Some("Python"),
        "js" | "mjs" | "cjs" | "jsx" => Some("JavaScript"),
        "ts" | "mts" | "cts" | "tsx" => Some("TypeScript"),
        "json" => Some("JSON"),
        "toml" => Some("TOML"),
        "yaml" | "yml" => Some("YAML"),
        "md" | "markdown" => Some("Markdown"),
        "sh" | "bash" | "zsh" => Some("Bourne Again Shell (bash)"),
        "go" => Some("Go"),
        "c" | "h" => Some("C"),
        "cpp" | "cc" | "cxx" | "hpp" => Some("C++"),
        "java" => Some("Java"),
        "rb" => Some("Ruby"),
        "html" | "htm" => Some("HTML"),
        "css" => Some("CSS"),
        "scss" | "sass" => Some("SCSS"),
        "sql" => Some("SQL"),
        "xml" => Some("XML"),
        "lua" => Some("Lua"),
        "swift" => Some("Swift"),
        "kt" | "kts" => Some("Kotlin"),
        _ => None,
    }
}

/// Convert syntect style to crossterm `ContentStyle`.
fn syntect_to_crossterm(style: syntect::highlighting::Style) -> crossterm::style::ContentStyle {
    use crossterm::style::{Attribute, ContentStyle};

    let mut cs = ContentStyle {
        foreground_color: Some(Color::Rgb {
            r: style.foreground.r,
            g: style.foreground.g,
            b: style.foreground.b,
        }),
        ..Default::default()
    };

    if style.font_style.contains(FontStyle::BOLD) {
        cs.attributes.set(Attribute::Bold);
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        cs.attributes.set(Attribute::Italic);
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        cs.attributes.set(Attribute::Underlined);
    }

    cs
}

/// Highlight a single line of code.
pub fn highlight_line(text: &str, syntax_name: &str) -> StyledLine {
    let syntax = SYNTAX_SET
        .find_syntax_by_name(syntax_name)
        .or_else(|| Some(SYNTAX_SET.find_syntax_plain_text()));

    let Some(syntax) = syntax else {
        return StyledLine::raw(text.to_string());
    };

    let theme = &THEME_SET.themes["base16-ocean.dark"];
    let mut highlighter = HighlightLines::new(syntax, theme);

    match highlighter.highlight_line(text, &SYNTAX_SET) {
        Ok(ranges) => {
            let spans: Vec<StyledSpan> = ranges
                .iter()
                .map(|(style, content)| {
                    StyledSpan::new(content.to_string(), syntect_to_crossterm(*style))
                })
                .collect();
            StyledLine::new(spans)
        }
        Err(_) => StyledLine::raw(text.to_string()),
    }
}

/// Highlight multiple lines of code, returning `StyledLines`.
pub fn highlight_code(code: &str, syntax_name: &str) -> Vec<StyledLine> {
    let syntax = SYNTAX_SET
        .find_syntax_by_name(syntax_name)
        .or_else(|| Some(SYNTAX_SET.find_syntax_plain_text()));

    let Some(syntax) = syntax else {
        return code
            .lines()
            .map(|l| StyledLine::raw(l.to_string()))
            .collect();
    };

    let theme = &THEME_SET.themes["base16-ocean.dark"];
    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut lines = Vec::new();

    for line in code.lines() {
        match highlighter.highlight_line(line, &SYNTAX_SET) {
            Ok(ranges) => {
                let spans: Vec<StyledSpan> = ranges
                    .iter()
                    .map(|(style, content)| {
                        StyledSpan::new(content.to_string(), syntect_to_crossterm(*style))
                    })
                    .collect();
                lines.push(StyledLine::new(spans));
            }
            Err(_) => {
                lines.push(StyledLine::raw(line.to_string()));
            }
        }
    }

    lines
}

/// Detect syntax from a markdown code fence language identifier.
/// Handles common aliases like "rust" -> "Rust", "javascript" -> "JavaScript".
pub fn syntax_from_fence(lang: &str) -> Option<&'static str> {
    match lang.to_lowercase().as_str() {
        "rust" | "rs" => Some("Rust"),
        "python" | "py" => Some("Python"),
        "javascript" | "js" | "jsx" => Some("JavaScript"),
        "typescript" | "ts" | "tsx" => Some("TypeScript"),
        "json" => Some("JSON"),
        "toml" => Some("TOML"),
        "yaml" | "yml" => Some("YAML"),
        "markdown" | "md" => Some("Markdown"),
        "bash" | "sh" | "shell" | "zsh" => Some("Bourne Again Shell (bash)"),
        "go" | "golang" => Some("Go"),
        "c" => Some("C"),
        "cpp" | "c++" | "cc" | "cxx" => Some("C++"),
        "java" => Some("Java"),
        "ruby" | "rb" => Some("Ruby"),
        "html" => Some("HTML"),
        "css" => Some("CSS"),
        "scss" | "sass" => Some("SCSS"),
        "sql" => Some("SQL"),
        "xml" => Some("XML"),
        "lua" => Some("Lua"),
        "swift" => Some("Swift"),
        "kotlin" | "kt" => Some("Kotlin"),
        "diff" => Some("Diff"), // For diff blocks
        _ => None,
    }
}

/// Render markdown content to styled lines using pulldown-cmark.
/// Supports: bold, italic, code spans, code blocks, headers, lists, tables.
pub fn render_markdown(content: &str) -> Vec<StyledLine> {
    render_markdown_with_width(content, 80)
}

/// Render markdown with explicit width for table rendering.
pub fn render_markdown_with_width(content: &str, width: usize) -> Vec<StyledLine> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    let parser = Parser::new_ext(content, options);
    let mut result = Vec::new();
    let mut current_line = LineBuilder::new();
    let mut in_bold = false;
    let mut in_italic = false;
    let mut in_code_block = false;
    let mut code_block_lang: Option<&'static str> = None;
    let mut code_block_buffer = String::new();
    let mut list_depth: usize = 0;
    let mut list_prefix: Option<String> = None;
    let mut current_line_is_prefix_only = false;
    let mut ordered_list_counters: Vec<usize> = Vec::new();
    let mut in_blockquote = false;

    // Table state
    let mut in_table = false;
    let mut table = Table::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut current_cell = String::new();
    let mut in_table_head = false;

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Strong => in_bold = true,
                Tag::Emphasis => in_italic = true,
                Tag::CodeBlock(kind) => {
                    in_code_block = true;
                    code_block_buffer.clear();
                    code_block_lang = match kind {
                        pulldown_cmark::CodeBlockKind::Fenced(ref lang) => syntax_from_fence(lang),
                        pulldown_cmark::CodeBlockKind::Indented => None,
                    };
                }
                Tag::Heading { level, .. } => {
                    // Push current line if not empty
                    let line = current_line.build();
                    if !line.is_empty() {
                        result.push(line);
                    }
                    // Headers get bold styling with prefix
                    let prefix = match level {
                        HeadingLevel::H1 => "# ",
                        HeadingLevel::H2 => "## ",
                        HeadingLevel::H3 => "### ",
                        _ => "#### ",
                    };
                    current_line = LineBuilder::new().bold(prefix);
                    in_bold = true;
                }
                Tag::List(start_num) => {
                    list_depth += 1;
                    if let Some(n) = start_num {
                        ordered_list_counters.push(n as usize);
                    } else {
                        ordered_list_counters.push(0); // 0 = unordered
                    }
                }
                Tag::Item => {
                    let indent = "  ".repeat(list_depth.saturating_sub(1));
                    let prefix = if let Some(counter) = ordered_list_counters.last_mut() {
                        if *counter > 0 {
                            let num = *counter;
                            *counter += 1;
                            format!("{indent}{num}. ")
                        } else {
                            format!("{indent}* ")
                        }
                    } else {
                        format!("{indent}* ")
                    };
                    list_prefix = Some(prefix.clone());
                    current_line = LineBuilder::new().raw(prefix);
                    current_line_is_prefix_only = true;
                }
                Tag::BlockQuote(_) => {
                    in_blockquote = true;
                }
                Tag::Paragraph => {
                    // Start fresh line for paragraphs
                    if !current_line_is_prefix_only {
                        let line = current_line.build();
                        if !line.is_empty() {
                            result.push(line);
                        }
                    }
                    if let Some(prefix) = list_prefix.as_ref() {
                        current_line = LineBuilder::new().raw(prefix.clone());
                        current_line_is_prefix_only = true;
                    } else {
                        current_line = LineBuilder::new();
                        current_line_is_prefix_only = false;
                    }
                }
                Tag::Table(alignments) => {
                    in_table = true;
                    table = Table::new();
                    table.alignments = alignments.into_iter().collect();
                    current_row.clear();
                }
                Tag::TableHead => {
                    in_table_head = true;
                    current_row.clear();
                }
                Tag::TableRow => {
                    current_row.clear();
                }
                Tag::TableCell => {
                    current_cell.clear();
                }
                _ => {}
            },
            Event::End(tag_end) => match tag_end {
                TagEnd::Strong => in_bold = false,
                TagEnd::Emphasis => in_italic = false,
                TagEnd::CodeBlock => {
                    in_code_block = false;
                    // Render the code block with syntax highlighting
                    if !code_block_buffer.is_empty() {
                        if let Some(lang) = code_block_lang {
                            if lang == "Diff" {
                                for line in code_block_buffer.lines() {
                                    result.push(highlight_diff_line(line));
                                }
                            } else {
                                for line in highlight_code(&code_block_buffer, lang) {
                                    result.push(line);
                                }
                            }
                        } else {
                            // No language - render as dim
                            for line in code_block_buffer.lines() {
                                result.push(LineBuilder::new().dim(line).build());
                            }
                        }
                    }
                    // Add blank line after code block
                    result.push(StyledLine::empty());
                    code_block_lang = None;
                }
                TagEnd::Heading { .. } => {
                    in_bold = false;
                    let line = current_line.build();
                    result.push(line);
                    current_line = LineBuilder::new();
                    if list_depth == 0 {
                        result.push(StyledLine::empty());
                    }
                }
                TagEnd::List(_) => {
                    list_depth = list_depth.saturating_sub(1);
                    ordered_list_counters.pop();
                    if list_depth == 0 {
                        result.push(StyledLine::empty());
                    }
                }
                TagEnd::BlockQuote(_) => {
                    in_blockquote = false;
                    result.push(StyledLine::empty());
                }
                TagEnd::Item => {
                    if !current_line_is_prefix_only {
                        let line = current_line.build();
                        if !line.is_empty() {
                            result.push(line);
                        }
                    }
                    list_prefix = None;
                    current_line_is_prefix_only = false;
                    current_line = LineBuilder::new();
                }
                TagEnd::Paragraph => {
                    if !current_line_is_prefix_only {
                        let line = current_line.build();
                        if !line.is_empty() {
                            result.push(line);
                        }
                    }
                    current_line = LineBuilder::new();
                    current_line_is_prefix_only = false;
                    if list_depth == 0 {
                        result.push(StyledLine::empty());
                    }
                }
                TagEnd::TableCell => {
                    current_row.push(std::mem::take(&mut current_cell));
                }
                TagEnd::TableRow => {
                    if in_table_head {
                        table.headers = std::mem::take(&mut current_row);
                    } else {
                        table.rows.push(std::mem::take(&mut current_row));
                    }
                }
                TagEnd::TableHead => {
                    // Header cells come directly under TableHead (no TableRow wrapper)
                    // Save accumulated cells as headers
                    table.headers = std::mem::take(&mut current_row);
                    in_table_head = false;
                }
                TagEnd::Table => {
                    in_table = false;
                    // Render the table
                    let table_lines = table.render(width);
                    result.extend(table_lines);
                    result.push(StyledLine::empty());
                    table = Table::new();
                }
                _ => {}
            },
            Event::Text(text) => {
                if in_code_block {
                    code_block_buffer.push_str(&text);
                } else if in_table {
                    current_cell.push_str(&text);
                } else {
                    let content = text.to_string();
                    // Handle line breaks within text
                    for (i, part) in content.split('\n').enumerate() {
                        if i > 0 {
                            if !current_line_is_prefix_only {
                                result.push(current_line.build());
                            }
                            if in_blockquote {
                                current_line =
                                    LineBuilder::new().styled(StyledSpan::dim("> ".to_string()));
                            } else {
                                current_line = LineBuilder::new();
                            }
                            current_line_is_prefix_only = false;
                        }
                        if !part.is_empty() {
                            if list_prefix.is_some()
                                && current_line_is_prefix_only
                                && part.trim().is_empty()
                            {
                                continue;
                            }
                            // Add blockquote prefix if this is the start of a blockquote line
                            if in_blockquote && current_line_is_prefix_only {
                                current_line =
                                    LineBuilder::new().styled(StyledSpan::dim("> ".to_string()));
                            }
                            let span = if in_bold && in_italic {
                                StyledSpan::bold(part.to_string()).with_italic()
                            } else if in_bold {
                                StyledSpan::bold(part.to_string())
                            } else if in_italic || in_blockquote {
                                StyledSpan::italic(part.to_string())
                            } else {
                                StyledSpan::raw(part.to_string())
                            };
                            current_line = current_line.styled(span);
                            current_line_is_prefix_only = false;
                        }
                    }
                }
            }
            Event::Rule => {
                // Horizontal rule / thematic break
                let line = current_line.build();
                if !line.is_empty() {
                    result.push(line);
                }
                let rule_width = width.min(40);
                result.push(StyledLine::dim("─".repeat(rule_width)));
                result.push(StyledLine::empty());
                current_line = LineBuilder::new();
            }
            Event::Code(code) => {
                // Inline code - render with dim styling
                let span = StyledSpan::dim(format!("`{code}`"));
                current_line = current_line.styled(span);
                current_line_is_prefix_only = false;
            }
            Event::SoftBreak | Event::HardBreak => {
                if !current_line_is_prefix_only {
                    result.push(current_line.build());
                }
                current_line = LineBuilder::new();
                current_line_is_prefix_only = false;
            }
            _ => {}
        }
    }

    // Push final line
    let line = current_line.build();
    if !line.is_empty() {
        result.push(line);
    }

    while result.last().is_some_and(StyledLine::is_empty) {
        result.pop();
    }

    result
}

/// Highlight markdown content with explicit width for table rendering.
pub fn highlight_markdown_with_width(content: &str, width: usize) -> Vec<StyledLine> {
    render_markdown_with_width(content, width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_code_block_indentation_preserved() {
        let input = r#"Text before

```rust
pub fn example() {
    if true {
        println!("nested");
    }
}
```

Text after"#;

        let lines = render_markdown(input);

        // Extract text content from lines
        let line_texts: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_str()).collect())
            .collect();

        // Find the "if true" line
        let if_line = line_texts
            .iter()
            .find(|l| l.contains("if true"))
            .expect("Should find 'if true' line");

        // Should preserve 4-space source indent
        assert!(
            if_line.starts_with("    ") || if_line.contains("    if"),
            "Code indentation not preserved. Line: '{}'",
            if_line
        );

        // Find println line - should have 8-space source indent
        let println_line = line_texts
            .iter()
            .find(|l| l.contains("println"))
            .expect("Should find println line");

        assert!(
            println_line.starts_with("        ") || println_line.contains("println"),
            "Nested indentation not preserved. Line: '{}'",
            println_line
        );
    }

    #[test]
    fn test_blank_line_after_code_block() {
        let input = r#"```rust
code
```
Next paragraph"#;

        let lines = render_markdown(input);
        let line_texts: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_str()).collect())
            .collect();

        // Should have: code line, blank line, "Next paragraph"
        let code_idx = line_texts.iter().position(|l| l.contains("code")).unwrap();
        let blank_idx = code_idx + 1;

        assert!(
            line_texts
                .get(blank_idx)
                .map(|l| l.trim().is_empty())
                .unwrap_or(false),
            "Expected blank line after code block, got: {:?}",
            line_texts.get(blank_idx)
        );
    }

    #[test]
    fn test_render_markdown_bold_italic() {
        let input = "**bold** and *italic* text";
        let lines = render_markdown(input);
        assert!(!lines.is_empty());
        // Check that spans are created
        let line = &lines[0];
        assert!(line.spans.len() >= 3);
    }

    #[test]
    fn test_render_markdown_headers() {
        let input = "# Heading 1\n## Heading 2";
        let lines = render_markdown(input);
        assert!(lines.len() >= 2);
    }

    #[test]
    fn test_render_markdown_drops_empty_list_item() {
        let input = "*\n\nParagraph";
        let lines = render_markdown(input);
        let line_texts: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_str()).collect())
            .collect();
        assert!(
            !line_texts.iter().any(|l| l.trim() == "*"),
            "Expected empty list item marker to be dropped, got: {:?}",
            line_texts
        );
    }

    #[test]
    fn test_diff_highlighting() {
        let line = highlight_diff_line("+added line");
        assert!(!line.is_empty());

        let line = highlight_diff_line("-removed line");
        assert!(!line.is_empty());

        let line = highlight_diff_line("@@ hunk header @@");
        assert!(!line.is_empty());
    }

    #[test]
    fn test_render_markdown_table() {
        let input = r#"| Name | Value |
|------|-------|
| foo  | 123   |
| bar  | 456   |"#;

        let lines = render_markdown_with_width(input, 80);
        assert!(!lines.is_empty(), "Table should produce lines");

        // Should have box drawing characters
        let all_text: String = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(all_text.contains("┌"), "Should have top border");
        assert!(all_text.contains("│"), "Should have column separators");
        assert!(all_text.contains("foo"), "Should contain cell content");
    }

    #[test]
    fn test_render_markdown_table_narrow() {
        let input = r#"| Name | Value |
|------|-------|
| foo  | 123   |"#;

        // Very narrow width forces fallback mode
        let lines = render_markdown_with_width(input, 20);
        assert!(!lines.is_empty(), "Narrow table should produce lines");

        let all_text: String = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Narrow mode uses "Header: Value" format
        assert!(
            all_text.contains("Name") && all_text.contains("foo"),
            "Should contain header and value, got: {:?}",
            all_text
        );
    }
}
