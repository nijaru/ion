package main

import (
	"bytes"
	"strings"
	"testing"
)

func TestAuthCLICommandUsageAndStatus(t *testing.T) {
	tempDir := t.TempDir()
	t.Setenv("HOME", tempDir)

	var stdout, stderr bytes.Buffer

	// 1. Bare `ion auth` shows usage
	handled, code := runTopLevelCommand([]string{"auth"}, &stdout, &stderr)
	if !handled || code != 1 {
		t.Fatalf("runTopLevelCommand(auth) = (%v, %d), want (true, 1)", handled, code)
	}
	if !strings.Contains(stderr.String(), "Usage: ion auth") {
		t.Fatalf("expected usage message, got %q", stderr.String())
	}

	stdout.Reset()
	stderr.Reset()

	// 2. `ion auth status` with empty credentials
	handled, code = runTopLevelCommand([]string{"auth", "status"}, &stdout, &stderr)
	if !handled || code != 0 {
		t.Fatalf("runTopLevelCommand(auth status) = (%v, %d), want (true, 0)", handled, code)
	}
	if !strings.Contains(stdout.String(), "No credentials saved") {
		t.Fatalf("expected 'No credentials saved', got %q", stdout.String())
	}

	stdout.Reset()
	stderr.Reset()

	// 3. `ion auth logout` without provider shows usage error
	handled, code = runTopLevelCommand([]string{"auth", "logout"}, &stdout, &stderr)
	if !handled || code != 1 {
		t.Fatalf("runTopLevelCommand(auth logout) = (%v, %d), want (true, 1)", handled, code)
	}
	if !strings.Contains(stderr.String(), "usage: ion auth logout <provider>") {
		t.Fatalf("expected logout usage, got %q", stderr.String())
	}

	stdout.Reset()
	stderr.Reset()

	// 4. `ion auth logout openai` clears credentials
	handled, code = runTopLevelCommand([]string{"auth", "logout", "openai"}, &stdout, &stderr)
	if !handled || code != 0 {
		t.Fatalf("runTopLevelCommand(auth logout openai) = (%v, %d), want (true, 0)", handled, code)
	}
	if !strings.Contains(stdout.String(), "Cleared credentials for OPENAI") {
		t.Fatalf("expected cleared message, got %q", stdout.String())
	}
}
