package main

import (
	"bytes"
	"strings"
	"testing"
)

func TestRunCLIUsesIsolatedFlagSets(t *testing.T) {
	var firstOut, firstErr bytes.Buffer
	if code := runCLI([]string{"--version"}, &firstOut, &firstErr); code != 0 {
		t.Fatalf("first runCLI code = %d, want 0", code)
	}
	if got := firstOut.String(); got != "ion v0.0.0\n" {
		t.Fatalf("first stdout = %q, want version", got)
	}
	if got := firstErr.String(); got != "" {
		t.Fatalf("first stderr = %q, want empty", got)
	}

	var secondOut, secondErr bytes.Buffer
	if code := runCLI([]string{"--version"}, &secondOut, &secondErr); code != 0 {
		t.Fatalf("second runCLI code = %d, want 0", code)
	}
	if got := secondOut.String(); got != "ion v0.0.0\n" {
		t.Fatalf("second stdout = %q, want version", got)
	}
	if got := secondErr.String(); got != "" {
		t.Fatalf("second stderr = %q, want empty", got)
	}
}

func TestRunCLIHelpReturnsSuccessWithoutProcessExit(t *testing.T) {
	var stdout, stderr bytes.Buffer
	if code := runCLI([]string{"--help"}, &stdout, &stderr); code != 0 {
		t.Fatalf("runCLI help code = %d, want 0", code)
	}
	if stdout.Len() != 0 {
		t.Fatalf("help stdout = %q, want empty", stdout.String())
	}
	if !strings.Contains(stderr.String(), "Usage of ion:") {
		t.Fatalf("help stderr = %q, want usage", stderr.String())
	}
	if !strings.Contains(stderr.String(), "-version") {
		t.Fatalf("help stderr = %q, want version flag", stderr.String())
	}
}

func TestRunCLIParserErrorReturnsArgumentExitCode(t *testing.T) {
	var stdout, stderr bytes.Buffer
	if code := runCLI([]string{"--version", "--unknown"}, &stdout, &stderr); code != 2 {
		t.Fatalf("runCLI parser error code = %d, want 2", code)
	}
	if stdout.Len() != 0 {
		t.Fatalf("parser-error stdout = %q, want empty", stdout.String())
	}
	if !strings.Contains(stderr.String(), "flag provided but not defined: -unknown") {
		t.Fatalf("parser-error stderr = %q, want unknown-flag diagnostic", stderr.String())
	}
}

func TestRunCLIPrintWithoutProviderKeepsStdoutClean(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	t.Setenv("ION_PROVIDER", "")
	t.Setenv("ION_MODEL", "")
	t.Setenv("ION_REASONING_EFFORT", "")

	var stdout, stderr bytes.Buffer
	if code := runCLI(
		[]string{"--no-session", "--print", "--prompt", "hello"},
		&stdout,
		&stderr,
	); code != 1 {
		t.Fatalf("print without provider code = %d, want 1; stderr=%q", code, stderr.String())
	}
	if stdout.Len() != 0 {
		t.Fatalf("print without provider stdout = %q, want empty", stdout.String())
	}
	if !strings.Contains(stderr.String(), "print mode error: no provider configured") {
		t.Fatalf("print without provider stderr = %q, want actionable diagnostic", stderr.String())
	}
}
