package app

import (
	"fmt"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
	"github.com/nijaru/ion/agent"
	"github.com/nijaru/ion/session"
)

type userMessageItem struct {
	ID       string
	ParentID string
	Text     string
	Index    int
}

type userMessageForkPickerState struct {
	items         []userMessageItem
	selectedIndex int
	loading       bool
	err           string
	maxVisible    int
}

type userMessagesLoadedMsg struct {
	generation uint64
	items      []userMessageItem
	err        error
}

func (m Model) openUserMessageForkPicker() (Model, tea.Cmd) {
	reader, ok := m.Model.Runner.(agent.SessionReader)
	if !ok {
		return m, m.terminalCommit().Entries(systemEntry("⚠ session reader unavailable"))
	}
	m.Picker.UserMessage = &userMessageForkPickerState{
		loading:    true,
		maxVisible: 10,
	}
	generation := m.Model.EventGeneration
	ctx := m.runtimeOperationContext()
	return m, func() tea.Msg {
		entries, err := reader.SessionBranch(ctx)
		if err != nil {
			return userMessagesLoadedMsg{generation: generation, err: err}
		}
		var items []userMessageItem
		idx := 1
		for _, e := range entries {
			if e == nil {
				continue
			}
			if session.EntryRole(e) == session.RoleUser {
				text := strings.TrimSpace(session.EntryText(e))
				if text != "" {
					items = append(items, userMessageItem{
						ID:       e.ID(),
						ParentID: e.ParentID(),
						Text:     text,
						Index:    idx,
					})
					idx++
				}
			}
		}
		return userMessagesLoadedMsg{
			generation: generation,
			items:      items,
		}
	}
}

func (m Model) handleUserMessagesLoaded(msg userMessagesLoadedMsg) (Model, tea.Cmd) {
	if msg.generation != m.Model.EventGeneration || m.Picker.UserMessage == nil {
		return m, nil
	}
	if msg.err != nil {
		m.Picker.UserMessage.loading = false
		m.Picker.UserMessage.err = msg.err.Error()
		return m, nil
	}
	if len(msg.items) == 0 {
		m.Picker.UserMessage = nil
		return m, m.terminalCommit().Entries(systemEntry("ℹ No user messages in conversation to fork from."))
	}
	m.Picker.UserMessage.loading = false
	m.Picker.UserMessage.items = msg.items
	m.Picker.UserMessage.selectedIndex = len(msg.items) - 1
	return m, nil
}

func (m Model) handleUserMessageForkPickerKey(msg tea.KeyPressMsg) (Model, tea.Cmd) {
	p := m.Picker.UserMessage
	if p == nil {
		return m, nil
	}
	switch msg.String() {
	case "up", "k":
		if len(p.items) == 0 {
			return m, nil
		}
		if p.selectedIndex == 0 {
			p.selectedIndex = len(p.items) - 1
		} else {
			p.selectedIndex--
		}
		return m, nil

	case "down", "j":
		if len(p.items) == 0 {
			return m, nil
		}
		if p.selectedIndex >= len(p.items)-1 {
			p.selectedIndex = 0
		} else {
			p.selectedIndex++
		}
		return m, nil

	case "esc", "q":
		m.Picker.UserMessage = nil
		m.Picker.OverlayClosedAt = time.Now()
		return m, nil

	case "enter":
		if len(p.items) == 0 || p.selectedIndex < 0 || p.selectedIndex >= len(p.items) {
			m.Picker.UserMessage = nil
			return m, nil
		}
		selected := p.items[p.selectedIndex]
		m.Picker.UserMessage = nil
		m.Picker.OverlayClosedAt = time.Now()

		navigator, ok := m.Model.Runner.(agent.SessionNavigator)
		if !ok {
			return m, m.terminalCommit().Entries(systemEntry("⚠ session navigation unavailable"))
		}
		generation := m.Model.EventGeneration
		ctx := m.runtimeOperationContext()
		targetLeafID := selected.ParentID
		promptText := selected.Text
		return m, func() tea.Msg {
			result, err := navigator.NavigateTree(ctx, targetLeafID, agent.NavigateOptions{})
			if err != nil {
				return undoResultMsg{generation: generation, err: err}
			}
			return undoResultMsg{
				generation:   generation,
				targetLeafID: result.LeafID,
				promptText:   promptText,
			}
		}
	}
	return m, nil
}

func (m Model) renderUserMessageForkPicker() string {
	p := m.Picker.UserMessage
	if p == nil {
		return ""
	}
	width := m.App.Width
	if width <= 0 {
		width = 80
	}
	var b strings.Builder

	borderStyle := lipgloss.NewStyle().Foreground(lipgloss.Color("240"))
	accentStyle := lipgloss.NewStyle().Foreground(lipgloss.Color("6")).Bold(true)
	mutedStyle := lipgloss.NewStyle().Foreground(lipgloss.Color("245"))
	boldStyle := lipgloss.NewStyle().Bold(true)

	b.WriteString(boldStyle.Render("Fork from Message") + "\n")
	b.WriteString(mutedStyle.Render("Select a user message to go back in conversation and fork into a new branch") + "\n")
	b.WriteString(borderStyle.Render(strings.Repeat("─", min(width, 78))) + "\n")

	if p.loading {
		b.WriteString(mutedStyle.Render("  Loading conversation messages...") + "\n")
		return b.String()
	}
	if p.err != "" {
		b.WriteString(lipgloss.NewStyle().Foreground(lipgloss.Color("1")).Render("  ⚠ "+p.err) + "\n")
		return b.String()
	}
	if len(p.items) == 0 {
		b.WriteString(mutedStyle.Render("  No user messages found") + "\n")
		return b.String()
	}

	maxVis := p.maxVisible
	if maxVis <= 0 {
		maxVis = 10
	}
	startIdx := max(0, min(p.selectedIndex-maxVis/2, len(p.items)-maxVis))
	endIdx := min(startIdx+maxVis, len(p.items))

	for i := startIdx; i < endIdx; i++ {
		item := p.items[i]
		isSelected := (i == p.selectedIndex)

		normalizedText := strings.ReplaceAll(item.Text, "\n", " ")
		maxTextWidth := max(20, width-8)
		if len(normalizedText) > maxTextWidth {
			normalizedText = normalizedText[:maxTextWidth-1] + "…"
		}

		if isSelected {
			b.WriteString(accentStyle.Render("› ") + boldStyle.Render(normalizedText) + "\n")
		} else {
			b.WriteString("  " + normalizedText + "\n")
		}

		meta := fmt.Sprintf("  Message %d of %d", item.Index, len(p.items))
		b.WriteString(mutedStyle.Render(meta) + "\n\n")
	}

	if startIdx > 0 || endIdx < len(p.items) {
		scroll := fmt.Sprintf("  (%d/%d)", p.selectedIndex+1, len(p.items))
		b.WriteString(mutedStyle.Render(scroll) + "\n")
	}

	b.WriteString(borderStyle.Render(strings.Repeat("─", min(width, 78))) + "\n")
	b.WriteString(mutedStyle.Render("  ↑/↓ select • Enter fork • Esc cancel") + "\n")

	return b.String()
}
