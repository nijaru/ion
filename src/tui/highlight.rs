//! Syntax highlighting for tool output using syntect.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
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
pub fn highlight_diff_line(line: &str) -> Line<'static> {
    let style = if line.starts_with('+') && !line.starts_with("+++") {
        Style::default().fg(Color::Green)
    } else if line.starts_with('-') && !line.starts_with("---") {
        Style::default().fg(Color::Red)
    } else if line.starts_with('@') {
        Style::default().fg(Color::Cyan)
    } else if line.starts_with("+++") || line.starts_with("---") {
        Style::default().bold()
    } else {
        Style::default().dim()
    };
    Line::from(Span::styled(line.to_string(), style))
}

/// Detect syntax from file extension.
pub fn detect_syntax(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next()?;
    match ext.to_lowercase().as_str() {
        "rs" => Some("Rust"),
        "py" => Some("Python"),
        "js" | "mjs" | "cjs" => Some("JavaScript"),
        "ts" | "mts" | "cts" => Some("TypeScript"),
        "tsx" => Some("TypeScript"),
        "jsx" => Some("JavaScript"),
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

/// Convert syntect style to ratatui style.
fn syntect_to_ratatui(style: syntect::highlighting::Style) -> Style {
    let fg = Color::Rgb(style.foreground.r, style.foreground.g, style.foreground.b);

    let mut ratatui_style = Style::default().fg(fg);

    if style.font_style.contains(FontStyle::BOLD) {
        ratatui_style = ratatui_style.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        ratatui_style = ratatui_style.add_modifier(Modifier::ITALIC);
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        ratatui_style = ratatui_style.add_modifier(Modifier::UNDERLINED);
    }

    ratatui_style
}

/// Highlight a single line of code.
pub fn highlight_line<'a>(text: &'a str, syntax_name: &str) -> Line<'a> {
    let syntax = SYNTAX_SET
        .find_syntax_by_name(syntax_name)
        .or_else(|| Some(SYNTAX_SET.find_syntax_plain_text()));

    let Some(syntax) = syntax else {
        return Line::raw(text.to_string());
    };

    let theme = &THEME_SET.themes["base16-ocean.dark"];
    let mut highlighter = HighlightLines::new(syntax, theme);

    match highlighter.highlight_line(text, &SYNTAX_SET) {
        Ok(ranges) => {
            let spans: Vec<Span> = ranges
                .iter()
                .map(|(style, content)| {
                    Span::styled(content.to_string(), syntect_to_ratatui(*style))
                })
                .collect();
            Line::from(spans)
        }
        Err(_) => Line::raw(text.to_string()),
    }
}

/// Highlight multiple lines of code, returning ratatui Lines.
#[allow(dead_code)]
pub fn highlight_code(code: &str, syntax_name: &str) -> Vec<Line<'static>> {
    let syntax = SYNTAX_SET
        .find_syntax_by_name(syntax_name)
        .or_else(|| Some(SYNTAX_SET.find_syntax_plain_text()));

    let Some(syntax) = syntax else {
        return code.lines().map(|l| Line::raw(l.to_string())).collect();
    };

    let theme = &THEME_SET.themes["base16-ocean.dark"];
    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut lines = Vec::new();

    for line in code.lines() {
        match highlighter.highlight_line(line, &SYNTAX_SET) {
            Ok(ranges) => {
                let spans: Vec<Span> = ranges
                    .iter()
                    .map(|(style, content)| {
                        Span::styled(content.to_string(), syntect_to_ratatui(*style))
                    })
                    .collect();
                lines.push(Line::from(spans));
            }
            Err(_) => {
                lines.push(Line::raw(line.to_string()));
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
        "javascript" | "js" => Some("JavaScript"),
        "typescript" | "ts" => Some("TypeScript"),
        "tsx" => Some("TypeScript"),
        "jsx" => Some("JavaScript"),
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

/// Highlight markdown content with syntax highlighting for code blocks.
/// Returns a vector of Lines with code blocks highlighted.
pub fn highlight_markdown_with_code(content: &str) -> Vec<Line<'static>> {
    let mut result = Vec::new();
    let mut in_code_block = false;
    let mut code_lang: Option<&'static str> = None;
    let mut code_buffer = String::new();
    let mut markdown_buffer = String::new();

    for line in content.lines() {
        if line.starts_with("```") {
            if in_code_block {
                // Closing fence - highlight accumulated code
                if !code_buffer.is_empty() {
                    if let Some(lang) = code_lang {
                        // Use diff highlighting for diff blocks
                        if lang == "Diff" {
                            for code_line in code_buffer.lines() {
                                let mut highlighted = highlight_diff_line(code_line);
                                highlighted.spans.insert(0, Span::raw("  "));
                                result.push(highlighted);
                            }
                        } else {
                            for code_line in highlight_code(&code_buffer, lang) {
                                let mut padded = vec![Span::raw("  ")];
                                padded.extend(code_line.spans);
                                result.push(Line::from(padded));
                            }
                        }
                    } else {
                        // No language - render as dim monospace
                        for code_line in code_buffer.lines() {
                            result.push(Line::from(vec![
                                Span::raw("  "),
                                Span::styled(code_line.to_string(), Style::default().dim()),
                            ]));
                        }
                    }
                }
                code_buffer.clear();
                in_code_block = false;
                code_lang = None;
            } else {
                // Flush markdown buffer before entering code block
                if !markdown_buffer.is_empty() {
                    let md = tui_markdown::from_str(&markdown_buffer);
                    for md_line in md.lines {
                        // Clone each span to get owned data
                        let owned_spans: Vec<Span<'static>> = md_line
                            .spans
                            .iter()
                            .map(|s| Span::styled(s.content.to_string(), s.style))
                            .collect();
                        result.push(Line::from(owned_spans));
                    }
                    markdown_buffer.clear();
                }
                // Opening fence - extract language
                let lang_str = line.trim_start_matches('`').trim();
                code_lang = syntax_from_fence(lang_str);
                in_code_block = true;
            }
        } else if in_code_block {
            // Accumulate code
            if !code_buffer.is_empty() {
                code_buffer.push('\n');
            }
            code_buffer.push_str(line);
        } else {
            // Accumulate markdown
            if !markdown_buffer.is_empty() {
                markdown_buffer.push('\n');
            }
            markdown_buffer.push_str(line);
        }
    }

    // Flush remaining markdown
    if !markdown_buffer.is_empty() {
        let md = tui_markdown::from_str(&markdown_buffer);
        for md_line in md.lines {
            let owned_spans: Vec<Span<'static>> = md_line
                .spans
                .iter()
                .map(|s| Span::styled(s.content.to_string(), s.style))
                .collect();
            result.push(Line::from(owned_spans));
        }
    }

    // Handle unclosed code block
    if in_code_block && !code_buffer.is_empty() {
        if let Some(lang) = code_lang {
            if lang == "Diff" {
                for code_line in code_buffer.lines() {
                    let mut highlighted = highlight_diff_line(code_line);
                    highlighted.spans.insert(0, Span::raw("  "));
                    result.push(highlighted);
                }
            } else {
                for code_line in highlight_code(&code_buffer, lang) {
                    let mut padded = vec![Span::raw("  ")];
                    padded.extend(code_line.spans);
                    result.push(Line::from(padded));
                }
            }
        } else {
            for code_line in code_buffer.lines() {
                result.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(code_line.to_string(), Style::default().dim()),
                ]));
            }
        }
    }

    result
}
