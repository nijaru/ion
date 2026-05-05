package main

import (
	"bytes"
	"strings"
	"testing"
)

func TestPrintResumeHint(t *testing.T) {
	var out bytes.Buffer
	printResumeHint(&out, " session-1 ")

	got := out.String()
	if !strings.Contains(got, "Resume this session with:\nion --resume session-1\n") {
		t.Fatalf("resume hint = %q", got)
	}
}

func TestPrintResumeHintSkipsEmptySession(t *testing.T) {
	var out bytes.Buffer
	printResumeHint(&out, " ")

	if got := out.String(); got != "" {
		t.Fatalf("resume hint = %q, want empty", got)
	}
}
