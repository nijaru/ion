package app

import (
	"strings"
	"testing"
)

func TestFormatPrintLinesAppendsSingleTrailingBlankLine(t *testing.T) {
	got := formatPrintLines("• answer", "", "")
	if !strings.HasSuffix(got, "\n") {
		t.Fatalf("formatted print body = %q, want trailing newline", got)
	}
	if strings.HasSuffix(got, "\n\n") {
		t.Fatalf("formatted print body = %q, want only a single trailing newline", got)
	}
	if got != "• answer\n" {
		t.Fatalf("formatted print body = %q, want trailing blanks trimmed with a single trailing newline", got)
	}
}

func TestFormatPrintLinesPreservesInteriorBlankLine(t *testing.T) {
	got := formatPrintLines("• first", "", "• second")
	if !strings.Contains(got, "\x1b[0m") {
		t.Fatalf("formatted print body = %q, want reset marker for interior blank line", got)
	}
	if !strings.HasSuffix(got, "\n") {
		t.Fatalf("formatted print body = %q, want trailing newline", got)
	}
	if strings.HasSuffix(got, "\n\n") {
		t.Fatalf("formatted print body = %q, want only a single trailing newline", got)
	}
}
