package main

import (
	"bytes"
	"context"
	"encoding/json"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/session"
)

func TestInspectTrajectoryCommand(t *testing.T) {
	tempDir := t.TempDir()
	store, err := session.NewSQLiteStore(tempDir, "test-session")
	if err != nil {
		t.Fatalf("failed to create sqlite store: %v", err)
	}
	defer store.Close()

	ctx := context.Background()
	now := time.Now()
	userMsg := session.NewUserText("Inspect this prompt", now)
	entry := &session.MessageEntry{
		EntryBase: session.EntryBase{
			ID:        "msg-1",
			ParentID:  "",
			Timestamp: now,
		},
		Message: userMsg,
	}
	if _, err := store.AppendLeafEntry(ctx, entry); err != nil {
		t.Fatalf("failed to append entry: %v", err)
	}
	if err := store.Close(); err != nil {
		t.Fatalf("failed to close store: %v", err)
	}

	var stdout, stderr bytes.Buffer
	handled, code := runTopLevelCommand([]string{"inspect", "trajectory", "--session-dir", tempDir}, &stdout, &stderr)
	if !handled || code != 0 {
		t.Fatalf("inspect trajectory failed: handled=%v, code=%d, stderr=%s", handled, code, stderr.String())
	}
	out := stdout.String()
	if !strings.Contains(out, "Ion Trajectory & Provenance Inspection") {
		t.Fatalf("expected header in output, got %q", out)
	}

	stdout.Reset()
	stderr.Reset()
	handled, code = runTopLevelCommand(
		[]string{"inspect", "trajectory", "--session-dir", tempDir, "--json"},
		&stdout,
		&stderr,
	)
	if !handled || code != 0 {
		t.Fatalf("inspect trajectory --json failed: handled=%v, code=%d, stderr=%s", handled, code, stderr.String())
	}
	var report TrajectoryReport
	if err := json.Unmarshal(stdout.Bytes(), &report); err != nil {
		t.Fatalf("failed to decode JSON trajectory: %v", err)
	}
	if len(report.Turns) == 0 {
		t.Fatal("expected at least 1 turn in trajectory report")
	}
	if len(report.Turns[0].Messages) == 0 {
		t.Fatal("expected at least 1 message on branch")
	}
}
