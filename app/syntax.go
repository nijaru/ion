package app

import (
	"strings"

	"github.com/alecthomas/chroma/v2/formatters"
	"github.com/alecthomas/chroma/v2/lexers"
	chromastyles "github.com/alecthomas/chroma/v2/styles"
)

// highlightSyntax applies syntax highlighting to code using chroma.
// Returns the highlighted code as a string with ANSI escape codes.
// Falls back to plain text if the language is not recognized or highlighting fails.
func highlightSyntax(code, language string) string {
	// Get lexer for the language
	lexer := lexers.Get(language)
	if lexer == nil {
		lexer = lexers.Analyse(code)
	}
	if lexer == nil {
		lexer = lexers.Fallback
	}

	// Use terminal-friendly style
	style := chromastyles.Get("monokai")
	if style == nil {
		style = chromastyles.Fallback
	}

	// Use terminal formatter (ANSI escape codes)
	formatter := formatters.Get("terminal")
	if formatter == nil {
		formatter = formatters.Fallback
	}

	// Tokenize and format
	iterator, err := lexer.Tokenise(nil, code)
	if err != nil {
		return code
	}

	var buf strings.Builder
	err = formatter.Format(&buf, style, iterator)
	if err != nil {
		return code
	}

	return buf.String()
}

// highlightCodeBlock applies syntax highlighting to a code block,
// preserving indentation. Each line is highlighted individually to
// maintain consistent styling across the block.
func highlightCodeBlock(code, language string, indent string) []string {
	text := strings.TrimRight(code, "\n")
	if strings.TrimSpace(text) == "" {
		return nil
	}

	// Try to highlight the entire block at once for better context
	highlighted := highlightSyntax(text, language)
	lines := strings.Split(highlighted, "\n")

	out := make([]string, 0, len(lines))
	for _, line := range lines {
		out = append(out, indent+line)
	}
	return out
}
