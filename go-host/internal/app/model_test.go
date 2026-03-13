package app

import (
	"context"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/go-host/internal/backend"
	"github.com/nijaru/ion/go-host/internal/session"
)

type stubBackend struct {
	sess *stubSession
}

func (b stubBackend) Name() string { return "stub" }

func (b stubBackend) Bootstrap() backend.Bootstrap {
	return backend.Bootstrap{
		Entries: []session.Entry{{Role: session.RoleSystem, Content: "boot"}},
		Status:  "ready",
	}
}

func (b stubBackend) Session() session.AgentSession { return b.sess }

type stubSession struct {
	events chan session.Event
}

func (s *stubSession) Open(ctx context.Context) error                    { return nil }
func (s *stubSession) Resume(ctx context.Context, id string) error       { return nil }
func (s *stubSession) SubmitTurn(ctx context.Context, turn string) error { return nil }
func (s *stubSession) CancelTurn(ctx context.Context) error              { return nil }
func (s *stubSession) Close() error                                      { return nil }
func (s *stubSession) Events() <-chan session.Event                      { return s.events }

func readyModel(t *testing.T) Model {
	t.Helper()
	sess := &stubSession{events: make(chan session.Event)}
	b := stubBackend{sess: sess}
	model := New(b)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 100, Height: 30})
	ready, ok := updated.(Model)
	if !ok {
		t.Fatalf("expected Model after window size update")
	}
	return ready
}

func TestModelStreamsAndCommitsPendingEntry(t *testing.T) {
	model := readyModel(t)

	updated, _ := model.Update(session.EventTurnStarted{})
	model = updated.(Model)
	updated, _ = model.Update(session.EventAssistantDelta{Delta: "streamed reply"})
	model = updated.(Model)

	if model.pending == nil || model.pending.Content != "streamed reply" {
		t.Fatalf("expected pending streamed assistant entry, got %#v", model.pending)
	}

	updated, _ = model.Update(session.EventAssistantMessage{})
	model = updated.(Model)

	if model.pending != nil {
		t.Fatalf("expected pending entry to be committed")
	}
	if got := model.entries[len(model.entries)-1].Content; got != "streamed reply" {
		t.Fatalf("expected last entry to be committed streamed reply, got %q", got)
	}
	if !strings.Contains(model.viewport.GetContent(), "streamed reply") {
		t.Fatalf("expected viewport content to contain streamed reply")
	}
}

func TestToolEntryRendersIntoTranscript(t *testing.T) {
	model := readyModel(t)
	updated, _ := model.Update(session.EventToolCallStarted{
		ToolName: "bash",
		Args:     "ls",
	})
	model = updated.(Model)
	
	updated, _ = model.Update(session.EventToolResult{
		ToolName: "bash",
		Result:   "ok",
	})
	model = updated.(Model)

	content := model.viewport.GetContent()
	if !strings.Contains(content, "bash(ls)") {
		t.Fatalf("expected tool title in viewport content: %q", content)
	}
	if !strings.Contains(content, "ok") {
		t.Fatalf("expected tool content in viewport content: %q", content)
	}
}

func TestLayoutClampsComposerHeight(t *testing.T) {
	model := readyModel(t)
	model.composer.SetValue(strings.Repeat("line\n", 20))
	model.layout()

	if got := model.composer.Height(); got != maxComposerHeight {
		t.Fatalf("expected composer height to clamp to %d, got %d", maxComposerHeight, got)
	}
	if got := model.viewport.Height(); got < 3 {
		t.Fatalf("expected viewport height floor, got %d", got)
	}
}
