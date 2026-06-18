package app

import (
	"strings"
	"testing"

	"github.com/charmbracelet/x/ansi"
)

func TestMarkdownRendering(t *testing.T) {
	tests := []struct {
		name     string
		input    string
		contains []string
	}{
		{
			name:  "heading",
			input: "# Hello World",
			contains: []string{
				"Hello World",
			},
		},
		{
			name:  "paragraph",
			input: "This is a paragraph.",
			contains: []string{
				"This is a paragraph.",
			},
		},
		{
			name:  "bold text",
			input: "This is **bold** text.",
			contains: []string{
				"bold",
			},
		},
		{
			name:  "italic text",
			input: "This is *italic* text.",
			contains: []string{
				"italic",
			},
		},
		{
			name:  "inline code",
			input: "Use `fmt.Println` to print.",
			contains: []string{
				"fmt.Println",
			},
		},
		{
			name: "code block",
			input: "```go\nfunc main() {\n\tfmt.Println(\"hello\")\n}\n```",
			contains: []string{
				"func main()",
				"fmt.Println",
			},
		},
		{
			name: "code block with language",
			input: "```python\ndef hello():\n    print(\"hello\")\n```",
			contains: []string{
				"def hello():",
				"print",
			},
		},
		{
			name:  "unordered list",
			input: "- Item 1\n- Item 2\n- Item 3",
			contains: []string{
				"Item 1",
				"Item 2",
				"Item 3",
			},
		},
		{
			name:  "ordered list",
			input: "1. First\n2. Second\n3. Third",
			contains: []string{
				"First",
				"Second",
				"Third",
			},
		},
		{
			name: "table",
			input: "| Name | Value |\n|------|-------|\n| foo  | bar   |",
			contains: []string{
				"Name",
				"Value",
				"foo",
				"bar",
			},
		},
		{
			name:  "blockquote",
			input: "> This is a quote.",
			contains: []string{
				"This is a quote.",
			},
		},
		{
			name:  "link",
			input: "[Example](https://example.com)",
			contains: []string{
				"Example",
			},
		},
		{
			name:  "horizontal rule",
			input: "---",
			contains: []string{
				"─",
			},
		},
		{
			name:  "task list",
			input: "- [x] Done\n- [ ] Todo",
			contains: []string{
				"[x]",
				"[ ]",
			},
		},
		{
			name:  "strikethrough",
			input: "~~deleted~~",
			contains: []string{
				"deleted",
			},
		},
		{
			name:  "empty input",
			input: "",
		},
		{
			name:  "whitespace only",
			input: "   \n   ",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			model := readyModel(t)
			result := model.renderMarkdownContent(tt.input)

			if tt.input == "" || strings.TrimSpace(tt.input) == "" {
				if strings.TrimSpace(result) != "" {
					t.Fatalf("expected empty result for empty input, got: %q", result)
				}
				return
			}

			stripped := ansi.Strip(result)
			for _, want := range tt.contains {
				if !strings.Contains(stripped, want) {
					t.Errorf("markdown rendering missing %q\ninput: %s\noutput: %s", want, tt.input, stripped)
				}
			}
		})
	}
}

func TestMarkdownCodeBlockSyntaxHighlighting(t *testing.T) {
	tests := []struct {
		name     string
		language string
		code     string
		// We can't test exact ANSI output, but we can verify the code is present
		contains []string
	}{
		{
			name:     "go code",
			language: "go",
			code:     "func main() {\n\tfmt.Println(\"hello\")\n}",
			contains: []string{
				"func",
				"main",
				"fmt.Println",
			},
		},
		{
			name:     "python code",
			language: "python",
			code:     "def hello():\n    print(\"hello\")",
			contains: []string{
				"def",
				"hello",
				"print",
			},
		},
		{
			name:     "javascript code",
			language: "javascript",
			code:     "function hello() {\n  console.log(\"hello\");\n}",
			contains: []string{
				"function",
				"hello",
				"console.log",
			},
		},
		{
			name:     "rust code",
			language: "rust",
			code:     "fn main() {\n    println!(\"hello\");\n}",
			contains: []string{
				"fn",
				"main",
				"println!",
			},
		},
		{
			name:     "unknown language falls back",
			language: "unknownlang",
			code:     "some code here",
			contains: []string{
				"some code here",
			},
		},
		{
			name:     "empty code",
			language: "go",
			code:     "",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := highlightSyntax(tt.code, tt.language)

			if tt.code == "" {
				if result != "" {
					t.Fatalf("expected empty result for empty code, got: %q", result)
				}
				return
			}

			stripped := ansi.Strip(result)
			for _, want := range tt.contains {
				if !strings.Contains(stripped, want) {
					t.Errorf("syntax highlighting missing %q\nlanguage: %s\ncode: %s\noutput: %s", want, tt.language, tt.code, stripped)
				}
			}
		})
	}
}

func TestMarkdownRenderingPreservesStructure(t *testing.T) {
	input := `# Title

First paragraph.

## Subtitle

- Item 1
- Item 2

` + "```go" + `
func main() {}
` + "```" + `

> Quote

---

Final paragraph.`

	model := readyModel(t)
	result := model.renderMarkdownContent(input)
	stripped := ansi.Strip(result)

	// Verify structure is preserved
	structuralChecks := []struct {
		name  string
		check func(string) bool
	}{
		{"has title", func(s string) bool { return strings.Contains(s, "Title") }},
		{"has subtitle", func(s string) bool { return strings.Contains(s, "Subtitle") }},
		{"has first paragraph", func(s string) bool { return strings.Contains(s, "First paragraph.") }},
		{"has items", func(s string) bool { return strings.Contains(s, "Item 1") && strings.Contains(s, "Item 2") }},
		{"has code", func(s string) bool { return strings.Contains(s, "func main()") }},
		{"has quote", func(s string) bool { return strings.Contains(s, "Quote") }},
		{"has horizontal rule", func(s string) bool { return strings.Contains(s, "─") }},
		{"has final paragraph", func(s string) bool { return strings.Contains(s, "Final paragraph.") }},
	}

	for _, check := range structuralChecks {
		if !check.check(stripped) {
			t.Errorf("markdown structure check failed: %s\noutput:\n%s", check.name, stripped)
		}
	}
}

func TestMarkdownCodeBlockWithLanguage(t *testing.T) {
	input := "```python\ndef hello():\n    print(\"hello\")\n```"

	model := readyModel(t)
	result := model.renderMarkdownContent(input)
	stripped := ansi.Strip(result)

	// Should contain the code
	if !strings.Contains(stripped, "def hello():") {
		t.Errorf("code block missing function definition:\n%s", stripped)
	}
	if !strings.Contains(stripped, "print") {
		t.Errorf("code block missing print statement:\n%s", stripped)
	}
}

func TestMarkdownInlineCode(t *testing.T) {
	input := "Use `fmt.Println` to print."

	model := readyModel(t)
	result := model.renderMarkdownContent(input)
	stripped := ansi.Strip(result)

	if !strings.Contains(stripped, "fmt.Println") {
		t.Errorf("inline code missing:\n%s", stripped)
	}
}

func TestMarkdownTableRendering(t *testing.T) {
	input := "| Name | Value |\n|------|-------|\n| foo  | bar   |\n| baz  | qux   |"

	model := readyModel(t)
	result := model.renderMarkdownContent(input)
	stripped := ansi.Strip(result)

	for _, want := range []string{"Name", "Value", "foo", "bar", "baz", "qux"} {
		if !strings.Contains(stripped, want) {
			t.Errorf("table missing %q:\n%s", want, stripped)
		}
	}
}

func TestMarkdownNestedLists(t *testing.T) {
	input := "- Item 1\n  - Nested 1\n  - Nested 2\n- Item 2"

	model := readyModel(t)
	result := model.renderMarkdownContent(input)
	stripped := ansi.Strip(result)

	for _, want := range []string{"Item 1", "Nested 1", "Nested 2", "Item 2"} {
		if !strings.Contains(stripped, want) {
			t.Errorf("nested list missing %q:\n%s", want, stripped)
		}
	}
}

func TestMarkdownBlockquote(t *testing.T) {
	input := "> This is a\n> multi-line\n> quote."

	model := readyModel(t)
	result := model.renderMarkdownContent(input)
	stripped := ansi.Strip(result)

	if !strings.Contains(stripped, "This is a") {
		t.Errorf("blockquote missing first line:\n%s", stripped)
	}
	if !strings.Contains(stripped, "multi-line") {
		t.Errorf("blockquote missing second line:\n%s", stripped)
	}
	if !strings.Contains(stripped, "quote.") {
		t.Errorf("blockquote missing third line:\n%s", stripped)
	}
}
