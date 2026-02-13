//! Syntax highlighting using syntect.

use crate::tui::terminal::{Color, StyledLine, StyledSpan, TextStyle};
use std::sync::LazyLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, ThemeSet};
use syntect::parsing::SyntaxSet;

/// Lazily loaded syntax and theme sets.
pub static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
pub static THEME_SET: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);

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

/// Convert syntect style to TUI TextStyle.
fn syntect_to_style(style: syntect::highlighting::Style) -> TextStyle {
    TextStyle {
        foreground_color: Some(Color::Rgb {
            r: style.foreground.r,
            g: style.foreground.g,
            b: style.foreground.b,
        }),
        bold: style.font_style.contains(FontStyle::BOLD),
        italic: style.font_style.contains(FontStyle::ITALIC),
        underlined: style.font_style.contains(FontStyle::UNDERLINE),
        ..TextStyle::default()
    }
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
                    StyledSpan::new(content.to_string(), syntect_to_style(*style))
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
                        StyledSpan::new(content.to_string(), syntect_to_style(*style))
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
