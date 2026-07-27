package app

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/internal/agent"
)

const (
	branchSummaryNoSummary = iota
	branchSummaryGenerate
	branchSummaryCustom
)

func (m Model) openBranchSummaryPrompt(targetID string) (Model, tea.Cmd) {
	if m.Model.Runner == nil {
		return m, m.terminalCommit().Entries(
			systemEntry("⚠ tree navigation is unavailable without an agent runner"),
		)
	}
	m.Picker.BranchSummary = &branchSummaryPromptState{targetID: targetID}
	return m, nil
}

func (m Model) handleBranchSummaryPromptKey(msg tea.KeyPressMsg) (Model, tea.Cmd) {
	prompt := m.Picker.BranchSummary
	if prompt == nil {
		return m, nil
	}
	if prompt.navigating {
		switch msg.String() {
		case "esc", "ctrl+c", "ctrl+d":
			if m.Model.Runner == nil {
				return m, nil
			}
			runner := m.Model.Runner
			generation := m.Model.EventGeneration
			return m, func() tea.Msg {
				if _, _, err := runner.Abort(); err != nil {
					return branchNavigationCancelMsg{
						generation: generation,
						err:        fmt.Errorf("cancel branch navigation: %w", err),
					}
				}
				return branchNavigationCancelMsg{generation: generation}
			}
		}
		return m, nil
	}

	if prompt.custom {
		return m.handleCustomBranchSummaryKey(msg)
	}

	switch msg.String() {
	case "esc", "ctrl+c", "ctrl+d":
		m.Picker.BranchSummary = nil
		return m, nil
	case "up":
		if prompt.choice > branchSummaryNoSummary {
			prompt.choice--
		}
	case "down":
		if prompt.choice < branchSummaryCustom {
			prompt.choice++
		}
	case "enter":
		switch prompt.choice {
		case branchSummaryNoSummary:
			return m.startTreeNavigation(agent.NavigateOptions{})
		case branchSummaryGenerate:
			return m.startTreeNavigation(agent.NavigateOptions{Summarize: true})
		case branchSummaryCustom:
			prompt.custom = true
			prompt.err = ""
		}
	}
	return m, nil
}

func (m Model) handleBranchNavigationCancel(msg branchNavigationCancelMsg) (Model, tea.Cmd) {
	if msg.generation != m.Model.EventGeneration {
		return m, nil
	}
	if msg.err != nil {
		return m.handleLocalError(msg.err)
	}
	return m, nil
}

func (m Model) handleCustomBranchSummaryKey(msg tea.KeyPressMsg) (Model, tea.Cmd) {
	prompt := m.Picker.BranchSummary
	if prompt == nil {
		return m, nil
	}
	switch msg.String() {
	case "esc", "ctrl+c", "ctrl+d":
		prompt.custom = false
		prompt.value = ""
		prompt.err = ""
	case "backspace":
		if prompt.value != "" {
			_, size := utf8.DecodeLastRuneInString(prompt.value)
			prompt.value = prompt.value[:len(prompt.value)-size]
		}
	case "enter":
		return m.startTreeNavigation(agent.NavigateOptions{
			Summarize:          true,
			CustomInstructions: strings.TrimSpace(prompt.value),
		})
	default:
		if text, ok := keyTextInput(msg); ok {
			prompt.value += text
			prompt.err = ""
		}
	}
	return m, nil
}

func (m Model) handleBranchSummaryPaste(msg tea.PasteMsg) (Model, tea.Cmd) {
	prompt := m.Picker.BranchSummary
	if prompt == nil || prompt.navigating || !prompt.custom {
		return m, nil
	}
	if text := inlinePasteText(msg.Content); text != "" {
		prompt.value += text
		prompt.err = ""
	}
	return m, nil
}

func (m Model) startTreeNavigation(opts agent.NavigateOptions) (Model, tea.Cmd) {
	prompt := m.Picker.BranchSummary
	if prompt == nil || m.Model.Runner == nil {
		return m, nil
	}
	prompt.navigating = true
	prompt.err = ""
	targetID := prompt.targetID
	runner := m.Model.Runner
	generation := m.Model.EventGeneration
	ctx := m.runtimeOperationContext()
	navigator, ok := runner.(agent.SessionNavigator)
	if !ok {
		prompt.navigating = false
		prompt.err = "tree navigation is unavailable"
		return m, nil
	}
	return m, func() tea.Msg {
		_, err := navigator.NavigateTree(ctx, targetID, opts)
		return treePickerMoveMsg{generation: generation, err: err, cancelled: errors.Is(err, context.Canceled)}
	}
}
