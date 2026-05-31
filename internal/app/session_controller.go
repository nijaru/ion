package app

import (
	"context"
	"errors"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/privacy"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

type localErrorMsg struct {
	err error
}

// Product session control owns the Ion side of the active-turn lifecycle.
// Key handlers and renderers should delegate here instead of making their own
// submit, cancel, queue, or settlement decisions.
func (m Model) submitComposer() (Model, tea.Cmd) {
	m.clearPendingAction()
	text := strings.TrimSpace(m.Input.Composer.Value())
	if text == "" {
		return m, nil
	}
	if m.Model.RuntimeSwitchRequest != 0 {
		return m, cmdError("Wait for the runtime switch to finish before sending input.")
	}
	if strings.HasPrefix(text, "/") {
		return m.submitText(text)
	}
	if m.localCommandBusy() {
		return m.submitBusyInput(text)
	}

	return m.submitText(text)
}

func (m Model) submitText(text string) (Model, tea.Cmd) {
	// Expand any paste marker placeholders to their original content.
	draft := text
	text = m.expandMarkers(text)

	if !strings.HasPrefix(text, "/") {
		if status := m.configurationStatus(); status != "" {
			return m, cmdError(status)
		}
		if reason := m.configuredSessionBudgetStopReason(); reason != "" {
			return m, cmdError(reason)
		}
	}

	if strings.HasPrefix(text, "/") {
		historyText, historyChanged := m.appendInputHistory(text)
		var historyCmd tea.Cmd
		if historyChanged {
			historyCmd = m.persistInputHistory(context.Background(), historyText)
		}
		m.resetComposerDraft()
		m, cmd := m.handleCommand(text)
		return m, sequenceCmds(cmd, historyCmd)
	}

	m.turnReducer().startSubmit()
	m.resetComposerDraft()
	return m, submitTurnCmd(m.Model.Session, text, draft)
}

func submitTurnCmd(sess session.AgentSession, text, draft string) tea.Cmd {
	return func() tea.Msg {
		if sess == nil {
			return turnSubmitResultMsg{
				text:  text,
				draft: draft,
				err:   errors.New("session unavailable"),
			}
		}
		if err := sess.SubmitTurn(context.Background(), text); err != nil {
			return turnSubmitResultMsg{text: text, draft: draft, err: err}
		}
		return turnSubmitResultMsg{text: text, draft: draft}
	}
}

func (m Model) handleTurnSubmitResult(msg turnSubmitResultMsg) (Model, tea.Cmd) {
	m.refreshRuntimeSessionSnapshot()
	if msg.err == nil {
		historyText, historyChanged := m.appendInputHistory(msg.text)
		var historyCmd tea.Cmd
		if historyChanged {
			historyCmd = m.persistInputHistory(context.Background(), historyText)
		}
		routingCmd := m.persistEntryCmd(
			"persist routing decision",
			m.routingDecision("use_model", "active_preset", ""),
		)
		if msg.rearm {
			return m, sequenceCmds(routingCmd, historyCmd, m.awaitSessionEvent())
		}
		return m, sequenceCmds(routingCmd, historyCmd)
	}
	m.turnReducer().rejectSubmit()
	var draftCmd tea.Cmd
	if strings.TrimSpace(m.Input.Composer.Value()) == "" {
		draftCmd = m.setComposerDraft(msg.draft)
	}
	return m, tea.Batch(draftCmd, cmdError(session.DisplayError(msg.err)))
}

func (m Model) handleQueuedTurn(msg queuedTurnMsg) (Model, tea.Cmd) {
	next, cmd := m.submitText(msg.text)
	if !msg.rearmSessionEvents {
		return next, cmd
	}
	if next.InFlight.Thinking {
		if cmd == nil {
			return next, next.awaitSessionEvent()
		}
		return next, rearmSubmitResultCmd(cmd)
	}
	return next, sequenceCmds(cmd, next.awaitSessionEvent())
}

func rearmSubmitResultCmd(submitCmd tea.Cmd) tea.Cmd {
	return func() tea.Msg {
		msg := submitCmd()
		if result, ok := msg.(turnSubmitResultMsg); ok {
			result.rearm = true
			return result
		}
		return msg
	}
}

func (m Model) submitBusyInput(text string) (Model, tea.Cmd) {
	mode := ""
	if m.Model.Config != nil {
		mode = m.Model.Config.BusyInputMode()
	}
	steering, supportsSteering := m.Model.Session.(session.SteeringSession)
	queued, supportsFollowUp := m.Model.Session.(session.QueuedInputSession)

	switch session.RouteBusyInput(session.BusyInputRouting{
		Mode:             mode,
		Thinking:         m.InFlight.Thinking,
		Compacting:       m.Progress.Compacting,
		SupportsSteering: supportsSteering,
		SupportsFollowUp: supportsFollowUp,
	}) {
	case session.BusyInputRouteSteer:
		m.resetComposerDraft()
		return m, steerTurnCmd(steering, text)
	case session.BusyInputRouteFollowUp:
		priorFollowUpCount := len(m.InFlight.QueuedTurns)
		m.resetComposerDraft()
		return m, followUpTurnCmd(queued, text, priorFollowUpCount)
	default:
		return m.queueBusyInputLocal(text)
	}
}

func (m Model) queueBusyInput(text string) (Model, tea.Cmd) {
	if m.InFlight.Thinking && !m.Progress.Compacting {
		if queued, ok := m.Model.Session.(session.QueuedInputSession); ok {
			priorFollowUpCount := len(m.InFlight.QueuedTurns)
			m.resetComposerDraft()
			return m, followUpTurnCmd(queued, text, priorFollowUpCount)
		}
	}

	return m.queueBusyInputLocal(text)
}

func steerTurnCmd(steering session.SteeringSession, text string) tea.Cmd {
	return func() tea.Msg {
		result, err := steering.SteerTurn(context.Background(), text)
		return steeringResultMsg{text: text, result: result, err: err}
	}
}

func (m Model) handleSteeringResult(msg steeringResultMsg) (Model, tea.Cmd) {
	if msg.err == nil && msg.result.Outcome == session.SteeringAccepted {
		entry, _ := storage.EntrySystem("Steering current turn", time.Time{})
		return m, m.terminalCommit().Entries(entry)
	}
	return m.queueBusyInput(msg.text)
}

func followUpTurnCmd(
	queued session.QueuedInputSession,
	text string,
	priorFollowUpCount int,
) tea.Cmd {
	return func() tea.Msg {
		result, err := queued.FollowUpTurn(context.Background(), text)
		return followUpResultMsg{
			text:               text,
			priorFollowUpCount: priorFollowUpCount,
			result:             result,
			err:                err,
		}
	}
}

func (m Model) handleFollowUpResult(msg followUpResultMsg) (Model, tea.Cmd) {
	if msg.err == nil && msg.result.Outcome == session.QueuedInputAccepted {
		queued := append([]string(nil), m.InFlight.QueuedTurns...)
		if len(queued) <= msg.priorFollowUpCount {
			queued = append(queued, msg.text)
		}
		m.turnReducer().setBackendQueuedInput(m.InFlight.QueuedSteering, queued)
		entry, _ := storage.EntrySystem("Queued follow-up", time.Time{})
		return m, m.terminalCommit().Entries(entry)
	}
	return m.queueBusyInputLocal(msg.text)
}

func (m Model) queueBusyInputLocal(text string) (Model, tea.Cmd) {
	m.turnReducer().queueTurn(text)
	m.resetComposerDraft()
	entry, _ := storage.EntrySystem("Queued follow-up", time.Time{})
	return m, m.terminalCommit().Entries(entry)
}

func (m Model) recallQueuedTurns() (Model, tea.Cmd) {
	backendOwned := m.InFlight.QueuedTurnsBackendOwned
	queued := m.turnReducer().drainQueuedTurnsText()
	if queued == "" {
		return m, nil
	}
	current := strings.TrimSpace(m.Input.Composer.Value())
	if current != "" {
		queued = current + "\n" + queued
	}
	setDraft := m.setComposerDraft(queued)
	if backendOwned {
		if queuedInput, ok := m.Model.Session.(session.QueuedInputSession); ok {
			return m, tea.Sequence(clearQueuedInputCmd(queuedInput), setDraft)
		}
	}
	return m, setDraft
}

func clearQueuedInputCmd(queued session.QueuedInputSession) tea.Cmd {
	return func() tea.Msg {
		if _, err := queued.ClearQueuedInput(context.Background()); err != nil {
			return queuedInputClearResultMsg{err: err}
		}
		return queuedInputClearResultMsg{}
	}
}

func (m Model) cancelRunningTurn(reason string) (Model, tea.Cmd) {
	m.turnReducer().cancelActiveTurn()
	entry, _ := storage.EntrySystem(reason, time.Time{})
	return m, sequenceCmds(
		m.terminalCommit().Entries(entry),
		m.persistEntryCmd("persist cancellation", storage.System{
			Type:    "system",
			Content: entry.Content,
			TS:      now(),
		}),
		cancelTurnCmd(m.Model.Session),
	)
}

func cancelTurnCmd(sess session.AgentSession) tea.Cmd {
	return func() tea.Msg {
		if sess == nil {
			return turnCancelResultMsg{err: errors.New("session unavailable")}
		}
		if err := sess.CancelTurn(context.Background()); err != nil {
			return turnCancelResultMsg{err: err}
		}
		return turnCancelResultMsg{}
	}
}

func (m Model) handleTurnCancelResult(msg turnCancelResultMsg) (Model, tea.Cmd) {
	if msg.err != nil {
		return m, persistErrorCmd("cancel turn", msg.err)
	}
	return m, nil
}

func (m Model) handleDeferredEnter() (Model, tea.Cmd) {
	if !m.Input.DeferredEnter {
		return m, nil
	}
	if m.printHoldActive() {
		return m, m.scheduleDeferredEnter()
	}
	m.inputReducer().finishDeferredEnter()
	return m.submitComposer()
}

func (m Model) awaitSessionEvent() tea.Cmd {
	generation := m.Model.EventGeneration
	if m.Model.Session == nil {
		return func() tea.Msg {
			return sessionEventMsg{
				generation: generation,
				event: session.Error{
					Base:  session.BaseNow(),
					Err:   errors.New("session unavailable"),
					Fatal: true,
				},
			}
		}
	}
	events := m.Model.Session.Events()
	if events == nil {
		return func() tea.Msg {
			return sessionEventMsg{
				generation: generation,
				event: session.Error{
					Base:  session.BaseNow(),
					Err:   errors.New("session event stream unavailable"),
					Fatal: true,
				},
			}
		}
	}
	return func() tea.Msg {
		ev, ok := <-events
		if !ok {
			return streamClosedMsg{generation: generation}
		}
		return sessionEventMsg{generation: generation, event: ev}
	}
}

// handleSessionEvent processes events from the agent session channel.
func (m Model) handleSessionEvent(ev session.Event) (Model, tea.Cmd) {
	turn := m.turnReducer()
	if turn.drainingUntilTurnStarted() {
		switch msg := ev.(type) {
		case session.UserMessage:
			if turn.shouldDrainLateEvent(msg.Timestamp) {
				return m, m.awaitSessionEvent()
			}
			turn.finishDrain()
		case session.TurnStarted:
			if turn.shouldDrainLateEvent(msg.Timestamp) {
				return m, m.awaitSessionEvent()
			}
			turn.finishDrain()
		case session.TurnFinished:
			turn.finishDrain()
		default:
			return m, m.awaitSessionEvent()
		}
	}

	switch msg := ev.(type) {
	case session.StatusChanged:
		return m.handleStatusChanged(msg)

	case session.TokenUsage:
		return m.handleTokenUsage(msg)

	case session.QueuedInputUpdated:
		return m.handleQueuedInputUpdated(msg)

	case session.TurnStarted:
		return m.handleTurnStarted(msg)

	case session.TurnSavePoint:
		return m, m.awaitSessionEvent()

	case session.TurnFinished:
		return m.handleTurnFinished()

	case session.ThinkingDelta:
		return m.handleThinkingDelta(msg)

	case session.UserMessage:
		return m.handleUserMessage(msg)

	case session.AgentDelta:
		return m.handleAgentDelta(msg)

	case session.AgentMessage:
		return m.handleAgentMessage(msg)

	case session.ToolCallStarted:
		return m.handleToolCallStarted(msg)

	case session.ToolOutputDelta:
		return m.handleToolOutputDelta(msg)

	case session.ToolResult:
		return m.handleToolResult(msg)

	case session.VerificationResult:
		return m, m.awaitSessionEvent()

	case session.ChildRequested:
		return m.handleChildRequested(msg)

	case session.ChildStarted:
		return m.handleChildStarted(msg)

	case session.ChildDelta:
		return m.handleChildDelta(msg)

	case session.ChildCompleted:
		return m.handleChildCompleted(msg)

	case session.ChildBlocked:
		return m.handleChildBlocked(msg)

	case session.ChildFailed:
		return m.handleChildFailed(msg)

	case session.ChildCanceled:
		return m.handleChildCanceled(msg)

	case session.Error:
		return m.handleSessionError(msg.Err, true)
	}

	return m, m.awaitSessionEvent()
}

func (m Model) handleUserMessage(msg session.UserMessage) (Model, tea.Cmd) {
	entry, ok := storage.EntryUser(msg.Message, msg.Timestamp)
	if !ok {
		return m, m.awaitSessionEvent()
	}
	return m, tea.Sequence(m.terminalCommit().Entries(entry), m.awaitSessionEvent())
}

func (m Model) handleStreamClosed() (Model, tea.Cmd) {
	entry, ok := m.turnReducer().streamClosed(time.Now())
	if !ok {
		return m, nil
	}
	var cmds []tea.Cmd
	cmds = append(cmds, m.terminalCommit().Entries(entry))
	cmds = append(cmds, m.persistEntryCmd("persist stream close error", storage.System{
		Type:    "system",
		Content: entry.Content,
		TS:      now(),
	}))
	return m, sequenceCmds(cmds...)
}

func (m Model) handleSessionError(err error, awaitTerminal bool) (Model, tea.Cmd) {
	decision := session.DecideErrorSettlement(session.ErrorSettlementInput{
		Err:           err,
		AwaitTerminal: awaitTerminal,
	})
	var cmds []tea.Cmd
	if decision.RoutingStop != nil {
		cmds = append(
			cmds,
			m.persistEntryCmd(
				"persist routing stop",
				m.routingDecision(
					"stop",
					decision.RoutingStop.Reason,
					decision.RoutingStop.StopReason,
				),
			),
		)
	}
	m.turnReducer().failTurn(decision.DisplayError, time.Now())
	entry, _ := storage.EntrySystem(decision.EntryContent, time.Time{})
	printErr := m.terminalCommit().Entries(entry)
	cmds = append([]tea.Cmd{printErr}, cmds...)
	if decision.PersistSystem {
		cmds = append(cmds, m.persistEntryCmd("persist session error", storage.System{
			Type:    "system",
			Content: entry.Content,
			TS:      now(),
		}))
	}
	if !decision.AwaitNext {
		return m, sequenceCmds(cmds...)
	}
	cmds = append(cmds, m.awaitSessionEvent())
	return m, sequenceCmds(cmds...)
}

func (m Model) handleLocalError(err error) (Model, tea.Cmd) {
	m.turnReducer().clearLocalErrorIfIdle()
	if !m.InFlight.Thinking {
		m.progressReducer().clearLocalBusyStatus()
	}
	entry, _ := storage.EntrySystem("Error: "+err.Error(), time.Time{})
	return m, m.terminalCommit().Entries(entry)
}

func isLocalBusyStatus(status string) bool {
	trimmed := strings.TrimSpace(status)
	return trimmed == "Switching runtime..." ||
		trimmed == "Saving runtime settings..." ||
		trimmed == "Loading session..." ||
		trimmed == "Checking provider..." ||
		trimmed == "Saving provider setup..." ||
		trimmed == "Loading settings..." ||
		trimmed == "Saving settings..." ||
		isCompactingStatus(trimmed)
}

func (m Model) handleStatusChanged(msg session.StatusChanged) (Model, tea.Cmd) {
	decision := m.turnReducer().applyStatusChanged(msg)
	persistTimestamp := msg.Timestamp
	if decision.Root {
		persistTimestamp = decision.PersistTimestamp
	}
	return m, sequenceCmds(m.persistEntryCmd("persist status", storage.Status{
		Type:   "status",
		Status: msg.Status,
		TS:     entryUnix(persistTimestamp),
	}), m.awaitSessionEvent())
}

func (m Model) handleQueuedInputUpdated(msg session.QueuedInputUpdated) (Model, tea.Cmd) {
	m.turnReducer().setBackendQueuedInput(msg.Snapshot.Steering, msg.Snapshot.FollowUp)
	return m, m.awaitSessionEvent()
}

func (m Model) handleTokenUsage(msg session.TokenUsage) (Model, tea.Cmd) {
	m.turnReducer().applyTokenUsage(msg)
	cmds := []tea.Cmd{m.persistEntryCmd("persist token usage", storage.TokenUsage{
		Type:   "token_usage",
		Input:  msg.Input,
		Output: msg.Output,
		Cost:   msg.Cost,
		TS:     entryUnix(msg.Timestamp),
	})}
	if reason := m.configuredBudgetStopReason(); reason != "" &&
		reason != m.Progress.BudgetStopReason {
		entry, _ := m.turnReducer().applyBudgetStop(reason, msg.Timestamp)
		cmds = append(
			cmds,
			m.persistEntryCmd(
				"persist routing stop",
				m.routingDecision("stop", "budget_limit", reason),
			),
		)
		if entry.Content != "" {
			cmds = append(cmds, m.persistEntryCmd("persist budget cancellation", storage.System{
				Type:    "system",
				Content: entry.Content,
				TS:      entryUnix(msg.Timestamp),
			}))
			cmds = append([]tea.Cmd{
				tea.Batch(
					m.terminalCommit().Entries(entry),
					cancelTurnCmd(m.Model.Session),
				),
			}, cmds...)
			cmds = append(cmds, m.awaitSessionEvent())
			return m, sequenceCmds(cmds...)
		}
	}
	cmds = append(cmds, m.awaitSessionEvent())
	return m, sequenceCmds(cmds...)
}

func (m Model) handleTurnStarted(msg session.TurnStarted) (Model, tea.Cmd) {
	m.turnReducer().startTurn(msg.Timestamp, time.Now())
	return m, m.awaitSessionEvent()
}

func (m Model) handleTurnFinished() (Model, tea.Cmd) {
	m.turnReducer().stopThinking()
	var cmds []tea.Cmd

	assistant, assistantCompleted, printAssistant := m.turnReducer().finishPendingAssistant()
	if printAssistant {
		cmds = append(cmds, m.terminalCommit().Entries(assistant))
	}
	if entry, ok := m.turnReducer().finishTurnMode(assistantCompleted); ok {
		cmds = append(cmds, m.terminalCommit().Entries(entry))
	}
	m.turnReducer().recordFinishedTurnSummary(time.Now())

	if queued := m.turnReducer().popQueuedTurn(); queued != "" {
		cmds = append(cmds, func() tea.Msg {
			return queuedTurnMsg{text: queued, rearmSessionEvents: true}
		})
		return m, tea.Sequence(cmds...)
	}
	cmds = append(cmds, loadGitDiffStats(m.App.Workdir))
	cmds = append(cmds, m.awaitSessionEvent())
	return m, tea.Sequence(cmds...)
}

func (m Model) handleThinkingDelta(msg session.ThinkingDelta) (Model, tea.Cmd) {
	m.turnReducer().appendThinkingDelta(msg.AgentID, msg.Delta)
	return m, m.awaitSessionEvent()
}

func (m Model) handleAgentDelta(msg session.AgentDelta) (Model, tea.Cmd) {
	m.turnReducer().appendAgentDelta(msg.AgentID, msg.Delta, msg.Timestamp)
	return m, m.awaitSessionEvent()
}

func (m Model) handleAgentMessage(msg session.AgentMessage) (Model, tea.Cmd) {
	if msg.AgentID != "" {
		return m.handleSubagentMessage(msg)
	}
	if entry, ok := m.turnReducer().commitAgentMessage(msg); ok {
		return m, tea.Sequence(m.terminalCommit().Entries(entry), m.awaitSessionEvent())
	}
	return m, m.awaitSessionEvent()
}

func (m Model) handleToolCallStarted(msg session.ToolCallStarted) (Model, tea.Cmd) {
	m.turnReducer().startToolCall(
		msg.ToolUseID,
		msg.Timestamp,
		privacy.Redact(m.formatToolTitle(msg.ToolName, msg.Args)),
	)
	return m, m.awaitSessionEvent()
}

func (m Model) handleToolOutputDelta(msg session.ToolOutputDelta) (Model, tea.Cmd) {
	m.turnReducer().appendToolOutput(msg.ToolUseID, msg.Delta, msg.Snapshot)
	return m, m.awaitSessionEvent()
}

func (m Model) handleToolResult(msg session.ToolResult) (Model, tea.Cmd) {
	toolUseID := msg.ToolUseID
	if toolUseID == "" {
		toolUseID = m.Progress.LastToolUseID
	}
	if entry, ok := m.turnReducer().completeToolResult(toolUseID, msg); ok {
		return m, tea.Sequence(m.terminalCommit().Entries(entry), m.awaitSessionEvent())
	}
	return m, m.awaitSessionEvent()
}

func tokenUsageTotal(msg session.TokenUsage) int {
	if msg.Total > 0 {
		return msg.Total
	}
	return msg.Input + msg.Output
}
