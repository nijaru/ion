package safety_test

import (
	"bytes"
	"slices"
	"testing"

	"github.com/go-json-experiment/json"

	"github.com/nijaru/ion/internal/audit"
	"github.com/nijaru/ion/internal/safety"
)

func TestEnvSanitizerSanitizeScrubsSecretsByDefault(t *testing.T) {
	sanitizer := safety.NewEnvSanitizer()

	got := sanitizer.Sanitize([]string{
		"PATH=/usr/bin",
		"HOME=/tmp/home",
		"OPENAI_API_KEY=secret",
		"GITHUB_TOKEN=secret",
		"SAFE_VAR=ok",
	})

	if !slices.Contains(got, "PATH=/usr/bin") || !slices.Contains(got, "HOME=/tmp/home") {
		t.Fatalf("expected safe defaults to survive, got %#v", got)
	}
	if !slices.Contains(got, "SAFE_VAR=ok") {
		t.Fatalf("expected non-secret var to survive, got %#v", got)
	}
	if slices.Contains(got, "OPENAI_API_KEY=secret") ||
		slices.Contains(got, "GITHUB_TOKEN=secret") {
		t.Fatalf("expected secret vars to be scrubbed, got %#v", got)
	}
}

func TestEnvSanitizerSanitizeRespectsAllowlist(t *testing.T) {
	sanitizer := &safety.EnvSanitizer{
		Allow: []string{"PATH", "SSH_AUTH_SOCK"},
		Deny:  []string{"AUTH", "TOKEN"},
	}

	got := sanitizer.Sanitize([]string{
		"PATH=/usr/bin",
		"SSH_AUTH_SOCK=/tmp/socket",
		"SESSION_TOKEN=secret",
	})

	if !slices.Contains(got, "SSH_AUTH_SOCK=/tmp/socket") {
		t.Fatalf("expected allowlisted auth sock to survive, got %#v", got)
	}
	if slices.Contains(got, "SESSION_TOKEN=secret") {
		t.Fatalf("expected denied token to be scrubbed, got %#v", got)
	}
}

func TestEnvSanitizerLogsAuditEvents(t *testing.T) {
	var buf bytes.Buffer
	sanitizer := &safety.EnvSanitizer{
		Allow:       []string{"PATH", "SAFE_VAR"},
		Deny:        []string{"TOKEN"},
		AuditLogger: audit.NewStreamLogger(&buf),
	}

	got := sanitizer.Sanitize([]string{
		"PATH=/usr/bin",
		"SAFE_VAR=ok",
		"SESSION_TOKEN=secret",
	})
	if len(got) != 2 {
		t.Fatalf("expected 2 env vars to survive, got %#v", got)
	}

	lines := bytes.Split(bytes.TrimSpace(buf.Bytes()), []byte("\n"))
	if len(lines) != 1 {
		t.Fatalf("expected 1 audit line, got %d", len(lines))
	}

	var event audit.Event
	if err := json.Unmarshal(lines[0], &event); err != nil {
		t.Fatalf("decode audit event: %v", err)
	}
	if event.Kind != audit.KindEnvSanitized {
		t.Fatalf("event kind = %q, want %q", event.Kind, audit.KindEnvSanitized)
	}
	if event.Metadata["removed_count"] != float64(1) {
		t.Fatalf("removed_count = %#v, want 1", event.Metadata["removed_count"])
	}
}
