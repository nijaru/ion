//! Markdown rendering using pulldown-cmark.

use super::diff::highlight_diff_line;
use super::syntax::{highlight_code, syntax_from_fence};
use crate::tui::table::Table;
use crate::tui::terminal::{LineBuilder, StyledLine, StyledSpan};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// Render markdown content to styled lines using pulldown-cmark.
/// Supports: bold, italic, code spans, code blocks, headers, lists, tables.
pub fn render_markdown(content: &str) -> Vec<StyledLine> {
    render_markdown_with_width(content, 80)
}

/// Render markdown with explicit width for table rendering.
#[allow(clippy::too_many_lines)]
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
                        #[allow(clippy::cast_possible_truncation)]
                        ordered_list_counters.push(n as usize);
                    } else {
                        ordered_list_counters.push(0); // 0 = unordered
                    }
                }
                Tag::Item => {
                    // Push any existing content before starting new item
                    // (handles nested lists inside items)
                    if !current_line_is_prefix_only {
                        let line = current_line.build();
                        if !line.is_empty() {
                            result.push(line);
                        }
                    }
                    let indent = "  ".repeat(list_depth.saturating_sub(1));
                    let prefix = if let Some(counter) = ordered_list_counters.last_mut() {
                        if *counter > 0 {
                            let num = *counter;
                            *counter += 1;
                            format!("{indent}{num}. ")
                        } else {
                            format!("{indent}- ")
                        }
                    } else {
                        format!("{indent}- ")
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
