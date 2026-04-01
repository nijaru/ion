package app

import (
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"
)

const (
	// pasteMarkerMinLines is the minimum number of lines to trigger marker collapse.
	pasteMarkerMinLines = 10
	// pasteMarkerMinChars is the minimum character count to trigger marker collapse.
	pasteMarkerMinChars = 1000
)

// handlePaste intercepts paste events. Large pastes are collapsed into markers
// to prevent textarea rendering lag.
func (m Model) handlePaste(msg tea.PasteMsg) (Model, tea.Cmd) {
	content := msg.Content
	lineCount := strings.Count(content, "\n") + 1

	if lineCount < pasteMarkerMinLines && len(content) < pasteMarkerMinChars {
		// Small paste — pass through to textarea directly.
		var cmd tea.Cmd
		m.Input.Composer, cmd = m.Input.Composer.Update(msg)
		return m, cmd
	}

	// Large paste — create a marker.
	m.pasteSeq++
	placeholder := fmt.Sprintf("[paste #%d +%d lines]", m.pasteSeq, lineCount)
	m.PasteMarkers[placeholder] = pasteMarker{
		placeholder: placeholder,
		content:     content,
	}

	// Insert the placeholder into the textarea.
	current := m.Input.Composer.Value()
	if current != "" {
		current += " "
	}
	m.Input.Composer.SetValue(current + placeholder)
	m.relayoutComposer()
	return m, nil
}

// expandMarkers replaces all paste marker placeholders with their original content.
func (m Model) expandMarkers(text string) string {
	for _, marker := range m.PasteMarkers {
		text = strings.ReplaceAll(text, marker.placeholder, marker.content)
	}
	return text
}
