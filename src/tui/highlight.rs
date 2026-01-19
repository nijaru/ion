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
