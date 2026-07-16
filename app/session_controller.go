package app

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
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
	images := cloneImageAttachments(m.Input.Images)
	if text == "" && len(images) == 0 {
		return m, nil
	}
	if m.Model.RuntimeSwitchRequest != 0 {
		return m, cmdError("Wait for the runtime switch to finish before sending input.")
	}
	if strings.HasPrefix(text, "/") {
		if len(images) > 0 {
			return m, cmdError("image attachments cannot be used with slash commands")
		}
		return m.submitText(text)
	}
	if m.localCommandBusy() {
		return m.submitBusyInput(text, images)
	}

	return m.submitTextWithImages(text, images)
}

func (m Model) submitText(text string) (Model, tea.Cmd) {
	return m.submitTextWithImages(text, nil)
}

func (m Model) submitTextWithImages(text string, images []session.ImageContent) (Model, tea.Cmd) {
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
	return m, submitTurnCmd(m.Model.Runner, text, draft, images)
}

func submitTurnCmd(runner agent.Runner, text, draft string, images []session.ImageContent) tea.Cmd {
	images = cloneImageAttachments(images)
	return func() tea.Msg {
		if runner != nil {
			_, err := runner.Prompt(context.Background(), text, images...)
			return turnSubmitResultMsg{text: text, draft: draft, images: images, err: err}
		}
		return turnSubmitResultMsg{
			text:   text,
			draft:  draft,
			images: images,
			err:    errors.New("turn execution requires a configured provider and model"),
		}
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
		m.Input.Images = cloneImageAttachments(msg.images)
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

func (m Model) submitBusyInput(text string, images []session.ImageContent) (Model, tea.Cmd) {
	mode := ""
	if m.Model.Config != nil {
		mode = m.Model.Config.BusyInputMode()
	}
	runner := m.Model.Runner
	supportsSteering := runner != nil
	supportsFollowUp := runner != nil

	switch RouteBusyInput(BusyInputRouting{
		Mode:             mode,
		Thinking:         m.InFlight.Thinking,
		Compacting:       m.Progress.Compacting,
		SupportsSteering: supportsSteering,
		SupportsFollowUp: supportsFollowUp,
	}) {
	case BusyInputRouteSteer:
		m.resetComposerDraft()
		_ = runner.Steer(text, images...) // ignore idle error; TUI can't fix harness state
		return m, nil
	case BusyInputRouteFollowUp:
		m.resetComposerDraft()
		_ = runner.FollowUp(text, images...)
		return m, nil
	default:
		if len(images) > 0 {
			return m, cmdError("image attachments require an active agent turn")
		}
		return m.queueBusyInputLocal(text)
	}
}

func (m Model) queueBusyInput(text string) (Model, tea.Cmd) {
	if m.InFlight.Thinking && !m.Progress.Compacting && m.Model.Runner != nil {
		m.resetComposerDraft()
		_ = m.Model.Runner.FollowUp(text)
		return m, nil
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

func (m Model) queueBusyInputLocal(text string) (Model, tea.Cmd) {
	m.turnReducer().QueueTurn(text)
	m.resetComposerDraft()
	entry, _ := session.EntrySystem("Queued follow-up", time.Time{})
	return m, m.terminalCommit().Entries(entry)
}

func (m Model) recallQueuedTurns() (Model, tea.Cmd) {
	decision := DecideQueuedInputRecall(QueuedInputRecallInput{
		CurrentDraft: m.Input.Composer.Value(),
		Steering:     m.InFlight.QueuedSteering,
		FollowUp:     m.InFlight.QueuedTurns,
		BackendOwned: m.InFlight.QueuedTurnsBackendOwned,
	})
	if !decision.Recall {
		return m, nil
	}
	m.turnReducer().ClearQueuedTurns()
	return m, m.setComposerDraft(decision.ComposerText)
}

func cloneImageAttachments(images []session.ImageContent) []session.ImageContent {
	if len(images) == 0 {
		return nil
	}
	cloned := make([]session.ImageContent, len(images))
	for i, image := range images {
		cloned[i] = session.ImageContent{
			Data:     append([]byte(nil), image.Data...),
			MimeType: image.MimeType,
		}
	}
	return cloned
}

func (m Model) cancelRunningTurn(reason string) (Model, tea.Cmd) {
	decision := m.turnReducer().CancelTurn(reason, time.Now())
	entry, _ := session.EntrySystem(decision.EntryContent, time.Time{})
	return m, batchCmds(
		m.terminalCommit().Entries(entry),
		m.persistEntryCmd("persist cancellation", StoreSystem{
			Type:    "system",
			Content: session.EntryText(entry),
			TS:      now(),
		}),
		cancelTurnCmd(m.Model.Runner),
	)
}

func cancelTurnCmd(runner agent.Runner) tea.Cmd {
	return func() tea.Msg {
		if runner == nil {
			return turnCancelResultMsg{err: errors.New("session unavailable")}
		}
		if _, _, err := runner.Abort(); err != nil {
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
	var done <-chan struct{}
	if m.Model.Runner != nil {
		events = m.Model.Runner.Events()
		if source, ok := m.Model.Runner.(interface{ Done() <-chan struct{} }); ok {
			done = source.Done()
		}
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
		select {
		case ev, ok := <-events:
			if !ok {
				return streamClosedMsg{generation: generation}
			}
			return sessionEventMsg{generation: generation, event: ev}
		case <-done:
			return streamClosedMsg{generation: generation}
		}
	}
}

// handleSessionEvent processes events from the agent session channel.
func (m Model) handleSessionEvent(ev session.Event) (Model, tea.Cmd) {
	turn := m.turnReducer()
	if turn.DrainingUntilTurnStarted() {
		decision := DecideEventDrain(EventDrainInput{
			Active:         m.InFlight.DrainUntilTurnStarted,
			DrainStartedAt: m.InFlight.DrainStartedAt,
			Event:          ev,
		})
		if decision.Action == EventDrainAwait {
			return m, m.awaitSessionEvent()
		}
		if decision.FinishDrain {
			turn.FinishDrain()
		}
	}

	switch msg := ev.(type) {
	case StatusChange:
		return m.handleStatusChanged(msg)

	case QueuedInputUpdate:
		return m.handleQueuedInputUpdated(msg)

	case session.TurnStart:
		return m.handleTurnStarted(msg)

	case session.MessageStart:
		// Keep the assistant's partial message in Plane B so streaming text is
		// visible before MessageEnd commits it to the transcript.
		m.turnReducer().StartAssistantMessage(msg.Message)
		return m, m.awaitSessionEvent()

	case session.TurnEnd:
		if msg.Error != nil {
			return m.handleSessionError(msg.Error, true)
		}
		return m.handleTurnFinished()

	case session.QueueUpdate:
		return m.handleQueueUpdate(msg)

	case session.Settled:
		return m.handleSettled(msg)

	case session.Abort:
		return m.handleAbort(msg)

	case session.SavePoint:
		// Internal lifecycle — writes are durable. No user-visible effect needed.
		return m, m.awaitSessionEvent()

	case session.AgentStart:
		return m, m.awaitSessionEvent()

	case session.AgentEnd:
		return m, m.awaitSessionEvent()

	case session.MessageUpdate:
		return m.handleMessageUpdate(msg)

	case session.AfterProviderResponse:
		// Provider responded; extension point for cost tracking / header logging.
		return m, m.awaitSessionEvent()

	case session.ProviderRetry:
		m.Progress.Mode = StateStreaming
		m.Progress.Status = providerRetryStatus(msg)
		m.Progress.StatusUpdatedAt = msg.Timestamp
		return m, m.awaitSessionEvent()

	case session.MessageEnd:
		return m.handleMessageEnd(msg)

	case session.ToolExecStart:
		return m.handleToolExecStart(msg)
	case session.ToolExecUpdate:
		return m.handleToolExecUpdate(msg)
	case session.ToolExecEnd:
		return m.handleToolExecEnd(msg)

	case session.ApprovalRequest:
		m.Picker.Approval = &approvalPromptState{request: msg}
		return m, m.awaitSessionEvent()

	case session.ApprovalResolution:
		if m.Picker.Approval != nil && m.Picker.Approval.request.ID == msg.ID {
			m.Picker.Approval = nil
		}
		return m, m.awaitSessionEvent()

	case *session.Error:
		// Background errors from harness (persist failures, flush writes, auto-compact).
		entry, _ := session.EntrySystem(fmt.Sprintf("⚠️  %s", msg.Err.Error()), time.Now())
		return m, tea.Sequence(m.terminalCommit().Entries(entry), m.awaitSessionEvent())
	}

	return m, m.awaitSessionEvent()
}

// handleQueueUpdate shows queued messages in the terminal when steer/followUp change.
func (m Model) handleQueueUpdate(msg session.QueueUpdate) (Model, tea.Cmd) {
	var parts []string
	if len(msg.Steer) > 0 {
		parts = append(parts, fmt.Sprintf("🧭 %d steered", len(msg.Steer)))
	}
	if len(msg.FollowUp) > 0 {
		parts = append(parts, fmt.Sprintf("🔁 %d follow-up", len(msg.FollowUp)))
	}
	if len(msg.NextTurn) > 0 {
		parts = append(parts, fmt.Sprintf("⏭️  %d next-turn", len(msg.NextTurn)))
	}
	if len(parts) == 0 {
		return m, m.awaitSessionEvent()
	}
	entry, _ := session.EntrySystem(fmt.Sprintf("📋 %s", strings.Join(parts, " · ")), time.Now())
	return m, tea.Sequence(m.terminalCommit().Entries(entry), m.awaitSessionEvent())
}

// handleSettled marks the harness as idle — enables submit button and clears in-flight state.
func (m Model) handleSettled(msg session.Settled) (Model, tea.Cmd) {
	m.InFlight.AgentCommitted = false
	m.InFlight.Thinking = false
	m.InFlight.Canceling = false
	m.Progress.Mode = StateReady
	m.Progress.Status = ""
	if msg.NextTurnCount > 0 {
		entry, _ := session.EntrySystem(fmt.Sprintf("ℹ️  %d queued turn(s) remaining", msg.NextTurnCount), time.Now())
		return m, tea.Sequence(m.terminalCommit().Entries(entry), m.awaitSessionEvent())
	}
	return m, m.awaitSessionEvent()
}

// handleAbort shows what was cleared when a run is cancelled.
func (m Model) handleAbort(msg session.Abort) (Model, tea.Cmd) {
	var parts []string
	if len(msg.ClearedSteer) > 0 {
		parts = append(parts, fmt.Sprintf("🧭 steered (%d)", len(msg.ClearedSteer)))
	}
	if len(msg.ClearedFollowUp) > 0 {
		parts = append(parts, fmt.Sprintf("🔁 follow-up (%d)", len(msg.ClearedFollowUp)))
	}
	if len(parts) == 0 {
		return m, m.awaitSessionEvent()
	}
	entry, _ := session.EntrySystem(fmt.Sprintf("🛑 Aborted: %s", strings.Join(parts, ", ")), time.Now())
	return m, tea.Sequence(m.terminalCommit().Entries(entry), m.awaitSessionEvent())
}

// handleStreamClosed displays a stream-closed system entry.
func (m Model) handleStreamClosed() (Model, tea.Cmd) {
	entryIf, _ := m.turnReducer().StreamClosed(time.Now())
	var cmds []tea.Cmd
	cmds = append(cmds, m.terminalCommit().Entries(entryIf))
	cmds = append(cmds, m.persistEntryCmd("persist stream close error", StoreSystem{
		Type:    "system",
		Content: session.EntryText(entryIf),
		TS:      now(),
	}))
	return m, sequenceCmds(cmds...)
}

func (m Model) handleSessionError(err error, awaitTerminal bool) (Model, tea.Cmd) {
	decision := DecideErrorSettlement(ErrorSettlementInput{
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
		cmds = append(cmds, m.persistEntryCmd("persist session error", StoreSystem{
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

func (m Model) handleStatusChanged(msg StatusChange) (Model, tea.Cmd) {
	decision := m.turnReducer().ApplyStatusChangedInput(msg)
	persistTimestamp := msg.When()
	if decision.Root {
		persistTimestamp = decision.PersistTimestamp
	}
	return m, batchCmds(m.persistEntryCmd("persist status", StoreStatus{
		Type:   "status",
		Status: msg.Status,
		TS:     entryUnix(persistTimestamp),
	}), m.awaitSessionEvent())
}

func (m Model) handleQueuedInputUpdated(msg QueuedInputUpdate) (Model, tea.Cmd) {
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
		if errText := assistantErrorText(assistant); errText != "" {
			notice, _ := session.EntrySystem("Error: "+errText, time.Now())
			cmds = append(cmds, m.terminalCommit().Entries(notice))
		}
	}
	if entry, ok := m.turnReducer().FinishTurnMode(assistantCompleted); ok {
		cmds = append(cmds, m.terminalCommit().Entries(entry))
	}
	m.turnReducer().RecordFinishedTurnSummary(time.Now())

	dispatch := m.turnReducer().FinishTurnDispatch()
	if dispatch.Action == TurnFinishedDispatchSubmitLocal {
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
	if cmd := m.persistCurrentSessionInfoCmd(); cmd != nil {
		cmds = append(cmds, cmd)
	}
	if dispatch.AwaitNext {
		cmds = append(cmds, m.awaitSessionEvent())
	}
	return m, tea.Sequence(cmds...)
}

func assistantErrorText(entry session.Entry) string {
	messageEntry, ok := entry.(*session.MessageEntry)
	if !ok {
		return ""
	}
	assistant, ok := messageEntry.Message.(*session.AssistantMessage)
	if !ok || assistant.StopReason != session.StopReasonError {
		return ""
	}
	return strings.TrimSpace(assistant.Error)
}

func (m Model) handleMessageEnd(msg session.MessageEnd) (Model, tea.Cmd) {
	// Token usage rides on MessageEnd (one per model call, including tool-use turns).
	// Also commit the assistant to mark the turn as having an assistant response.
	m.turnReducer().ApplyTokenUsage(msg.Message)
	if _, isAssistant := msg.Message.(*session.AssistantMessage); isAssistant {
		m.turnReducer().CommitAgentMessage(msg.Message)
	}
	var cmds []tea.Cmd
	if m.Model.Config != nil && m.InFlight.Thinking {
		if reason := BudgetStopReason(BudgetStopInput{
			CurrentTurnCost: m.Progress.CurrentTurnCost,
			TotalCost:       m.Progress.TotalCost,
			MaxTurnCost:     m.Model.Config.MaxTurnCost,
			MaxSessionCost:  m.Model.Config.MaxSessionCost,
		}); reason != "" {
			m.Progress.BudgetStopReason = reason
			var cancelCmd tea.Cmd
			m, cancelCmd = m.cancelRunningTurn(reason)
			cmds = append(cmds, cancelCmd)
		}
	}
	in, out, cost := session.TokenUsage(msg.Message)
	if in > 0 || out > 0 || cost > 0 {
		cmds = append(cmds, m.persistEntryCmd("persist token usage", StoreTokenUsage{
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
	// The partial assistant message is authoritative: it contains the full
	// accumulated content, unlike the event delta which is only one chunk.
	m.turnReducer().UpdateAssistantMessage(msg.Message)
	// Keep the delta fallback for providers that omit the partial message.
	switch delta := msg.Delta.(type) {
	case session.ThinkingDelta, *session.ThinkingDelta:
		if msg.Message == nil {
			m.turnReducer().AppendThinkingDelta(msg.AgentID, delta)
		}
	case session.TextDelta, *session.TextDelta:
		if msg.Message == nil {
			m.turnReducer().AppendAgentDelta(msg.AgentID, delta, msg.When())
		}
	}
	return m, m.awaitSessionEvent()
}

// Old event handlers (AgentMessage, ToolCallStart, ToolExecutionUpdate, ToolCallEnd,
// ChildRequest/Start/Delta/Complete/Block/Fail/Cancel) removed.
// Token usage and tool tracking will be handled via TurnEnd and ToolExecStart/End.

func (m Model) handleToolExecStart(msg session.ToolExecStart) (Model, tea.Cmd) {
	m.turnReducer().StartToolCall(
		msg.ToolCallID,
		time.Now(),
		config.Redact(m.formatToolTitle(msg.Name, string(msg.Args))),
	)
	return m, m.awaitSessionEvent()
}

func (m Model) handleToolExecUpdate(msg session.ToolExecUpdate) (Model, tea.Cmd) {
	m.turnReducer().AppendToolOutput(msg.ToolCallID, fmt.Sprintf("%v", msg.Partial), false)
	return m, m.awaitSessionEvent()
}

func (m Model) handleToolExecEnd(msg session.ToolExecEnd) (Model, tea.Cmd) {
	toolUseID := msg.ToolCallID
	if toolUseID == "" {
		toolUseID = m.Progress.LastToolUseID
	}
	if entry, ok := m.turnReducer().CompleteToolResult(toolUseID, msg); ok {
		return m, tea.Sequence(m.terminalCommit().Entries(entry), m.awaitSessionEvent())
	}
	return m, m.awaitSessionEvent()
}

// See ARCHITECTURE-PLAN.md Phase 1.

type runtimeRequestController struct {
	model *Model
}

func (m *Model) runtimeRequest() runtimeRequestController {
	return runtimeRequestController{model: m}
}

func (c runtimeRequestController) begin(status string) uint64 {
	decision := BeginRuntimeRequest(RuntimeRequestBeginInput{
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
	return RuntimeRequestMatches(c.model.Model.RuntimeSwitchRequest, requestID)
}

func (c runtimeRequestController) finish(requestID uint64) bool {
	decision := FinishRuntimeRequest(c.model.Model.RuntimeSwitchRequest, requestID)
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
	decision := ClearRuntimeRequest()
	c.model.Model.RuntimeSwitchRequest = decision.Active
	if decision.ClearLocalStatus {
		c.model.progressReducer().clearLocalBusyStatus()
	}
}

type persistenceController struct {
	runner agent.Runner
}

func (m Model) persistenceController() persistenceController {
	return persistenceController{runner: m.Model.Runner}
}

func (c persistenceController) appendEntry(action string, entry session.Entry) tea.Cmd {
	if c.runner == nil {
		return nil
	}
	return func() tea.Msg {
		if err := c.runner.PersistEntry(context.Background(), entry); err != nil {
			return localErrorMsg{err: fmt.Errorf("%s: %w", action, err)}
		}
		return nil
	}
}

func persistErrorCmd(action string, err error) tea.Cmd {
	if err == nil {
		return nil
	}
	return func() tea.Msg {
		return localErrorMsg{err: fmt.Errorf("%s: %w", action, err)}
	}
}

func (m Model) persistCurrentSessionInfoCmd() tea.Cmd {
	if m.Model.Store == nil {
		return nil
	}
	info, ok := m.currentSessionInfo(context.Background())
	if !ok {
		return nil
	}
	store, ok := m.Model.Store.(sessionCatalogWriter)
	if !ok {
		return nil
	}
	return func() tea.Msg {
		if err := store.UpdateSession(context.Background(), info); err != nil {
			return localErrorMsg{err: fmt.Errorf("persist session info: %w", err)}
		}
		return nil
	}
}

func (m Model) currentSessionInfo(ctx context.Context) (session.SessionInfoEntry, bool) {
	id := ""
	if m.Model.Store != nil {
		id = m.Model.Store.GetLeafID()
	}
	if id == "" && m.activeSession() != nil {
		id = m.activeSession().ID()
	}
	if id == "" || id == "ion" {
		return session.SessionInfoEntry{}, false
	}

	var entries []session.Entry
	var err error
	if m.activeSession() != nil {
		entries, err = m.activeSession().Branch(ctx)
	} else if m.Model.Store != nil {
		entries, err = m.Model.Store.Branch(ctx)
	}
	if err != nil {
		return session.SessionInfoEntry{}, false
	}

	firstUser := ""
	lastUser := ""
	for _, entry := range entries {
		me, ok := entry.(*session.MessageEntry)
		if !ok {
			continue
		}
		if _, ok := me.Message.(*session.UserMessage); !ok {
			continue
		}
		text := strings.TrimSpace(session.MessageText(me.Message))
		if text == "" {
			continue
		}
		if firstUser == "" {
			firstUser = text
		}
		lastUser = text
	}
	if lastUser == "" {
		return session.SessionInfoEntry{}, false
	}

	return session.SessionInfoEntry{
		EntryBase:   session.EntryBase{ID: id, Timestamp: time.Now()},
		Workdir:     m.App.Workdir,
		Model:       m.currentSessionModelName(),
		Branch:      m.App.Branch,
		Name:        truncateRunes(firstUser, 80),
		LastPreview: truncateRunes(lastUser, 120),
		UpdatedAt:   time.Now(),
	}, true
}

func (m Model) currentSessionModelName() string {
	provider := ""
	model := ""
	if m.Model.Backend != nil {
		provider = strings.TrimSpace(m.Model.Backend.Provider())
		model = strings.TrimSpace(m.Model.Backend.Model())
	}
	if m.Model.Config != nil {
		if provider == "" {
			provider = strings.TrimSpace(m.Model.Config.Provider)
		}
		if model == "" {
			model = strings.TrimSpace(m.Model.Config.Model)
		}
	}
	if provider == "" {
		return model
	}
	if model == "" {
		return provider
	}
	return provider + "/" + model
}

func (m Model) persistEntryCmd(action string, entry session.Entry) tea.Cmd {
	// Convert runtime-specific entries to session.CustomEntry for SQLite persistence.
	// Each gets a unique ID (not the original entry's zero-valued ID) to avoid
	// UNIQUE constraint failures when multiple entries have empty/identical IDs.
	now := time.Now()
	id := fmt.Sprintf("%s-%d", action, now.UnixNano())
	switch e := entry.(type) {
	case StoreRoutingDecision:
		data, _ := json.Marshal(e)
		entry = &session.CustomEntry{
			EntryBase: session.EntryBase{ID: id, ParentID: e.ParentID(), Timestamp: now},
			Type:      "routing_decision",
			Data:      data,
		}
	case StoreSystem:
		data, _ := json.Marshal(e)
		entry = &session.CustomEntry{
			EntryBase: session.EntryBase{ID: id, ParentID: e.ParentID(), Timestamp: now},
			Type:      "store_system",
			Data:      data,
		}
	case StoreStatus:
		data, _ := json.Marshal(e)
		entry = &session.CustomEntry{
			EntryBase: session.EntryBase{ID: id, ParentID: e.ParentID(), Timestamp: now},
			Type:      "store_status",
			Data:      data,
		}
	case StoreTokenUsage:
		data, _ := json.Marshal(e)
		entry = &session.CustomEntry{
			EntryBase: session.EntryBase{ID: id, ParentID: e.ParentID(), Timestamp: now},
			Type:      "store_token_usage",
			Data:      data,
		}
	}
	return m.persistenceController().appendEntry(action, entry)
}

func sequenceCmds(cmds ...tea.Cmd) tea.Cmd {
	filtered := cmds[:0]
	for _, cmd := range cmds {
		if cmd != nil {
			filtered = append(filtered, cmd)
		}
	}
	switch len(filtered) {
	case 0:
		return nil
	case 1:
		return filtered[0]
	default:
		return tea.Sequence(filtered...)
	}
}

func batchCmds(cmds ...tea.Cmd) tea.Cmd {
	filtered := cmds[:0]
	for _, cmd := range cmds {
		if cmd != nil {
			filtered = append(filtered, cmd)
		}
	}
	switch len(filtered) {
	case 0:
		return nil
	case 1:
		return filtered[0]
	default:
		return tea.Batch(filtered...)
	}
}

func entryUnix(timestamp time.Time) int64 {
	if timestamp.IsZero() {
		return now()
	}
	return timestamp.UTC().Unix()
}
