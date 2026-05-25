package app

import (
	"charm.land/bubbles/v2/spinner"
	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/session"
)

func (m *Model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	if m == nil {
		return m, nil
	}
	next, cmd := m.update(msg)
	*m = next
	return m, cmd
}

func (m Model) update(msg tea.Msg) (Model, tea.Cmd) {
	if next, cmd, ok := m.dispatchAppControlMessage(msg); ok {
		return next, cmd
	}
	if next, cmd, ok := m.dispatchRuntimeControllerMessage(msg); ok {
		return next, cmd
	}
	if next, cmd, ok := m.dispatchPickerControllerMessage(msg); ok {
		return next, cmd
	}
	if next, cmd, ok := m.dispatchTurnControllerMessage(msg); ok {
		return next, cmd
	}
	if next, cmd, ok := m.dispatchInputMessage(msg); ok {
		return next, cmd
	}

	return m, m.updateComposer(msg)
}

func (m Model) dispatchAppControlMessage(msg tea.Msg) (Model, tea.Cmd, bool) {
	switch msg := msg.(type) {
	case spinner.TickMsg:
		var cmd tea.Cmd
		m.Input.Spinner, cmd = m.Input.Spinner.Update(msg)
		return m, cmd, true

	case tea.WindowSizeMsg:
		next, cmd := m.handleWindowSize(msg)
		return next, cmd, true

	case clearPendingMsg:
		if msg.action == m.Input.Pending {
			m.clearPendingAction()
		}
		return m, nil, true

	case deferredEnterMsg:
		next, cmd := m.handleDeferredEnter()
		return next, cmd, true

	case sessionCompactedMsg:
		next, cmd := m.handleSessionCompacted(msg)
		return next, cmd, true

	case sessionCostMsg:
		next, cmd := m.handleSessionCost(msg)
		return next, cmd, true

	case sessionUsageLoadedMsg:
		next, cmd := m.handleSessionUsageLoaded(msg)
		return next, cmd, true

	case gitDiffStatsMsg:
		next, cmd := m.handleGitDiffStats(msg)
		return next, cmd, true

	case externalEditorFinishedMsg:
		next, cmd := m.handleExternalEditorFinished(msg)
		return next, cmd, true

	case fileReferenceCompletionMsg:
		next, cmd := m.handleFileReferenceCompletion(msg)
		return next, cmd, true

	case localErrorMsg:
		next, cmd := m.handleLocalError(msg.err)
		return next, cmd, true

	case localEntriesMsg:
		next, cmd := m.handleLocalEntries(msg)
		return next, cmd, true

	case terminalCommitLinesMsg:
		return m, terminalCommitFlushCmd(msg.lines...), true
	}

	return m, nil, false
}

func (m Model) dispatchRuntimeControllerMessage(msg tea.Msg) (Model, tea.Cmd, bool) {
	switch msg := msg.(type) {
	case runtimeSwitchedMsg:
		next, cmd := m.handleRuntimeSwitched(msg)
		return next, cmd, true

	case runtimeTransitionCommittedMsg:
		next, cmd := m.handleRuntimeTransitionCommitted(msg)
		return next, cmd, true

	case runtimeSwitchErrorMsg:
		next, cmd := m.handleRuntimeSwitchError(msg)
		return next, cmd, true

	case resumeSessionSelectedMsg:
		next, cmd := m.handleResumeSessionSelected(msg)
		return next, cmd, true

	case providerSelectionResolvedMsg:
		next, cmd := m.handleProviderSelectionResolved(msg)
		return next, cmd, true

	case modelPickerSetupResolvedMsg:
		next, cmd := m.handleModelPickerSetupResolved(msg)
		return next, cmd, true

	case setupPromptSavedMsg:
		next, cmd := m.handleSetupPromptSaved(msg)
		return next, cmd, true

	case settingsCommandMsg:
		next, cmd := m.handleSettingsCommandResult(msg)
		return next, cmd, true
	}

	return m, nil, false
}

func (m Model) dispatchPickerControllerMessage(msg tea.Msg) (Model, tea.Cmd, bool) {
	switch msg := msg.(type) {
	case modelPickerLoadedMsg:
		next, cmd := m.handleModelPickerLoaded(msg)
		return next, cmd, true

	case sessionPickerLoadedMsg:
		next, cmd := m.handleSessionPickerLoaded(msg)
		return next, cmd, true
	}

	return m, nil, false
}

func (m Model) dispatchTurnControllerMessage(msg tea.Msg) (Model, tea.Cmd, bool) {
	switch msg := msg.(type) {
	case sessionEventMsg:
		if msg.generation != m.Model.EventGeneration {
			return m, nil, true
		}
		next, cmd := m.handleSessionEvent(msg.event)
		return next, cmd, true

	case streamClosedMsg:
		if msg.generation != m.Model.EventGeneration {
			return m, nil, true
		}
		next, cmd := m.handleStreamClosed()
		return next, cmd, true

	case queuedTurnMsg:
		next, cmd := m.handleQueuedTurn(msg)
		return next, cmd, true

	case turnSubmitResultMsg:
		next, cmd := m.handleTurnSubmitResult(msg)
		return next, cmd, true

	case steeringResultMsg:
		next, cmd := m.handleSteeringResult(msg)
		return next, cmd, true

	case followUpResultMsg:
		next, cmd := m.handleFollowUpResult(msg)
		return next, cmd, true

	case queuedInputClearResultMsg:
		if msg.err != nil {
			next, cmd := m.handleLocalError(msg.err)
			return next, cmd, true
		}
		return m, nil, true

	case turnCancelResultMsg:
		next, cmd := m.handleTurnCancelResult(msg)
		return next, cmd, true

	case session.StatusChanged,
		session.TokenUsage,
		session.QueuedInputUpdated,
		session.TurnStarted,
		session.TurnSavePoint,
		session.TurnFinished,
		session.ThinkingDelta,
		session.UserMessage,
		session.AgentDelta,
		session.AgentMessage,
		session.ToolCallStarted,
		session.ToolOutputDelta,
		session.ToolResult,
		session.VerificationResult,
		session.ChildRequested,
		session.ChildStarted,
		session.ChildDelta,
		session.ChildCompleted,
		session.ChildBlocked,
		session.ChildFailed,
		session.ChildCanceled,
		session.Error:
		next, cmd := m.handleSessionEvent(msg.(session.Event))
		return next, cmd, true
	}

	return m, nil, false
}

func (m Model) dispatchInputMessage(msg tea.Msg) (Model, tea.Cmd, bool) {
	switch msg := msg.(type) {
	case tea.PasteMsg:
		if m.Picker.Session != nil {
			next, cmd := m.handleSessionPickerPaste(msg)
			return next, cmd, true
		}
		if m.Picker.Setup != nil {
			next, cmd := m.handleSetupPromptPaste(msg)
			return next, cmd, true
		}
		if m.Picker.Overlay != nil {
			next, cmd := m.handlePickerPaste(msg)
			return next, cmd, true
		}
		next, cmd := m.handlePaste(msg)
		return next, cmd, true

	case tea.KeyPressMsg:
		next, cmd := m.handleKey(msg)
		return next, cmd, true
	}

	return m, nil, false
}
