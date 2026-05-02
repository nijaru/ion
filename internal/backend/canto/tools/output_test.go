package tools

import (
	"strings"
	"testing"
	"unicode/utf8"
)

func TestLimitToolOutputBytesAddsVisibleMarker(t *testing.T) {
	got := limitToolOutputBytes("alpha\nbravo\ncharlie", 10)
	if !strings.HasPrefix(got, "alpha\nbrav") {
		t.Fatalf("truncated output = %q", got)
	}
	if !strings.Contains(got, "[tool output truncated after 10 bytes; 9 bytes omitted.") {
		t.Fatalf("truncated output missing marker: %q", got)
	}
}

func TestLimitToolOutputBytesPreservesShortOutput(t *testing.T) {
	const output = "short output"
	if got := limitToolOutputBytes(output, 1024); got != output {
		t.Fatalf("output = %q, want unchanged", got)
	}
}

func TestLimitToolOutputBytesDoesNotSplitFirstRune(t *testing.T) {
	got := limitToolOutputBytes("世hello", 1)
	if !utf8.ValidString(got) {
		t.Fatalf("truncated output is invalid UTF-8: %q", got)
	}
	if strings.Contains(got, "\ufffd") ||
		!strings.HasPrefix(got, "[tool output truncated after 0 bytes;") {
		t.Fatalf("truncated output = %q, want marker without replacement rune", got)
	}
}
