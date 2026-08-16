package app

import (
	"errors"
	"fmt"
	"strings"

	"charm.land/bubbles/v2/spinner"
	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/agent"
	"github.com/nijaru/ion/session"
)

func (m *Model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	if m == nil {
		return m, nil
	}
	next, cmd := m.update(msg)
	*m = next
	return m, cmd
}

func syntheticAssistantEntry(snapshot agent.RuntimeSnapshot) session.Entry {
	assistant := snapshot.ActiveTurn.Assistant
	id := fmt.Sprintf(
		"synthetic-assistant:%d:%s:%d",
		snapshot.ActiveTurnToken,
		assistant.ResponseID,
		assistant.Timestamp.UnixNano(),
	)
	return &session.MessageEntry{
		EntryBase: session.EntryBase{ID: id},
		Message:   assistant,
	}
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
		if msg.action == m.Input.Pending && msg.requestID == m.Input.PendingActionRequest {
			m.clearPendingAction()
		}
		return m, nil, true

	case deferredEnterMsg:
		if msg.generation != m.Model.EventGeneration {
			return m, nil, true
		}
		next, cmd := m.handleDeferredEnter()
		return next, cmd, true

	case sessionCompactedMsg:
		next, cmd := m.handleSessionCompacted(msg)
		return next, cmd, true

	case sessionCostMsg:
		next, cmd := m.handleSessionCost(msg)
		return next, cmd, true

	case sessionCopiedMsg:
		next, cmd := m.handleSessionCopied(msg)
		return next, cmd, true

	case debugLogWrittenMsg:
		if msg.generation != m.Model.EventGeneration {
			return m, nil, true
		}
		if msg.err != nil {
			next, cmd := m.handleLocalError(msg.err)
			return next, cmd, true
		}
		return m, m.terminalCommit().Entries(systemEntry("Debug log written to " + msg.path)), true

	case sessionUsageLoadedMsg:
		next, cmd := m.handleSessionUsageLoaded(msg)
		return next, cmd, true

	case gitDiffStatsMsg:
		next, cmd := m.handleGitDiffStats(msg)
		return next, cmd, true

	case gitBranchChangedMsg:
		if msg.generation != m.Model.EventGeneration {
			return m, nil, true
		}
		if msg.branch != m.App.Branch {
			m.App.Branch = msg.branch
		}
		return m, m.pollGitBranch(), true

	case directShellResultMsg:
		return m, m.terminalCommit().Entries(systemEntry(msg.content)), true

	case externalEditorFinishedMsg:
		next, cmd := m.handleExternalEditorFinished(msg)
		return next, cmd, true

	case fileReferenceCompletionMsg:
		next, cmd := m.handleFileReferenceCompletion(msg)
		return next, cmd, true

	case skillCompletionMsg:
		next, cmd := m.handleSkillCompletion(msg)
		return next, cmd, true

	case localErrorMsg:
		next, cmd := m.handleLocalError(msg.err)
		return next, cmd, true

	case inputHistoryResultMsg:
		if msg.generation != m.Model.EventGeneration {
			return m, nil, true
		}
		if msg.err != nil {
			next, cmd := m.handleLocalError(msg.err)
			return next, cmd, true
		}
		return m, nil, true

	case runtimeCatalogUpdateMsg:
		if msg.generation != m.Model.EventGeneration {
			return m, nil, true
		}
		if msg.err != nil {
			next, cmd := m.handleLocalError(msg.err)
			return next, cmd, true
		}
		return m, nil, true

	case runtimeLeafSnapshotMsg:
		if msg.generation != m.Model.EventGeneration {
			return m, nil, true
		}
		if msg.treeNavigationRequest != m.Model.TreeNavigationRequest {
			if msg.info != nil && m.Model.SessionCatalog != nil {
				return m, m.persistSessionCatalogInfoCmd(msg.generation, msg.info), true
			}
			if m.Picker.BranchSummary != nil && m.Picker.BranchSummary.navigating {
				return m, nil, true
			}
			return m, m.awaitSessionEvent(), true
		}
		if msg.err != nil {
			next, cmd := m.handleLocalError(msg.err)
			return next, cmd, true
		}
		m.Model.LeafID = strings.TrimSpace(msg.leafID)
		return m, m.persistSessionCatalogInfoCmd(msg.generation, msg.info), true

	case approvalResolveMsg:
		next, cmd := m.handleApprovalResolve(msg)
		return next, cmd, true

	case branchNavigationCancelMsg:
		next, cmd := m.handleBranchNavigationCancel(msg)
		return next, cmd, true

	case localEntriesMsg:
		next, cmd := m.handleLocalEntries(msg)
		return next, cmd, true

	case memorySearchMsg:
		next, cmd := m.handleMemorySearch(msg)
		return next, cmd, true

	case memoryAuditMsg:
		next, cmd := m.handleMemoryAudit(msg)
		return next, cmd, true

	case memoryActionMsg:
		next, cmd := m.handleMemoryAction(msg)
		return next, cmd, true

	case checkpointListMsg:
		next, cmd := m.handleCheckpointList(msg)
		return next, cmd, true

	case checkpointPlanMsg:
		next, cmd := m.handleCheckpointPlan(msg)
		return next, cmd, true

	case checkpointRestoredMsg:
		next, cmd := m.handleCheckpointRestored(msg)
		return next, cmd, true

	case terminalCommitLinesMsg:
		if msg.generation != m.Model.EventGeneration || msg.epoch != m.Model.terminalCommitEpoch {
			return m, nil, true
		}
		m.acceptPrintedEntries(msg.entryKeys)
		return m, terminalCommitFlushAndSignalCmd(msg.lines, terminalCommitPrintedMsg{
			generation: msg.generation,
			epoch:      msg.epoch,
			entryKeys:  append([]string(nil), msg.entryKeys...),
		}), true

	case terminalCommitPrintedMsg:
		if msg.generation != m.Model.EventGeneration || msg.epoch != m.Model.terminalCommitEpoch {
			return m, nil, true
		}
		m.clearPrintedSubmittedEntry(msg.entryKeys)
		return m, nil, true

	case tea.ResumeMsg:
		// Resume from suspend - no special handling needed
		return m, nil, true
	}

	return m, nil, false
}

func (m Model) dispatchRuntimeControllerMessage(msg tea.Msg) (Model, tea.Cmd, bool) {
	switch msg := msg.(type) {
	case runtimeSwitchedMsg:
		next, cmd := m.handleRuntimeSwitched(msg)
		return next, cmd, true

	case reloadConfigLoadedMsg:
		next, cmd := m.handleReloadConfigLoaded(msg)
		return next, cmd, true

	case TransitionCommittedMsg:
		next, cmd := m.handleRuntimeTransitionCommitted(msg)
		return next, cmd, true

	case thinkingRuntimeAppliedMsg:
		next, cmd := m.handleThinkingRuntimeApplied(msg)
		return next, cmd, true

	case runtimeSwitchErrorMsg:
		next, cmd := m.handleRuntimeSwitchError(msg)
		return next, cmd, true

	case resumeSessionSelectedMsg:
		next, cmd := m.handleResumeSessionSelected(msg)
		return next, cmd, true

	case scopedModelsLoadedMsg:
		next, cmd := m.handleScopedModelsLoaded(msg)
		return next, cmd, true

	case scopedModelsListedMsg:
		next, cmd := m.handleScopedModelsListed(msg)
		return next, cmd, true

	case modelPickerSetupResolvedMsg:
		next, cmd := m.handleModelPickerSetupResolved(msg)
		return next, cmd, true

	case providerSetupResolvedMsg:
		next, cmd := m.handleProviderSetupResolved(msg)
		return next, cmd, true

	case providerItemsLoadedMsg:
		next, cmd := m.handleProviderItemsLoaded(msg)
		return next, cmd, true

	case setupPromptSavedMsg:
		next, cmd := m.handleSetupPromptSaved(msg)
		return next, cmd, true

	case logoutProviderSavedMsg:
		next, cmd := m.handleLogoutProviderSaved(msg)
		return next, cmd, true

	case changelogLoadedMsg:
		next, cmd := m.handleChangelogLoaded(msg)
		return next, cmd, true

	case skillsNoticeLoadedMsg:
		next, cmd := m.handleSkillsNoticeLoaded(msg)
		return next, cmd, true

	case skillDetailLoadedMsg:
		next, cmd := m.handleSkillDetailLoaded(msg)
		return next, cmd, true

	case settingsCommandMsg:
		next, cmd := m.handleSettingsCommandResult(msg)
		return next, cmd, true

	case actionReconciledMsg:
		next, cmd := m.handleActionReconciled(msg)
		return next, cmd, true

	case interruptedTurnAbortedMsg:
		next, cmd := m.handleInterruptedTurnAborted(msg)
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

	case sessionForkedMsg:
		next, cmd := m.handleSessionForked(msg)
		return next, cmd, true

	case treePickerLoadedMsg:
		next, cmd := m.handleTreePickerLoaded(msg)
		return next, cmd, true

	case treePickerMoveMsg:
		next, cmd := m.handleTreePickerMove(msg)
		return next, cmd, true

	case replayBranchMsg:
		next, cmd := m.handleReplayBranch(msg)
		return next, cmd, true

	case sessionExportedMsg:
		next, cmd := m.handleSessionExported(msg)
		return next, cmd, true

	case sessionSharedMsg:
		next, cmd := m.handleSessionShared(msg)
		return next, cmd, true

	case sessionImportedMsg:
		next, cmd := m.handleSessionImported(msg)
		return next, cmd, true

	case sessionSearchResultsMsg:
		next, cmd := m.handleSessionSearchResults(msg)
		return next, cmd, true

	case sessionNamedMsg:
		next, cmd := m.handleSessionNamed(msg)
		return next, cmd, true

	case sessionClonedMsg:
		next, cmd := m.handleSessionCloned(msg)
		return next, cmd, true

	case labelShowMsg:
		next, cmd := m.handleLabelShow(msg)
		return next, cmd, true

	case undoResultMsg:
		next, cmd := m.handleUndoResult(msg)
		return next, cmd, true

	case diffResultMsg:
		next, cmd := m.handleDiffResult(msg)
		return next, cmd, true

	case userMessagesLoadedMsg:
		next, cmd := m.handleUserMessagesLoaded(msg)
		return next, cmd, true

	case oauthLoginFinishedMsg:
		next, cmd := m.handleOAuthLoginFinished(msg)
		return next, cmd, true
	}

	return m, nil, false
}

func (m Model) dispatchTurnControllerMessage(msg tea.Msg) (Model, tea.Cmd, bool) {
	switch msg := msg.(type) {
	case runtimeSubscriptionMsg:
		if msg.generation != m.Model.EventGeneration {
			if msg.subscription != nil {
				msg.subscription.Close()
			}
			return m, nil, true
		}
		if m.Picker.BranchSummary != nil && m.Picker.BranchSummary.navigating {
			if state := m.Model.EventSubscriptionState; state != nil && state.generation == msg.generation {
				state.pending = false
				state.retryAfterNavigation = true
			}
			if msg.subscription != nil {
				msg.subscription.Close()
			}
			return m, nil, true
		}
		if msg.treeNavigationRequest != m.Model.TreeNavigationRequest {
			if state := m.Model.EventSubscriptionState; state != nil && state.generation == msg.generation {
				state.pending = false
				if m.Picker.BranchSummary != nil && m.Picker.BranchSummary.navigating {
					state.retryAfterNavigation = true
				}
			}
			if msg.subscription != nil {
				msg.subscription.Close()
			}
			if m.Picker.BranchSummary != nil && m.Picker.BranchSummary.navigating {
				return m, nil, true
			}
			return m, m.awaitSessionEvent(), true
		}
		if state := m.Model.EventSubscriptionState; state != nil && state.generation == msg.generation {
			state.pending = false
		}
		if msg.err != nil {
			if errors.Is(msg.err, agent.ErrSnapshotChanged) {
				return m, m.awaitSessionEvent(), true
			}
			if errors.Is(msg.err, agent.ErrRuntimeClosed) {
				next, cmd := m.handleStreamClosed(msg.err)
				return next, cmd, true
			}
			next, cmd := m.handleSessionError(msg.err, false)
			return next, cmd, true
		}
		if msg.subscription == nil {
			next, cmd := m.handleSessionError(errors.New("runtime returned an empty event subscription"), false)
			return next, cmd, true
		}
		if m.Model.EventSubscription != nil {
			m.Model.EventSubscription.Close()
		}
		m.Model.EventSubscription = msg.subscription
		m.Model.EventCursor = msg.subscription.Snapshot.Cursor
		// The initial subscription can be established after the runtime has
		// already entered an approval wait. Reconcile every authoritative
		// snapshot; only a cursor-based recovery needs transcript replay.
		snapshot := msg.subscription.Snapshot
		replayBranch := snapshot.Branch
		syntheticAssistant := snapshot.Resynced && snapshot.ActiveTurn.Assistant != nil &&
			snapshot.ActiveTurn.AssistantCommitted && !snapshot.ActiveTurn.AssistantInBranch
		if syntheticAssistant {
			replayBranch = append(append([]session.Entry(nil), replayBranch...), syntheticAssistantEntry(snapshot))
		}
		m.applyAgentRuntimeSnapshot(snapshot)
		if syntheticAssistant && snapshot.ActiveTurn.AssistantCommitted {
			// The synthetic entry is already rendered by SwitchReplay. Keep the
			// semantic assistant for error inspection, but do not print it again
			// when the missed TurnEnd is received.
			m.InFlight.SuppressAssistantPrint = true
		}
		var snapshotCmd tea.Cmd
		if snapshot.Resynced {
			snapshotCmd = m.terminalCommit().SwitchReplay(
				nil,
				replayBranch,
				"Runtime resynchronized from the session snapshot.",
				"",
			)
		}
		return m, sequenceCmds(snapshotCmd, m.awaitSessionEvent()), true

	case sessionEventMsg:
		if msg.generation != m.Model.EventGeneration {
			return m, nil, true
		}
		if state := m.Model.EventSubscriptionState; state != nil && state.generation == msg.generation {
			if msg.reader != 0 && msg.reader != state.reader {
				return m, nil, true
			}
			state.readerBusy = false
		}
		if msg.cursor != m.Model.EventCursor {
			if m.Model.EventSubscription != nil {
				m.Model.EventSubscription.Close()
				m.Model.EventSubscription = nil
			}
			return m, m.awaitSessionEvent(), true
		}
		m.Model.EventCursor.Next++
		next, cmd := m.handleSessionEvent(msg.event)
		return next, cmd, true

	case streamClosedMsg:
		if msg.generation != m.Model.EventGeneration {
			return m, nil, true
		}
		if state := m.Model.EventSubscriptionState; state != nil && state.generation == msg.generation {
			state.readerBusy = false
		}
		if errors.Is(msg.err, agent.ErrSubscriptionLagged) {
			m.Model.EventSubscription = nil
			return m, m.awaitSessionEvent(), true
		}
		if m.Model.EventSubscription != nil {
			m.Model.EventSubscription.Close()
			m.Model.EventSubscription = nil
		}
		next, cmd := m.handleStreamClosed(msg.err)
		return next, cmd, true

	case busyInputResultMsg:
		next, cmd := m.handleBusyInputResult(msg)
		return next, cmd, true

	case turnSubmitResultMsg:
		next, cmd := m.handleTurnSubmitResult(msg)
		return next, cmd, true

	case turnCancelResultMsg:
		next, cmd := m.handleTurnCancelResult(msg)
		return next, cmd, true

	// Raw session events (for unit tests that don't wrap in sessionEventMsg).
	case session.TurnStart,
		session.TurnEnd,
		session.AgentStart,
		session.AgentEnd,
		session.ModelUpdate,
		session.ThinkingUpdate,
		session.ToolsUpdate,
		session.MessageUpdate,
		session.MessageEnd,
		session.MessageStart,
		session.ToolExecStart,
		session.ToolExecUpdate,
		session.ToolExecEnd,
		session.ApprovalRequest,
		session.ApprovalResolution,
		session.QueueUpdate,
		session.Settled,
		session.RuntimeReady,
		session.Abort,
		session.SavePoint,
		session.ProviderRetry,
		*session.Error:
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
		if m.Picker.BranchSummary != nil {
			next, cmd := m.handleBranchSummaryPaste(msg)
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
