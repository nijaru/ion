package app

import (
	"strings"
	"testing"

	"github.com/nijaru/ion/session"
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

func TestActionRecoverySummaryRequiresVerification(t *testing.T) {
	actions := []session.ActionRecord{
		{ID: "action-1", Tool: "bash", State: session.ActionIndeterminate, Error: "provider\nclosed before result"},
		{ID: "action-2", Tool: "mcp.fetch", State: session.ActionStarted},
	}

	got := actionRecoverySummary(actions)
	if !strings.Contains(got, "Action recovery: 2 unsettled external action(s); verify before retry") {
		t.Fatalf("summary = %q, want verification warning", got)
	}
	if !strings.Contains(got, "- action-1: bash indeterminate — provider closed before result") {
		t.Fatalf("summary = %q, want normalized action reason", got)
	}
	if !strings.Contains(got, "- action-2: mcp.fetch started") {
		t.Fatalf("summary = %q, want second action", got)
	}
	if actionRecoverySummary(nil) != "" {
		t.Fatal("empty recovery should not add a summary")
	}
}
