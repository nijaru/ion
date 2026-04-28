package app

import (
	"context"
	"errors"
	"path/filepath"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/canto/llm"
	csession "github.com/nijaru/canto/session"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func TestCoreLoopSmokeSubmitStreamToolPersistReplay(t *testing.T) {
	model, sess, store, stored := newCoreLoopSmokeModel(t)

	model.Input.Composer.SetValue("run smoke")
	updated, _ := model.Update(sendKeyMsg())
	model = updated.(Model)

	if len(sess.submits) != 1 || sess.submits[0] != "run smoke" {
		t.Fatalf("submitted turns = %#v, want run smoke", sess.submits)
	}

	for _, ev := range []session.Event{
		session.TurnStarted{},
		session.TokenUsage{Input: 12, Output: 4, Cost: 0.0012},
		session.AgentDelta{Delta: "working"},
		session.ToolCallStarted{ToolUseID: "tool-1", ToolName: "bash", Args: "echo smoke"},
		session.ToolOutputDelta{ToolUseID: "tool-1", Delta: "sm"},
		session.ToolOutputDelta{ToolUseID: "tool-1", Delta: "oke\n"},
		session.ToolResult{ToolUseID: "tool-1", ToolName: "bash", Result: "smoke\n"},
		session.AgentMessage{Message: "done"},
		session.TurnFinished{},
	} {
		updated, _ = model.Update(ev)
		model = updated.(Model)
	}

	if model.Progress.Mode != stateComplete {
		t.Fatalf("progress mode = %v, want complete", model.Progress.Mode)
	}
	if model.Progress.LastTurnSummary.Input != 12 || model.Progress.LastTurnSummary.Output != 4 {
		t.Fatalf("last usage = %d/%d, want 12/4", model.Progress.LastTurnSummary.Input, model.Progress.LastTurnSummary.Output)
	}
	toolCall := llm.Call{ID: "tool-1", Type: "function"}
	toolCall.Function.Name = "bash"
	toolCall.Function.Arguments = `{"args":"echo smoke"}`
	appendCantoHistory(t, context.Background(), store, stored.ID(),
		llm.Message{Role: llm.RoleUser, Content: "run smoke"},
		llm.Message{Role: llm.RoleAssistant, Calls: []llm.Call{toolCall}},
		llm.Message{Role: llm.RoleTool, ToolID: "tool-1", Name: "bash", Content: "smoke\n"},
		llm.Message{Role: llm.RoleAssistant, Content: "done"},
	)

	resumed, err := store.ResumeSession(context.Background(), stored.ID())
	if err != nil {
		t.Fatalf("resume session: %v", err)
	}
	entries, err := resumed.Entries(context.Background())
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	requireEntry(t, entries, session.User, "run smoke")
	requireEntry(t, entries, session.Agent, "done")
	requireEntry(t, entries, session.Tool, "smoke")

	input, output, cost, err := resumed.Usage(context.Background())
	if err != nil {
		t.Fatalf("usage: %v", err)
	}
	if input != 12 || output != 4 || cost != 0.0012 {
		t.Fatalf("usage = %d/%d/%f, want 12/4/0.0012", input, output, cost)
	}
}

func TestCoreLoopSmokeApprovalAndCancel(t *testing.T) {
	model, sess, _, _ := newCoreLoopSmokeModel(t)

	updated, _ := model.Update(session.TurnStarted{})
	model = updated.(Model)
	updated, _ = model.Update(session.ApprovalRequest{
		RequestID:   "approval-1",
		ToolName:    "bash",
		Description: "Tool: bash",
	})
	model = updated.(Model)

	if model.Progress.Mode != stateApproval {
		t.Fatalf("progress mode = %v, want approval", model.Progress.Mode)
	}

	updated, _ = model.Update(tea.KeyPressMsg{Code: 'y', Text: "y"})
	model = updated.(Model)

	if model.Approval.Pending != nil {
		t.Fatal("approval should be cleared")
	}
	if len(sess.approvals) != 1 || sess.approvals[0] != (stubApproval{id: "approval-1", ok: true}) {
		t.Fatalf("approvals = %#v, want approval-1 true", sess.approvals)
	}
	if model.Progress.Mode != stateReady {
		t.Fatalf("progress mode = %v, want ready after approval", model.Progress.Mode)
	}

	updated, _ = model.Update(session.TurnStarted{})
	model = updated.(Model)
	updated, _ = model.Update(tea.KeyPressMsg{Code: tea.KeyEsc})
	model = updated.(Model)

	if sess.cancels != 1 {
		t.Fatalf("cancels = %d, want 1", sess.cancels)
	}
	if model.Progress.Mode != stateCancelled {
		t.Fatalf("progress mode = %v, want cancelled", model.Progress.Mode)
	}
}

func TestCoreLoopSmokeCancelPersistsTerminalEntry(t *testing.T) {
	model, sess, store, stored := newCoreLoopSmokeModel(t)

	updated, _ := model.Update(session.TurnStarted{})
	model = updated.(Model)
	updated, _ = model.Update(tea.KeyPressMsg{Code: tea.KeyEsc})
	model = updated.(Model)

	if sess.cancels != 1 {
		t.Fatalf("cancels = %d, want 1", sess.cancels)
	}
	if model.Progress.Mode != stateCancelled {
		t.Fatalf("progress mode = %v, want cancelled", model.Progress.Mode)
	}

	resumed, err := store.ResumeSession(context.Background(), stored.ID())
	if err != nil {
		t.Fatalf("resume session: %v", err)
	}
	entries, err := resumed.Entries(context.Background())
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	requireEntry(t, entries, session.System, "Canceled by user")
}

func TestCoreLoopSmokeProviderLimitErrorPersistsStopTrace(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	stored := &stubStorageSession{}
	model := New(
		stubBackend{sess: &stubSession{events: make(chan session.Event)}, provider: "fake", model: "model"},
		stored,
		nil,
		"/tmp/ion-smoke",
		"main",
		"dev",
		nil,
	)

	updated, _ := model.Update(session.TurnStarted{})
	model = updated.(Model)
	updated, _ = model.Update(session.Error{Err: errors.New("status 429: rate limit exceeded")})
	model = updated.(Model)

	if model.Progress.Mode != stateError {
		t.Fatalf("progress mode = %v, want error", model.Progress.Mode)
	}
	if !strings.Contains(model.Progress.LastError, "API rate limit") {
		t.Fatalf("last error = %q, want rate limit prefix", model.Progress.LastError)
	}
	var found bool
	for _, appended := range stored.appends {
		decision, ok := appended.(storage.RoutingDecision)
		if ok && decision.Decision == "stop" && decision.Reason == "rate_limit" {
			found = true
			break
		}
	}
	if !found {
		t.Fatalf("missing provider rate-limit routing stop in %#v", stored.appends)
	}
}

func TestCoreLoopSmokeProviderLimitErrorPersistsForResume(t *testing.T) {
	model, _, store, stored := newCoreLoopSmokeModel(t)

	updated, _ := model.Update(session.TurnStarted{})
	model = updated.(Model)
	updated, _ = model.Update(session.TokenUsage{Input: 20, Output: 3, Cost: 0.02})
	model = updated.(Model)
	updated, _ = model.Update(session.Error{Err: errors.New("status 429: rate limit exceeded")})
	model = updated.(Model)

	if model.Progress.Mode != stateError {
		t.Fatalf("progress mode = %v, want error", model.Progress.Mode)
	}

	resumed, err := store.ResumeSession(context.Background(), stored.ID())
	if err != nil {
		t.Fatalf("resume session: %v", err)
	}
	entries, err := resumed.Entries(context.Background())
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	requireEntry(t, entries, session.System, "Error: API rate limit")

	input, output, cost, err := resumed.Usage(context.Background())
	if err != nil {
		t.Fatalf("usage: %v", err)
	}
	if input != 20 || output != 3 || cost != 0.02 {
		t.Fatalf("usage = %d/%d/%f, want 20/3/0.02", input, output, cost)
	}
}

func TestCoreLoopSmokeRetryStatusPersists(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	stored := &stubStorageSession{}
	model := New(
		stubBackend{sess: &stubSession{events: make(chan session.Event)}, provider: "fake", model: "model"},
		stored,
		nil,
		"/tmp/ion-smoke",
		"main",
		"dev",
		nil,
	)

	status := "Network error. Retrying in 2s... Ctrl+C stops."
	updated, _ := model.Update(session.StatusChanged{Status: status})
	model = updated.(Model)

	if model.Progress.Status != status {
		t.Fatalf("status = %q, want retry status", model.Progress.Status)
	}
	var found bool
	for _, appended := range stored.appends {
		record, ok := appended.(storage.Status)
		if ok && record.Status == status {
			found = true
			break
		}
	}
	if !found {
		t.Fatalf("missing retry status persistence in %#v", stored.appends)
	}
}

func TestCoreLoopSmokeRetryStatusPersistsForResume(t *testing.T) {
	model, _, store, stored := newCoreLoopSmokeModel(t)

	status := "Network error. Retrying in 2s... Ctrl+C stops."
	updated, _ := model.Update(session.StatusChanged{Status: status})
	model = updated.(Model)

	if model.Progress.Status != status {
		t.Fatalf("status = %q, want retry status", model.Progress.Status)
	}

	resumed, err := store.ResumeSession(context.Background(), stored.ID())
	if err != nil {
		t.Fatalf("resume session: %v", err)
	}
	got, err := resumed.LastStatus(context.Background())
	if err != nil {
		t.Fatalf("last status: %v", err)
	}
	if got != status {
		t.Fatalf("last status = %q, want %q", got, status)
	}
}

func TestCoreLoopSmokeToolPreviewRedactsSensitiveArgs(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	stored := &stubStorageSession{}
	model := New(
		stubBackend{sess: &stubSession{events: make(chan session.Event)}, provider: "fake", model: "model"},
		stored,
		nil,
		"/tmp/ion-smoke",
		"main",
		"dev",
		nil,
	)

	updated, _ := model.Update(session.ToolCallStarted{
		ToolUseID: "tool-1",
		ToolName:  "bash",
		Args:      `curl -H "Authorization: Bearer abc.def-123" https://example.test`,
	})
	model = updated.(Model)

	if model.InFlight.Pending == nil {
		t.Fatal("expected pending tool preview")
	}
	if strings.Contains(model.InFlight.Pending.Title, "abc.def-123") {
		t.Fatalf("tool preview leaked token: %q", model.InFlight.Pending.Title)
	}
	if !strings.Contains(model.InFlight.Pending.Title, "[redacted-secret]") {
		t.Fatalf("tool preview missing redaction marker: %q", model.InFlight.Pending.Title)
	}
	for _, appended := range stored.appends {
		if _, ok := appended.(storage.ToolUse); ok {
			t.Fatalf("tool start should not be app-persisted: %#v", stored.appends)
		}
	}
}

func newCoreLoopSmokeModel(t *testing.T) (Model, *stubSession, storage.Store, storage.Session) {
	t.Helper()
	t.Setenv("HOME", t.TempDir())

	store, err := storage.NewCantoStore(filepath.Join(t.TempDir(), "store"))
	if err != nil {
		t.Fatalf("new store: %v", err)
	}
	stored, err := store.OpenSession(context.Background(), "/tmp/ion-smoke", "fake/model", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}
	sess := &stubSession{events: make(chan session.Event)}
	model := New(
		stubBackend{sess: sess, provider: "fake", model: "model"},
		stored,
		store,
		"/tmp/ion-smoke",
		"main",
		"dev",
		nil,
	)
	return model, sess, store, stored
}

func requireEntry(t *testing.T, entries []session.Entry, role session.Role, content string) {
	t.Helper()
	for _, entry := range entries {
		if entry.Role == role && strings.Contains(entry.Content, content) {
			return
		}
	}
	t.Fatalf("missing %s entry containing %q in %#v", role, content, entries)
}

func appendCantoHistory(
	t *testing.T,
	ctx context.Context,
	store storage.Store,
	sessionID string,
	messages ...llm.Message,
) {
	t.Helper()
	cantoStore, ok := store.(interface{ Canto() *csession.SQLiteStore })
	if !ok {
		t.Fatalf("store %T does not expose Canto history", store)
	}
	for _, msg := range messages {
		if err := cantoStore.Canto().Save(
			ctx,
			csession.NewEvent(sessionID, csession.MessageAdded, msg),
		); err != nil {
			t.Fatalf("append canto history: %v", err)
		}
	}
}
