package app

import (
	"context"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

type stubBackend struct {
	sess *stubSession
}

func (b stubBackend) Name() string { return "stub" }

func (b stubBackend) Bootstrap() backend.Bootstrap {
	return backend.Bootstrap{
		Entries: []session.Entry{{Role: session.System, Content: "boot"}},
		Status:  "ready",
	}
}

func (b stubBackend) Session() session.AgentSession { return b.sess }

func (b stubBackend) SetStore(s storage.Store) {}

func (b stubBackend) SetSession(s storage.Session) {}

type stubSession struct {
	events chan session.Event
}

func (s *stubSession) Open(ctx context.Context) error                    { return nil }
func (s *stubSession) Resume(ctx context.Context, id string) error       { return nil }
func (s *stubSession) SubmitTurn(ctx context.Context, turn string) error { return nil }
func (s *stubSession) CancelTurn(ctx context.Context) error              { return nil }
func (s *stubSession) Close() error                                      { return nil }
func (s *stubSession) Events() <-chan session.Event                      { return s.events }
func (s *stubSession) Approve(ctx context.Context, id string, ok bool) error { return nil }
func (s *stubSession) RegisterMCPServer(ctx context.Context, cmd string, args ...string) error {
	return nil
}
func (s *stubSession) ID() string                                        { return "stub" }
func (s *stubSession) Meta() map[string]string                           { return nil }

func readyModel(t *testing.T) Model {
	t.Helper()
	sess := &stubSession{events: make(chan session.Event)}
	b := stubBackend{sess: sess}
	model := New(b, nil)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 100, Height: 30})
	ready, ok := updated.(Model)
	if !ok {
		t.Fatalf("expected Model after window size update")
	}
	return ready
}

func TestModelStreamsAndCommitsPendingEntry(t *testing.T) {
	model := readyModel(t)

	updated, _ := model.Update(session.TurnStarted{})
	model = updated.(Model)
	updated, _ = model.Update(session.AssistantDelta{Delta: "streamed reply"})
	model = updated.(Model)

	if model.pending == nil || model.pending.Content != "streamed reply" {
		t.Fatalf("expected pending streamed assistant entry, got %#v", model.pending)
	}

	updated, cmd := model.Update(session.AssistantMessage{})
	model = updated.(Model)

	if model.pending != nil {
		t.Fatalf("expected pending entry to be cleared after flush")
	}

	// Verify that a Println command was returned
	if cmd == nil {
		t.Fatalf("expected tea.Println command after finalizing message")
	}
}

func TestToolEntryFlushesToTranscript(t *testing.T) {
	model := readyModel(t)
	updated, _ := model.Update(session.ToolCallStarted{
		ToolName:  "bash",
		Args:      "ls",
	})
	model = updated.(Model)

	if model.pending == nil || model.pending.Role != session.Tool {
		t.Fatalf("expected pending tool entry")
	}
	
	updated, cmd := model.Update(session.ToolResult{
		ToolName:  "bash",
		Result:    "ok",
	})
	model = updated.(Model)

	if model.pending != nil {
		t.Fatalf("expected pending entry to be cleared")
	}
	if cmd == nil {
		t.Fatalf("expected tea.Println command for tool result")
	}
}

func TestLayoutClampsComposerHeight(t *testing.T) {
	model := readyModel(t)
	
	// Initial height should be min (1)
	model.layout()
	if got := model.composer.Height(); got != minComposerHeight {
		t.Fatalf("expected initial composer height %d, got %d", minComposerHeight, got)
	}

	// 5 lines of text
	model.composer.SetValue("1\n2\n3\n4\n5")
	model.layout()

	// Should be 5
	if got := model.composer.Height(); got != 5 {
		t.Fatalf("expected composer height 5 for 5 lines, got %d", got)
	}

	// Over the max (10)
	model.composer.SetValue(strings.Repeat("line\n", 20))
	model.layout()

	if got := model.composer.Height(); got != maxComposerHeight {
		t.Fatalf("expected composer height to clamp to %d, got %d", maxComposerHeight, got)
	}
}
