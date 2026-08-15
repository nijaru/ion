package diff

import (
	"testing"
)

func TestComputeIntraLineDiff(t *testing.T) {
	oldLine := "func foo(bar string) error"
	newLine := "func foo(bar, baz string) error"

	oldParts, newParts := ComputeIntraLineDiff(oldLine, newLine)

	if len(oldParts) != 2 {
		t.Fatalf("expected 2 old parts, got %d: %+v", len(oldParts), oldParts)
	}
	if oldParts[0].Text != "func foo(bar" || oldParts[0].Removed {
		t.Fatalf("unexpected oldPart 0: %+v", oldParts[0])
	}
	if oldParts[1].Text != " string) error" || oldParts[1].Removed {
		t.Fatalf("unexpected oldPart 1: %+v", oldParts[1])
	}

	if len(newParts) != 3 {
		t.Fatalf("expected 3 new parts, got %d: %+v", len(newParts), newParts)
	}
	if newParts[0].Text != "func foo(bar" {
		t.Fatalf("unexpected newPart 0: %+v", newParts[0])
	}
	if newParts[1].Text != ", baz" || !newParts[1].Added {
		t.Fatalf("unexpected newPart 1: %+v", newParts[1])
	}
	if newParts[2].Text != " string) error" {
		t.Fatalf("unexpected newPart 2: %+v", newParts[2])
	}
}

func TestIsSingleLineReplacement(t *testing.T) {
	lines := []string{
		"@@ -10,3 +10,3 @@",
		" context line",
		"-func foo(bar string)",
		"+func foo(bar, baz string)",
		" another context",
	}

	if !IsSingleLineReplacement(lines, 2) {
		t.Fatal("expected line 2 to be single line replacement")
	}
	if IsSingleLineReplacement(lines, 1) {
		t.Fatal("context line should not be single line replacement")
	}

	multiLines := []string{
		"-first line",
		"-second line",
		"+replacement line",
	}
	if IsSingleLineReplacement(multiLines, 0) {
		t.Fatal("multi-line removal should not be single line replacement")
	}
}
