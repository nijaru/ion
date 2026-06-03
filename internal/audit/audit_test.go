package audit

import (
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"github.com/go-json-experiment/json"
)

func TestJSONLLogger_AppendsEvents(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "security.jsonl")

	logger, err := NewJSONLLogger(path)
	if err != nil {
		t.Fatalf("NewJSONLLogger: %v", err)
	}
	if err := logger.Log(t.Context(), Event{
		Kind:      KindToolDenied,
		SessionID: "sess-1",
		Tool:      "bash",
		Resource:  "rm -rf /",
		Decision:  "deny",
		Reason:    "unsafe command",
	}); err != nil {
		t.Fatalf("Log #1: %v", err)
	}
	if err := logger.Log(t.Context(), Event{
		Kind:      KindSandboxEscapeAttempt,
		SessionID: "sess-1",
		Tool:      "bash",
		Decision:  "deny",
		Reason:    "sandbox wrap failed",
	}); err != nil {
		t.Fatalf("Log #2: %v", err)
	}
	if err := logger.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}

	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("ReadFile: %v", err)
	}
	lines := strings.Split(strings.TrimSpace(string(data)), "\n")
	if len(lines) != 2 {
		t.Fatalf("expected 2 JSONL lines, got %d", len(lines))
	}

	var first Event
	if err := json.Unmarshal([]byte(lines[0]), &first); err != nil {
		t.Fatalf("decode first event: %v", err)
	}
	if first.Kind != KindToolDenied {
		t.Fatalf("first event kind = %q, want %q", first.Kind, KindToolDenied)
	}
	if first.Time.IsZero() {
		t.Fatal("expected event time to be populated")
	}
}

func TestJSONLLogger_CloseIsIdempotentAndStopsLogging(t *testing.T) {
	path := filepath.Join(t.TempDir(), "security.jsonl")

	logger, err := NewJSONLLogger(path)
	if err != nil {
		t.Fatalf("NewJSONLLogger: %v", err)
	}
	if err := logger.Close(); err != nil {
		t.Fatalf("Close #1: %v", err)
	}
	if err := logger.Close(); err != nil {
		t.Fatalf("Close #2: %v", err)
	}

	if err := logger.Log(t.Context(), Event{Kind: KindToolAllowed}); err == nil {
		t.Fatal("Log after Close succeeded")
	}
}

func TestJSONLLogger_ConcurrentLogAndClose(t *testing.T) {
	path := filepath.Join(t.TempDir(), "security.jsonl")

	logger, err := NewJSONLLogger(path)
	if err != nil {
		t.Fatalf("NewJSONLLogger: %v", err)
	}

	var wg sync.WaitGroup
	for range 100 {
		wg.Go(func() {
			_ = logger.Log(t.Context(), Event{Kind: KindToolAllowed})
		})
	}
	wg.Go(func() {
		_ = logger.Close()
	})
	wg.Wait()
}
