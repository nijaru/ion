package app

import (
	"strings"

	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"
)

const composerPrompt = "› "

func (m Model) renderComposer() string {
	return renderComposerView(m.Input.Composer.View(), m.shellWidth())
}

func renderComposerView(view string, width int) string {
	rows := strings.Split(view, "\n")
	continuationPrompt := strings.Repeat(" ", composerPromptWidth())
	for i := range rows {
		prompt := continuationPrompt
		if i == 0 {
			prompt = composerPrompt
		}
		rows[i] = fitLine(prompt+rows[i], width)
	}
	return strings.Join(rows, "\n")
}

func composerPromptWidth() int {
	return ansi.StringWidth(composerPrompt)
}

func (m Model) renderComposerCompletions() string {
	if m.Picker.Overlay != nil ||
		m.Picker.Session != nil ||
		m.Picker.Setup != nil ||
		m.Input.Completion == nil ||
		len(m.Input.Completion.items) == 0 {
		return ""
	}

	labelWidth := 0
	for _, item := range m.Input.Completion.items {
		labelWidth = max(labelWidth, lipgloss.Width(item.Label))
	}

	lines := make([]string, 0, len(m.Input.Completion.items))
	for _, item := range m.Input.Completion.items {
		line := item.Label
		if item.Detail != "" {
			line += strings.Repeat(" ", max(2, labelWidth-lipgloss.Width(item.Label)+2))
			line += item.Detail
		}
		lines = append(lines, m.shellPaddedLine(m.st.dim, line))
	}
	return strings.Join(lines, "\n")
}
