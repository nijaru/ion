package app

import (
	"context"
	"errors"
	"strings"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/agent"
)

const (
	branchSummaryNoSummary = iota
	branchSummaryGenerate
	branchSummaryCustom
)

func (m *Model) clearTreeNavigationCancel() {
	if m.Model.treeNavigationCancel != nil {
		m.Model.treeNavigationCancel()
		m.Model.treeNavigationCancel = nil
	}
}

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
			generation := m.Model.EventGeneration
			requestID := m.Model.TreeNavigationRequest
			cancel := m.Model.treeNavigationCancel
			return m, func() tea.Msg {
				if cancel == nil {
					return branchNavigationCancelMsg{
						generation: generation,
						requestID:  requestID,
						err:        errors.New("branch navigation cancellation is unavailable"),
					}
				}
				cancel()
				return branchNavigationCancelMsg{generation: generation, requestID: requestID}
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
	if msg.generation != m.Model.EventGeneration ||
		msg.requestID != m.Model.TreeNavigationRequest {
		return m, nil
	}
	m.clearTreeNavigationCancel()
	if msg.err != nil {
		return m.handleLocalError(msg.err)
	}
	// The cancellation command only canceled the navigation context. The
	// navigation worker still owns the terminal result and must settle before
	// the prompt becomes retryable or event recovery resumes.
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
	if m.localCommandBusy() {
		prompt.err = m.localCommandBusyMessage("navigating the session tree")
		return m, nil
	}
	navigator, ok := m.Model.Runner.(agent.SessionNavigator)
	if !ok {
		prompt.err = "tree navigation is unavailable"
		return m, nil
	}
	prompt.navigating = true
	prompt.err = ""
	targetID := prompt.targetID
	generation := m.Model.EventGeneration
	m.clearTreeNavigationCancel()
	m.Model.TreeNavigationRequest++
	requestID := m.Model.TreeNavigationRequest
	ctx, cancel := context.WithCancel(m.runtimeOperationContext())
	m.Model.treeNavigationCancel = cancel
	return m, func() tea.Msg {
		result, err := navigator.NavigateTree(ctx, targetID, opts)
		return treePickerMoveMsg{
			generation:     generation,
			requestID:      requestID,
			leafID:         result.LeafID,
			editorText:     result.EditorText,
			restoreEditor:  result.RestoreEditor,
			activeProvider: result.ActiveProvider,
			activeModel:    result.ActiveModel,
			err:            err,
			cancelled:      errors.Is(err, context.Canceled),
		}
	}
}
