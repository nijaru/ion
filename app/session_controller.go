package app

import (
	"context"
	"errors"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/session"
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
		if decision := m.submitPreflight(); !decision.Allowed {
			return m, cmdError(decision.Reason)
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

	m.turnReducer().StartSubmit()
	m.resetComposerDraft()
	return m, submitTurnCmd(m.Model.Session, m.Model.Runner, text, draft)
}

func submitTurnCmd(sess session.Session, runner agent.Runner, text, draft string) tea.Cmd {
	return func() tea.Msg {
		if runner != nil {
			// Use the Runner (Harness) for turn execution.
			// Prompt blocks until the turn completes and emits events.
			_, err := runner.Prompt(context.Background(), text)
			return turnSubmitResultMsg{text: text, draft: draft, err: err}
		}
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
		if msg.rearm || m.InFlight.Thinking {
			return m, batchCmds(routingCmd, historyCmd, m.awaitSessionEvent())
		}
		return m, sequenceCmds(routingCmd, historyCmd)
	}
	m.turnReducer().RejectSubmit("")
	var draftCmd tea.Cmd
	if strings.TrimSpace(m.Input.Composer.Value()) == "" {
		draftCmd = m.setComposerDraft(msg.draft)
	}
	return m, tea.Batch(draftCmd, cmdError(msg.err.Error()))
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

// queueFollowUp queues a follow-up message when Alt+Enter is pressed while the agent is streaming.
func (m Model) queueFollowUp() (Model, tea.Cmd) {
	text := strings.TrimSpace(m.Input.Composer.Value())
	if text == "" {
		return m, nil
	}
	return m.queueBusyInput(text)
}

func steerTurnCmd(steering session.SteeringSession, text string) tea.Cmd {
	return func() tea.Msg {
		result, err := steering.SteerTurn(context.Background(), text)
		return steeringResultMsg{text: text, result: result, err: err}
	}
}

func (m Model) handleSteeringResult(msg steeringResultMsg) (Model, tea.Cmd) {
	decision := session.DecideSteeringResult(msg.result, msg.err)
	if decision.Action == session.BusyInputResultAccepted {
		entry, _ := session.EntrySystem(decision.NoticeContent, time.Time{})
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
	decision := session.DecideFollowUpResult(session.FollowUpResultInput{
		Text:               msg.text,
		PriorFollowUpCount: msg.priorFollowUpCount,
		CurrentFollowUp:    m.InFlight.QueuedTurns,
		Result:             msg.result,
		Err:                msg.err,
	})
	if decision.Action == session.BusyInputResultAccepted {
		m.turnReducer().SetBackendQueuedInput(m.InFlight.QueuedSteering, decision.FollowUp)
		entry, _ := session.EntrySystem(decision.NoticeContent, time.Time{})
		return m, m.terminalCommit().Entries(entry)
	}
	return m.queueBusyInputLocal(msg.text)
}

func (m Model) queueBusyInputLocal(text string) (Model, tea.Cmd) {
	m.turnReducer().QueueTurn(text)
	m.resetComposerDraft()
	entry, _ := session.EntrySystem("Queued follow-up", time.Time{})
	return m, m.terminalCommit().Entries(entry)
}

func (m Model) recallQueuedTurns() (Model, tea.Cmd) {
	decision := session.DecideQueuedInputRecall(session.QueuedInputRecallInput{
		CurrentDraft: m.Input.Composer.Value(),
		Steering:     m.InFlight.QueuedSteering,
		FollowUp:     m.InFlight.QueuedTurns,
		BackendOwned: m.InFlight.QueuedTurnsBackendOwned,
	})
	if !decision.Recall {
		return m, nil
	}
	m.turnReducer().ClearQueuedTurns()
	setDraft := m.setComposerDraft(decision.ComposerText)
	if decision.ClearBackend {
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
	decision := m.turnReducer().CancelTurn(reason, time.Now())
	entry, _ := session.EntrySystem(decision.EntryContent, time.Time{})
	return m, batchCmds(
		m.terminalCommit().Entries(entry),
		m.persistEntryCmd("persist cancellation", session.StoreSystem{
			Type:    "system",
			Content: session.EntryText(entry),
			TS:      now(),
		}),
		cancelTurnCmd(m.Model.Session),
	)
}

func cancelTurnCmd(sess session.Session) tea.Cmd {
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
	var events <-chan session.Event
	if m.Model.Runner != nil {
		events = m.Model.Runner.Events()
	} else if m.Model.Session != nil {
		events = m.Model.Session.Events()
	}
	if events == nil {
		return func() tea.Msg {
			return sessionEventMsg{
				generation: generation,
				event: session.TurnEnd{
					Base:  session.BaseNow(),
					Error: errors.New("session event stream unavailable"),
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
	if turn.DrainingUntilTurnStarted() {
		decision := session.DecideEventDrain(session.EventDrainInput{
			Active:         m.InFlight.DrainUntilTurnStarted,
			DrainStartedAt: m.InFlight.DrainStartedAt,
			Event:          ev,
		})
		if decision.Action == session.EventDrainAwait {
			return m, m.awaitSessionEvent()
		}
		if decision.FinishDrain {
			turn.FinishDrain()
		}
	}

	switch msg := ev.(type) {
	case session.StatusChange:
		return m.handleStatusChanged(msg)

	case session.QueuedInputUpdate:
		return m.handleQueuedInputUpdated(msg)

	case session.TurnStart:
		return m.handleTurnStarted(msg)

	case session.TurnEnd:
		if msg.Error != nil {
			return m.handleSessionError(msg.Error, true)
		}
		return m.handleTurnFinished()

	case session.AgentStart:
		return m, m.awaitSessionEvent()

	case session.AgentEnd:
		return m, m.awaitSessionEvent()

	case session.MessageUpdate:
		return m.handleMessageUpdate(msg)

	case session.MessageEnd:
		return m.handleMessageEnd(msg)

	case session.UserMessage:
		return m.handleUserMessage(msg)

	case session.AgentMessage:
		return m.handleAgentMessage(msg)

	case session.ToolCallStart:
		return m.handleToolCallStarted(msg)

	case session.ToolExecutionUpdate:
		return m.handleToolExecutionUpdate(msg)

	case session.ToolCallEnd:
		return m.handleToolResult(msg)

	case session.ChildRequest:
		return m.handleChildRequested(msg)

	case session.ChildStart:
		return m.handleChildStarted(msg)

	case session.ChildDelta:
		return m.handleChildDelta(msg)

	case session.ChildComplete:
		return m.handleChildCompleted(msg)

	case session.ChildBlock:
		return m.handleChildBlocked(msg)

	case session.ChildFail:
		return m.handleChildFailed(msg)

	case session.ChildCancel:
		return m.handleChildCanceled(msg)
	}

	return m, m.awaitSessionEvent()
}

func (m Model) handleUserMessage(msg session.UserMessage) (Model, tea.Cmd) {
	entry, _ := session.EntryUser(msg.Content[0].(session.TextContent).Text, msg.When())
	return m, tea.Sequence(m.terminalCommit().Entries(entry), m.awaitSessionEvent())
}

func (m Model) handleStreamClosed() (Model, tea.Cmd) {
	entryIf, _ := m.turnReducer().StreamClosed(time.Now())
	var cmds []tea.Cmd
	cmds = append(cmds, m.terminalCommit().Entries(entryIf))
	cmds = append(cmds, m.persistEntryCmd("persist stream close error", session.StoreSystem{
		Type:    "system",
		Content: session.EntryText(entryIf),
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
	entry, _ := session.EntrySystem(decision.EntryContent, time.Time{})
	cmds = append(cmds, m.terminalCommit().Entries(entry))

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
	m.turnReducer().FailTurn(decision.DisplayError, time.Now())
	if decision.PersistSystem {
		cmds = append(cmds, m.persistEntryCmd("persist session error", session.StoreSystem{
			Type:    "system",
			Content: session.EntryText(entry),
			TS:      now(),
		}))
	}

	if decision.AwaitNext {
		cmds = append(cmds, m.awaitSessionEvent())
	}

	return m, batchCmds(cmds...)
}

func (m Model) handleLocalError(err error) (Model, tea.Cmd) {
	m.turnReducer().ClearLocalErrorIfIdle()
	if !m.InFlight.Thinking {
		m.progressReducer().clearLocalBusyStatus()
	}
	entry, _ := session.EntrySystem("Error: "+err.Error(), time.Time{})
	return m, m.terminalCommit().Entries(entry)
}

func (m Model) handleStatusChanged(msg session.StatusChange) (Model, tea.Cmd) {
	decision := m.turnReducer().ApplyStatusChangedInput(msg)
	persistTimestamp := msg.When()
	if decision.Root {
		persistTimestamp = decision.PersistTimestamp
	}
	return m, batchCmds(m.persistEntryCmd("persist status", session.StoreStatus{
		Type:   "status",
		Status: msg.Status,
		TS:     entryUnix(persistTimestamp),
	}), m.awaitSessionEvent())
}

func (m Model) handleQueuedInputUpdated(msg session.QueuedInputUpdate) (Model, tea.Cmd) {
	m.turnReducer().SetBackendQueuedInput(msg.Snapshot.Steering, msg.Snapshot.FollowUp)
	return m, m.awaitSessionEvent()
}

func (m Model) handleTurnStarted(msg session.TurnStart) (Model, tea.Cmd) {
	m.turnReducer().StartTurn(msg.When(), time.Now())
	return m, m.awaitSessionEvent()
}

func (m Model) handleTurnFinished() (Model, tea.Cmd) {
	m.turnReducer().StopThinking()
	var cmds []tea.Cmd

	assistant, assistantCompleted, printAssistant := m.turnReducer().FinishPendingAssistant()
	if printAssistant {
		cmds = append(cmds, m.terminalCommit().Entries(assistant))
	}
	if entry, ok := m.turnReducer().FinishTurnMode(assistantCompleted); ok {
		cmds = append(cmds, m.terminalCommit().Entries(entry))
	}
	m.turnReducer().RecordFinishedTurnSummary(time.Now())

	dispatch := m.turnReducer().FinishTurnDispatch()
	if dispatch.Action == session.TurnFinishedDispatchSubmitLocal {
		cmds = append(cmds, func() tea.Msg {
			return queuedTurnMsg{
				text:               dispatch.Text,
				rearmSessionEvents: dispatch.RearmSessionEvents,
			}
		})
		return m, tea.Sequence(cmds...)
	}
	if dispatch.ReloadGitDiff {
		cmds = append(cmds, loadGitDiffStats(m.App.Workdir))
	}
	if dispatch.AwaitNext {
		cmds = append(cmds, m.awaitSessionEvent())
	}
	return m, tea.Sequence(cmds...)
}

func (m Model) handleMessageEnd(msg session.MessageEnd) (Model, tea.Cmd) {
	// Token usage rides on MessageEnd (one per model call, including tool-use turns).
	m.turnReducer().ApplyTokenUsage(msg.Message)
	var cmds []tea.Cmd
	in, out, cost := session.TokenUsage(msg.Message)
	if in > 0 || out > 0 || cost > 0 {
		cmds = append(cmds, m.persistEntryCmd("persist token usage", session.StoreTokenUsage{
			Type:   "token_usage",
			Input:  in,
			Output: out,
			Cost:   cost,
			TS:     entryUnix(msg.When()),
		}))
	}
	cmds = append(cmds, m.awaitSessionEvent())
	return m, sequenceCmds(cmds...)
}

func (m Model) handleMessageUpdate(msg session.MessageUpdate) (Model, tea.Cmd) {
	// Route based on block_type (Pi model: single message_update event)
	switch msg.BlockType {
	case "thinking":
		m.turnReducer().AppendThinkingDelta(msg.AgentID, msg.Delta)
	default:
		m.turnReducer().AppendAgentDelta(msg.AgentID, msg.Delta, msg.When())
	}
	return m, m.awaitSessionEvent()
}

func (m Model) handleAgentMessage(msg session.AgentMessage) (Model, tea.Cmd) {
	if msg.AgentID != "" {
		return m.handleSubagentMessage(msg)
	}

	// Token usage tracking (was separate TokenUsage event, Pi: usage in message_end)
	m.turnReducer().ApplyTokenUsage(msg)
	var cmds []tea.Cmd
	if msg.InputTokens > 0 || msg.OutputTokens > 0 || msg.Cost > 0 {
		cmds = append(cmds, m.persistEntryCmd("persist token usage", session.StoreTokenUsage{
			Type:   "token_usage",
			Input:  msg.InputTokens,
			Output: msg.OutputTokens,
			Cost:   msg.Cost,
			TS:     entryUnix(msg.When()),
		}))
	}
	if reason := m.configuredBudgetStopReason(); reason != "" &&
		reason != m.Progress.BudgetStopReason {
		entry, _ := m.turnReducer().ApplyBudgetStop(reason, msg.When())
		cmds = append(
			cmds,
			m.persistEntryCmd(
				"persist routing stop",
				m.routingDecision("stop", "budget_limit", reason),
			),
		)
		if session.EntryText(entry) != "" {
			cmds = append(cmds, m.persistEntryCmd("persist budget cancellation", session.StoreSystem{
				Type:    "system",
				Content: session.EntryText(entry),
				TS:      entryUnix(msg.When()),
			}))
			cmds = append([]tea.Cmd{
				tea.Batch(
					m.terminalCommit().Entries(entry),
					cancelTurnCmd(m.Model.Session),
				),
			}, cmds...)
			return m, batchCmds(append(cmds, m.awaitSessionEvent())...)
		}
	}

	// Commit the message to the turn reducer
	if entry, ok := m.turnReducer().CommitAgentMessage(msg); ok {
		return m, tea.Sequence(m.terminalCommit().Entries(entry), m.awaitSessionEvent())
	}
	cmds = append(cmds, m.awaitSessionEvent())
	return m, batchCmds(cmds...)
}

func (m Model) handleToolCallStarted(msg session.ToolCallStart) (Model, tea.Cmd) {
	m.turnReducer().StartToolCall(
		msg.ToolUseID(),
		msg.When(),
		config.Redact(m.formatToolTitle(msg.ToolName(), msg.ArgsString())),
	)
	return m, m.awaitSessionEvent()
}

func (m Model) handleToolExecutionUpdate(msg session.ToolExecutionUpdate) (Model, tea.Cmd) {
	m.turnReducer().AppendToolOutput(msg.ToolUseID(), msg.PartialResult(), false)
	return m, m.awaitSessionEvent()
}

func (m Model) handleToolResult(msg session.ToolCallEnd) (Model, tea.Cmd) {
	toolUseID := msg.ToolUseID()
	if toolUseID == "" {
		toolUseID = m.Progress.LastToolUseID
	}
	if entry, ok := m.turnReducer().CompleteToolResult(toolUseID, msg); ok {
		return m, tea.Sequence(m.terminalCommit().Entries(entry), m.awaitSessionEvent())
	}
	return m, m.awaitSessionEvent()
}

func (m Model) handleChildRequested(msg session.ChildRequest) (Model, tea.Cmd) { return m, nil }
func (m Model) handleChildStarted(msg session.ChildStart) (Model, tea.Cmd) { return m, nil }
func (m Model) handleChildDelta(msg session.ChildDelta) (Model, tea.Cmd) { return m, nil }
func (m Model) handleChildCompleted(msg session.ChildComplete) (Model, tea.Cmd) { return m, nil }
func (m Model) handleChildBlocked(msg session.ChildBlock) (Model, tea.Cmd) { return m, nil }
func (m Model) handleChildFailed(msg session.ChildFail) (Model, tea.Cmd) { return m, nil }
func (m Model) handleChildCanceled(msg session.ChildCancel) (Model, tea.Cmd) { return m, nil }

func (m Model) handleSubagentMessage(msg session.AgentMessage) (Model, tea.Cmd) {
	return m, m.awaitSessionEvent()
}

type runtimeRequestController struct {
	model *Model
}

func (m *Model) runtimeRequest() runtimeRequestController {
	return runtimeRequestController{model: m}
}

func (c runtimeRequestController) begin(status string) uint64 {
	decision := session.BeginRuntimeRequest(session.RuntimeRequestBeginInput{
		Current: c.model.Model.RuntimeSwitchRequest,
		Status:  status,
	})
	c.model.Model.RuntimeSwitchRequest = decision.RequestID
	if decision.SetLocalStatus {
		c.model.progressReducer().beginLocalStatus(decision.Status)
	}
	return decision.RequestID
}

func (c runtimeRequestController) matches(requestID uint64) bool {
	return session.RuntimeRequestMatches(c.model.Model.RuntimeSwitchRequest, requestID)
}

func (c runtimeRequestController) finish(requestID uint64) bool {
	decision := session.FinishRuntimeRequest(c.model.Model.RuntimeSwitchRequest, requestID)
	if !decision.Matched {
		return false
	}
	c.model.Model.RuntimeSwitchRequest = decision.Active
	if decision.ClearLocalStatus {
		c.model.progressReducer().clearLocalBusyStatus()
	}
	return true
}

func (c runtimeRequestController) clear() {
	decision := session.ClearRuntimeRequest()
	c.model.Model.RuntimeSwitchRequest = decision.Active
	if decision.ClearLocalStatus {
		c.model.progressReducer().clearLocalBusyStatus()
	}
}
