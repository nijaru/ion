package app

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/go-host/internal/backend"
	"github.com/nijaru/ion/go-host/internal/session"
)

type stubBackend struct{}

func (stubBackend) Name() string { return "stub" }

func (stubBackend) Bootstrap() backend.Bootstrap {
	return backend.Bootstrap{
		Entries: []session.Entry{{Role: session.RoleSystem, Content: "boot"}},
		Status:  "ready",
	}
}

func (stubBackend) Submit(string) tea.Cmd { return nil }

func readyModel(t *testing.T) Model {
	t.Helper()
	model := New(stubBackend{})
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 100, Height: 30})
	ready, ok := updated.(Model)
	if !ok {
		t.Fatalf("expected Model after window size update")
	}
	return ready
}

func TestModelStreamsAndCommitsPendingEntry(t *testing.T) {
	model := readyModel(t)

	updated, _ := model.Update(backend.StreamStartMsg{Role: session.RoleAssistant})
	model = updated.(Model)
	updated, _ = model.Update(backend.StreamDeltaMsg{Delta: "streamed reply"})
	model = updated.(Model)

	if model.pending == nil || model.pending.Content != "streamed reply" {
		t.Fatalf("expected pending streamed assistant entry, got %#v", model.pending)
	}

	updated, _ = model.Update(backend.StreamDoneMsg{})
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
	updated, _ := model.Update(backend.AppendEntryMsg{Entry: session.Entry{
		Role:    session.RoleTool,
		Title:   "bash(ls)",
		Content: "ok",
	}})
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
