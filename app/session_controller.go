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
	"github.com/nijaru/ion/internal/runtime"
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

func submitTurnCmd(_ session.Session, runner runtime.Runner, text, draft string) tea.Cmd {
	return func() tea.Msg {
		if runner != nil {
			_, err := runner.Prompt(context.Background(), text)
			return turnSubmitResultMsg{text: text, draft: draft, err: err}
		}
		return turnSubmitResultMsg{
			text:  text,
			draft: draft,
			err:   errors.New("turn execution requires a configured provider and model"),
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
	runner := m.Model.Runner
	supportsSteering := runner != nil
	supportsFollowUp := runner != nil

	switch runtime.RouteBusyInput(runtime.BusyInputRouting{
		Mode:             mode,
		Thinking:         m.InFlight.Thinking,
		Compacting:       m.Progress.Compacting,
		SupportsSteering: supportsSteering,
		SupportsFollowUp: supportsFollowUp,
	}) {
	case runtime.BusyInputRouteSteer:
		m.resetComposerDraft()
		_ = runner.Steer(text) // ignore idle error; TUI can't fix harness state
		return m, nil
	case runtime.BusyInputRouteFollowUp:
		m.resetComposerDraft()
		_ = runner.FollowUp(text)
		return m, nil
	default:
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
	decision := runtime.DecideQueuedInputRecall(runtime.QueuedInputRecallInput{
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

func (m Model) cancelRunningTurn(reason string) (Model, tea.Cmd) {
	decision := m.turnReducer().CancelTurn(reason, time.Now())
	entry, _ := session.EntrySystem(decision.EntryContent, time.Time{})
	return m, batchCmds(
		m.terminalCommit().Entries(entry),
		m.persistEntryCmd("persist cancellation", runtime.StoreSystem{
			Type:    "system",
			Content: session.EntryText(entry),
			TS:      now(),
		}),
		cancelTurnCmd(m.Model.Runner),
	)
}

func cancelTurnCmd(runner runtime.Runner) tea.Cmd {
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
		decision := runtime.DecideEventDrain(runtime.EventDrainInput{
			Active:         m.InFlight.DrainUntilTurnStarted,
			DrainStartedAt: m.InFlight.DrainStartedAt,
			Event:          ev,
		})
		if decision.Action == runtime.EventDrainAwait {
			return m, m.awaitSessionEvent()
		}
		if decision.FinishDrain {
			turn.FinishDrain()
		}
	}

	switch msg := ev.(type) {
	case runtime.StatusChange:
		return m.handleStatusChanged(msg)

	case runtime.QueuedInputUpdate:
		return m.handleQueuedInputUpdated(msg)

	case session.TurnStart:
		return m.handleTurnStarted(msg)

	case session.MessageStart:
		// Message boundary for scoping; current TUI delegates to MessageUpdate deltas.
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

	case session.MessageEnd:
		return m.handleMessageEnd(msg)

	case session.ToolExecStart:
		return m.handleToolExecStart(msg)
	case session.ToolExecUpdate:
		return m.handleToolExecUpdate(msg)
	case session.ToolExecEnd:
		return m.handleToolExecEnd(msg)

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
	cmds = append(cmds, m.persistEntryCmd("persist stream close error", runtime.StoreSystem{
		Type:    "system",
		Content: session.EntryText(entryIf),
		TS:      now(),
	}))
	return m, sequenceCmds(cmds...)
}

func (m Model) handleSessionError(err error, awaitTerminal bool) (Model, tea.Cmd) {
	decision := runtime.DecideErrorSettlement(runtime.ErrorSettlementInput{
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
		cmds = append(cmds, m.persistEntryCmd("persist session error", runtime.StoreSystem{
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

func (m Model) handleStatusChanged(msg runtime.StatusChange) (Model, tea.Cmd) {
	decision := m.turnReducer().ApplyStatusChangedInput(msg)
	persistTimestamp := msg.When()
	if decision.Root {
		persistTimestamp = decision.PersistTimestamp
	}
	return m, batchCmds(m.persistEntryCmd("persist status", runtime.StoreStatus{
		Type:   "status",
		Status: msg.Status,
		TS:     entryUnix(persistTimestamp),
	}), m.awaitSessionEvent())
}

func (m Model) handleQueuedInputUpdated(msg runtime.QueuedInputUpdate) (Model, tea.Cmd) {
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
	if dispatch.Action == runtime.TurnFinishedDispatchSubmitLocal {
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

func (m Model) handleMessageEnd(msg session.MessageEnd) (Model, tea.Cmd) {
	// Token usage rides on MessageEnd (one per model call, including tool-use turns).
	// Also commit the assistant to mark the turn as having an assistant response.
	m.turnReducer().ApplyTokenUsage(msg.Message)
	if _, isAssistant := msg.Message.(*session.AssistantMessage); isAssistant {
		m.turnReducer().CommitAgentMessage(msg.Message)
	}
	var cmds []tea.Cmd
	in, out, cost := session.TokenUsage(msg.Message)
	if in > 0 || out > 0 || cost > 0 {
		cmds = append(cmds, m.persistEntryCmd("persist token usage", runtime.StoreTokenUsage{
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
	decision := runtime.BeginRuntimeRequest(runtime.RuntimeRequestBeginInput{
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
	return runtime.RuntimeRequestMatches(c.model.Model.RuntimeSwitchRequest, requestID)
}

func (c runtimeRequestController) finish(requestID uint64) bool {
	decision := runtime.FinishRuntimeRequest(c.model.Model.RuntimeSwitchRequest, requestID)
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
	decision := runtime.ClearRuntimeRequest()
	c.model.Model.RuntimeSwitchRequest = decision.Active
	if decision.ClearLocalStatus {
		c.model.progressReducer().clearLocalBusyStatus()
	}
}

type persistenceController struct {
	storage session.Session
}

func (m Model) persistenceController() persistenceController {
	return persistenceController{storage: m.Model.Storage}
}

func (c persistenceController) appendEntry(action string, entry session.Entry) tea.Cmd {
	if c.storage == nil {
		return nil
	}
	return func() tea.Msg {
		if _, err := c.storage.Append(context.Background(), entry); err != nil {
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
	if id == "" && m.Model.Session != nil {
		id = m.Model.Session.ID()
	}
	if id == "" || id == "canto" {
		return session.SessionInfoEntry{}, false
	}

	var entries []session.Entry
	var err error
	if m.Model.Session != nil {
		entries, err = m.Model.Session.Branch(ctx)
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

func (m Model) persistEntryCmd(action string, entry runtime.StoreEvent) tea.Cmd {
	// Convert runtime-specific entries to session.CustomEntry for SQLite persistence.
	// Each gets a unique ID (not the original entry's zero-valued ID) to avoid
	// UNIQUE constraint failures when multiple entries have empty/identical IDs.
	now := time.Now()
	id := fmt.Sprintf("%s-%d", action, now.UnixNano())
	switch e := entry.(type) {
	case runtime.StoreRoutingDecision:
		data, _ := json.Marshal(e)
		entry = &session.CustomEntry{
			EntryBase: session.EntryBase{ID: id, ParentID: e.ParentID(), Timestamp: now},
			Type:      "routing_decision",
			Data:      data,
		}
	case runtime.StoreSystem:
		data, _ := json.Marshal(e)
		entry = &session.CustomEntry{
			EntryBase: session.EntryBase{ID: id, ParentID: e.ParentID(), Timestamp: now},
			Type:      "store_system",
			Data:      data,
		}
	case runtime.StoreStatus:
		data, _ := json.Marshal(e)
		entry = &session.CustomEntry{
			EntryBase: session.EntryBase{ID: id, ParentID: e.ParentID(), Timestamp: now},
			Type:      "store_status",
			Data:      data,
		}
	case runtime.StoreTokenUsage:
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
