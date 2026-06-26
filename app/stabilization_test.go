package app

import (
	"strings"
	"testing"

	"github.com/charmbracelet/x/ansi"
	"github.com/nijaru/ion/session"
)

func TestRenderEntry(t *testing.T) {
	b := &stubBackend{}
	m := New(b, nil, nil, "/tmp", "main", "dev", nil)

	// 1. Agent message with multiple lines
	entry := agentMsgEntry("Line 1\n\nLine 2\n\nLine 3")
	rendered := m.renderEntry(entry)
	expected := "• Line 1\n\n  Line 2\n\n  Line 3"
	if ansi.Strip(rendered) != expected {
		t.Errorf("expected:\n%q\ngot:\n%q", expected, ansi.Strip(rendered))
	}

	// 2. Agent message with reasoning
	entry = &session.MessageEntry{
		Message: &session.AssistantMessage{
			Content: []session.Content{
				session.ThinkingContent{Text: "Thought 1"},
				session.TextContent{Text: "Reply 1"},
			},
		},
	}
	rendered = m.renderEntry(entry)
	if !strings.Contains(ansi.Strip(rendered), "Thinking") {
		t.Error("reasoning marker should be shown by default (collapsed)")
	}
	if strings.Contains(ansi.Strip(rendered), "Thought 1") {
		t.Error("reasoning content should be hidden in collapsed mode")
	}
	if !strings.Contains(ansi.Strip(rendered), "Reply 1") {
		t.Error("expected 'Reply 1' in output")
	}
}
