package app

import (
	"github.com/nijaru/ion/config"
	"context"
	"errors"
	"path/filepath"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
	csession "github.com/nijaru/ion/session"
)

func TestCoreLoopSmokeSubmitStreamToolPersistReplay(t *testing.T) {
	model, sess, store, stored := newCoreLoopSmokeModel(t)

	model.Input.Composer.SetValue("run smoke")
	updated, cmd := model.Update(sendKeyMsg())
	model = testModel(t, updated)
	model, _ = applySubmitResult(t, model, cmd)

	if len(sess.submits) != 1 || sess.submits[0] != "run smoke" {
		t.Fatalf("submitted turns = %#v, want run smoke", sess.submits)
	}

	for _, ev := range []session.AgentEvent{
		session.TurnStart{},
		session.TokenUsage{Input: 12, Output: 4, Cost: 0.0012},
		session.AgentDelta{Delta: "working"},
		session.ToolCallStart{ToolUseID: "tool-1", ToolName: "bash", Args: "echo smoke"},
		session.ToolOutputDelta{ToolUseID: "tool-1", Delta: "sm"},
		session.ToolOutputDelta{ToolUseID: "tool-1", Delta: "oke\n"},
		session.ToolCallEnd{ToolUseID: "tool-1", ToolName: "bash", Result: "smoke\n"},
		session.AgentMessage{Message: "done"},
		session.TurnEnd{},
	} {
		var cmd tea.Cmd
		updated, cmd = model.Update(ev)
		model = testModel(t, updated)
		if _, ok := ev.(session.TokenUsage); ok {
			runSequencePrefix(t, cmd, 1)
		}
	}

	if model.Progress.Mode != stateComplete {
		t.Fatalf("progress mode = %v, want complete", model.Progress.Mode)
	}
	if model.Progress.LastTurnSummary.Input != 12 || model.Progress.LastTurnSummary.Output != 4 {
		t.Fatalf(
			"last usage = %d/%d, want 12/4",
			model.Progress.LastTurnSummary.Input,
			model.Progress.LastTurnSummary.Output,
		)
	}
	toolCall := llm.Call{ID: "tool-1", Type: "function"}
	toolCall.Function.Name = "bash"
	toolCall.Function.Arguments = `{"args":"echo smoke"}`
	appendCantoHistory(
		t, context.Background(), store, stored.ID(),
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
	requireEntry(t, entries, session.RoleUser, "run smoke")
	requireEntry(t, entries, session.RoleAgent, "done")
	requireEntry(t, entries, session.RoleTool, "smoke")

	input, output, cost, err := resumed.Usage(context.Background())
	if err != nil {
		t.Fatalf("usage: %v", err)
	}
	if input != 12 || output != 4 || cost != 0.0012 {
		t.Fatalf("usage = %d/%d/%f, want 12/4/0.0012", input, output, cost)
	}
}

func TestMinimalHarnessAcceptanceFinalStateAndReplay(t *testing.T) {
	model, sess, store, stored := newCoreLoopSmokeModel(t)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 96, Height: 30})
	model = testModel(t, updated)

	model.Input.Composer.SetValue("inspect workspace")
	updated, cmd := model.Update(sendKeyMsg())
	model = testModel(t, updated)
	model, _ = applySubmitResult(t, model, cmd)

	if len(sess.submits) != 1 || sess.submits[0] != "inspect workspace" {
		t.Fatalf("submitted turns = %#v, want inspect workspace", sess.submits)
	}

	for _, ev := range []session.AgentEvent{
		session.TurnStart{},
		session.TokenUsage{Input: 20, Output: 8, Cost: 0.003},
		session.AgentDelta{Delta: "\n\nReading before tool"},
		session.ToolCallStart{ToolUseID: "tool-1", ToolName: "read", Args: "README.md"},
		session.ToolOutputDelta{ToolUseID: "tool-1", Delta: "# ion\n"},
		session.ToolCallEnd{ToolUseID: "tool-1", ToolName: "read", Result: "# ion\n"},
		session.AgentDelta{Delta: " composing final"},
		session.AgentMessage{Message: "Done with `README.md`."},
		session.TurnEnd{},
	} {
		var cmd tea.Cmd
		updated, cmd = model.Update(ev)
		model = testModel(t, updated)
		if _, ok := ev.(session.TokenUsage); ok {
			runSequencePrefix(t, cmd, 1)
		}
	}

	if model.Progress.Mode != stateComplete {
		t.Fatalf("progress mode = %v, want complete", model.Progress.Mode)
	}
	if model.InFlight.Pending != nil ||
		len(model.InFlight.PendingTools) != 0 ||
		model.InFlight.StreamBuf != "" ||
		model.InFlight.ReasonBuf != "" ||
		len(model.InFlight.QueuedTurns) != 0 {
		t.Fatalf("in-flight state not cleared: %#v", model.InFlight)
	}
	if !model.App.PrintedTranscript {
		t.Fatal("final assistant message did not mark transcript printed")
	}
	view := ansi.Strip(model.View().Content)
	if !strings.Contains(view, "Complete") {
		t.Fatalf("view = %q, want complete progress", view)
	}
	if strings.Contains(view, "Reading before tool") ||
		strings.Contains(view, "composing final") {
		t.Fatalf("view = %q, want no stale streaming text after final commit", view)
	}

	toolCall := llm.Call{ID: "tool-1", Type: "function"}
	toolCall.Function.Name = "read"
	toolCall.Function.Arguments = `{"path":"README.md"}`
	appendCantoHistory(
		t, context.Background(), store, stored.ID(),
		llm.Message{Role: llm.RoleUser, Content: "inspect workspace"},
		llm.Message{Role: llm.RoleAssistant, Calls: []llm.Call{toolCall}},
		llm.Message{Role: llm.RoleTool, ToolID: "tool-1", Name: "read", Content: "# ion\n"},
		llm.Message{Role: llm.RoleAssistant, Content: "Done with `README.md`."},
	)

	resumed, err := store.ResumeSession(context.Background(), stored.ID())
	if err != nil {
		t.Fatalf("resume session: %v", err)
	}
	entries, err := resumed.Entries(context.Background())
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	requireEntriesInOrder(t, entries, []entryWant{
		{role: session.RoleUser, content: "inspect workspace"},
		{role: session.RoleTool, content: "# ion"},
		{role: session.RoleAgent, content: "Done with `README.md`."},
	})
}

func TestCoreLoopSmokeCancelPersistsTerminalEntry(t *testing.T) {
	model, sess, store, stored := newCoreLoopSmokeModel(t)

	updated, _ := model.Update(session.TurnStart{})
	model = testModel(t, updated)
	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyEsc})
	model = testModel(t, updated)

	if cmd == nil {
		t.Fatal("expected cancel command")
	}
	runCommandTree(t, cmd)
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
	requireEntry(t, entries, session.RoleSystem, "Canceled by user")
}

func TestCoreLoopSmokeStreamCloseDuringTurnStopsBusyState(t *testing.T) {
	model, _, store, stored := newCoreLoopSmokeModel(t)

	updated, _ := model.Update(session.TurnStart{})
	model = testModel(t, updated)
	updated, cmd := model.Update(streamClosedMsg{generation: model.Model.EventGeneration})
	model = testModel(t, updated)

	if cmd == nil {
		t.Fatal("stream close during turn should print terminal error")
	}
	if model.InFlight.Thinking {
		t.Fatal("thinking stayed true after stream close")
	}
	if model.Progress.Mode != stateError {
		t.Fatalf("progress mode = %v, want error", model.Progress.Mode)
	}
	if model.Progress.LastError != "session event stream closed" {
		t.Fatalf("last error = %q, want stream closed", model.Progress.LastError)
	}
	runSequencePrefix(t, cmd, 2)

	resumed, err := store.ResumeSession(context.Background(), stored.ID())
	if err != nil {
		t.Fatalf("resume session: %v", err)
	}
	entries, err := resumed.Entries(context.Background())
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	requireEntry(t, entries, session.RoleSystem, "Error: session event stream closed")
}

func TestCoreLoopSmokeProviderLimitErrorPersistsStopTrace(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	stored := &stubStorageSession{}
	model := New(
		stubBackend{
			sess:     &stubSession{events: make(chan session.AgentEvent)},
			provider: "fake",
			model:    "model",
		},
		stored,
		nil,
		"/tmp/ion-smoke",
		"main",
		"dev",
		nil,
	)

	updated, _ := model.Update(session.TurnStart{})
	model = testModel(t, updated)
	updated, cmd := model.Update(session.TurnError{Err: errors.New("status 429: rate limit exceeded")})
	model = testModel(t, updated)
	runSequencePrefix(t, cmd, 2)

	if model.Progress.Mode != stateError {
		t.Fatalf("progress mode = %v, want error", model.Progress.Mode)
	}
	if !strings.Contains(model.Progress.LastError, "API rate limit") {
		t.Fatalf("last error = %q, want rate limit prefix", model.Progress.LastError)
	}
	var found bool
	for _, appended := range stored.appends {
		decision, ok := appended.(session.StoreRoutingDecision)
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

	updated, _ := model.Update(session.TurnStart{})
	model = testModel(t, updated)
	updated, cmd := model.Update(session.TokenUsage{Input: 20, Output: 3, Cost: 0.02})
	model = testModel(t, updated)
	runSequencePrefix(t, cmd, 1)
	updated, cmd = model.Update(session.TurnError{Err: errors.New("status 429: rate limit exceeded")})
	model = testModel(t, updated)
	runSequencePrefix(t, cmd, 3)

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
	requireEntry(t, entries, session.RoleSystem, "Error: API rate limit")

	input, output, cost, err := resumed.Usage(context.Background())
	if err != nil {
		t.Fatalf("usage: %v", err)
	}
	if input != 20 || output != 3 || cost != 0.02 {
		t.Fatalf("usage = %d/%d/%f, want 20/3/0.02", input, output, cost)
	}
}

func TestCoreLoopSmokeBudgetCancellationPersistsForResume(t *testing.T) {
	model, _, store, stored := newCoreLoopSmokeModel(t)
	model.Model.Config = &config.Config{MaxTurnCost: 0.01}

	updated, _ := model.Update(session.TurnStart{})
	model = testModel(t, updated)
	updated, cmd := model.Update(session.TokenUsage{Input: 20, Output: 3, Cost: 0.02})
	model = testModel(t, updated)
	runSequencePrefix(t, cmd, 4)

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
	requireEntry(t, entries, session.RoleSystem, "Canceled: turn cost limit reached")

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
		stubBackend{
			sess:     &stubSession{events: make(chan session.AgentEvent)},
			provider: "fake",
			model:    "model",
		},
		stored,
		nil,
		"/tmp/ion-smoke",
		"main",
		"dev",
		nil,
	)

	status := "Network error. Retrying in 2s... Ctrl+C stops."
	updated, cmd := model.Update(session.StatusChange{Status: status})
	model = testModel(t, updated)
	runSequencePrefix(t, cmd, 1)

	if model.Progress.Status != status {
		t.Fatalf("status = %q, want retry status", model.Progress.Status)
	}
	var found bool
	for _, appended := range stored.appends {
		record, ok := appended.(session.StoreStatus)
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
	updated, cmd := model.Update(session.StatusChange{Status: status})
	model = testModel(t, updated)
	runSequencePrefix(t, cmd, 1)

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
		stubBackend{
			sess:     &stubSession{events: make(chan session.AgentEvent)},
			provider: "fake",
			model:    "model",
		},
		stored,
		nil,
		"/tmp/ion-smoke",
		"main",
		"dev",
		nil,
	)

	updated, _ := model.Update(session.ToolCallStart{
		ToolUseID: "tool-1",
		ToolName:  "bash",
		Args:      `curl -H "Authorization: Bearer abc.def-123" https://example.test`,
	})
	model = testModel(t, updated)

	if model.InFlight.Pending == nil {
		t.Fatal("expected pending tool preview")
	}
	if strings.Contains(model.InFlight.Pending.Title, "abc.def-123") {
		t.Fatalf("tool preview leaked token: %q", model.InFlight.Pending.Title)
	}
	if !strings.Contains(model.InFlight.Pending.Title, "[redacted-secret]") {
		t.Fatalf("tool preview missing redaction marker: %q", model.InFlight.Pending.Title)
	}
	if len(stored.messages) != 0 {
		t.Fatalf("tool start should not be app-persisted: %#v", stored.messages)
	}
}

func newCoreLoopSmokeModel(t *testing.T) (Model, *stubSession, session.SessionStore, session.SessionHandle) {
	t.Helper()
	t.Setenv("HOME", t.TempDir())

	store, err := session.NewCantoStore(filepath.Join(t.TempDir(), "store"))
	if err != nil {
		t.Fatalf("new store: %v", err)
	}
	stored, err := store.OpenSession(context.Background(), "/tmp/ion-smoke", "fake/model", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}
	sess := &stubSession{events: make(chan session.AgentEvent)}
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

type entryWant struct {
	role    session.Role
	content string
}

func requireEntriesInOrder(t *testing.T, entries []session.Entry, wants []entryWant) {
	t.Helper()
	start := 0
	for _, want := range wants {
		found := false
		for i := start; i < len(entries); i++ {
			if entries[i].Role == want.role && strings.Contains(entries[i].Content, want.content) {
				start = i + 1
				found = true
				break
			}
		}
		if !found {
			t.Fatalf(
				"missing ordered %s entry containing %q after %d in %#v",
				want.role,
				want.content,
				start,
				entries,
			)
		}
	}
}

func appendCantoHistory(
	t *testing.T,
	ctx context.Context,
	store session.SessionStore,
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
