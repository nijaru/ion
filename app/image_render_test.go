package app

import (
	"strings"
	"testing"
	"time"

	"github.com/charmbracelet/x/ansi"
	"github.com/nijaru/ion/session"
)

func TestRenderEntryWithImages(t *testing.T) {
	model := readyModel(t)

	pngBytes := []byte{0x89, 'P', 'N', 'G', '\r', '\n', 0x1a, '\n', 0x00}
	userMsg := &session.UserMessage{
		Content: []session.Content{
			session.TextContent{Text: "Look at this screenshot"},
			session.ImageContent{Data: pngBytes, MimeType: "image/png"},
		},
		Timestamp: time.Now(),
	}

	entry := &session.MessageEntry{
		EntryBase: session.EntryBase{ID: "msg-img-1", Timestamp: time.Now()},
		Message:   userMsg,
	}

	rendered := model.renderEntry(entry)
	stripped := ansi.Strip(rendered)

	if !strings.Contains(stripped, "Look at this screenshot") {
		t.Fatalf("rendered output missing text: %q", stripped)
	}
	hasImageOutput := strings.Contains(rendered, "\x1b]1337;") ||
		strings.Contains(rendered, "\x1b_G") ||
		strings.Contains(stripped, "[Image: image/png (9 bytes)]")
	if !hasImageOutput {
		t.Fatalf("rendered output missing image escape or fallback: %q", rendered)
	}
}

func TestRenderToolResultWithImages(t *testing.T) {
	model := readyModel(t)

	pngBytes := []byte{0x89, 'P', 'N', 'G', '\r', '\n', 0x1a, '\n', 0x00}
	toolMsg := &session.ToolResultMessage{
		ToolName: "read",
		Title:    "read screenshot.png",
		Content: []session.Content{
			session.TextContent{Text: "Read image file [image/png]"},
			session.ImageContent{Data: pngBytes, MimeType: "image/png"},
		},
		Timestamp: time.Now(),
	}

	entry := &session.MessageEntry{
		EntryBase: session.EntryBase{ID: "msg-tool-img-1", Timestamp: time.Now()},
		Message:   toolMsg,
	}

	rendered := model.renderEntry(entry)
	stripped := ansi.Strip(rendered)

	if !strings.Contains(stripped, "screenshot.png") {
		t.Fatalf("rendered output missing tool title: %q", stripped)
	}
	hasImageOutput := strings.Contains(rendered, "\x1b]1337;") ||
		strings.Contains(rendered, "\x1b_G") ||
		strings.Contains(stripped, "[Image: image/png (9 bytes)]")
	if !hasImageOutput {
		t.Fatalf("rendered output missing tool image escape or fallback: %q", rendered)
	}
}
