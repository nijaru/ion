package app

import (
	"strings"
	"testing"
)

func TestFormatPrintLinesDoesNotAppendTrailingBlankLine(t *testing.T) {
	got := formatPrintLines("• answer", "", "")
	if strings.HasSuffix(got, "\n") {
		t.Fatalf("formatted print body = %q, want no trailing newline", got)
	}
	if got != "• answer" {
		t.Fatalf("formatted print body = %q, want trailing blanks trimmed", got)
	}
}

func TestFormatPrintLinesPreservesInteriorBlankLine(t *testing.T) {
	got := formatPrintLines("• first", "", "• second")
	if !strings.Contains(got, "\x1b[0m") {
		t.Fatalf("formatted print body = %q, want reset marker for interior blank line", got)
	}
	if strings.HasSuffix(got, "\n") {
		t.Fatalf("formatted print body = %q, want no trailing newline", got)
	}
}
