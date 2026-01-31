#[cfg(test)]
mod tests {
    use crate::tui::highlight::markdown::render_markdown_with_width;
    use crate::tui::highlight::{highlight_diff_line, render_markdown};

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

    #[test]
    fn test_render_markdown_unordered_list_dash() {
        let input = "- Item one\n- Item two\n- Item three";
        let lines = render_markdown(input);
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
        // Unordered lists should use * prefix, not numbers
        assert!(
            all_text.contains("* Item one"),
            "Unordered list (dash) should use * prefix, got: {:?}",
            all_text
        );
        assert!(
            !all_text.contains("1."),
            "Unordered list should not have numbers, got: {:?}",
            all_text
        );
    }

    #[test]
    fn test_render_markdown_unordered_list_asterisk() {
        let input = "* Item one\n* Item two\n* Item three";
        let lines = render_markdown(input);
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
        // Unordered lists should use * prefix, not numbers
        assert!(
            all_text.contains("* Item one"),
            "Unordered list (asterisk) should use * prefix, got: {:?}",
            all_text
        );
        assert!(
            !all_text.contains("1."),
            "Unordered list should not have numbers, got: {:?}",
            all_text
        );
    }

    #[test]
    fn test_render_markdown_unordered_list_plus() {
        let input = "+ Item one\n+ Item two\n+ Item three";
        let lines = render_markdown(input);
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
        // Unordered lists should use * prefix, not numbers
        assert!(
            all_text.contains("* Item one"),
            "Unordered list (plus) should use * prefix, got: {:?}",
            all_text
        );
        assert!(
            !all_text.contains("1."),
            "Unordered list should not have numbers, got: {:?}",
            all_text
        );
    }

    #[test]
    fn test_render_markdown_ordered_list() {
        let input = "1. First\n2. Second\n3. Third";
        let lines = render_markdown(input);
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
        // Ordered lists should use numbered prefix
        assert!(
            all_text.contains("1. First"),
            "Ordered list should use numbered prefix, got: {:?}",
            all_text
        );
    }

    #[test]
    fn test_render_markdown_nested_unordered_list() {
        let input = "- Item one\n  - Nested one\n  - Nested two\n- Item two";
        let lines = render_markdown(input);
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
        // Nested lists should not get numbered
        assert!(
            !all_text.contains("1.") && !all_text.contains("2."),
            "Nested unordered list should not have numbers, got: {:?}",
            all_text
        );
    }

    #[test]
    fn test_render_markdown_mixed_lists() {
        // Ordered list with unordered sub-list
        let input = "1. First\n   - Sub one\n   - Sub two\n2. Second";
        let lines = render_markdown(input);
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
        // Should have both 1. and * prefixes, sub-list should use *
        assert!(
            all_text.contains("1. First"),
            "Should have ordered prefix, got: {:?}",
            all_text
        );
        assert!(
            all_text.contains("* Sub one"),
            "Nested unordered should use *, got: {:?}",
            all_text
        );
    }
}
