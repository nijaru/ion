package app

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"sync/atomic"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/session"
)

type turnCancellationState struct {
	cancel          context.CancelFunc
	token           atomic.Uint64
	accepted        atomic.Bool
	started         atomic.Bool
	cancelRequested atomic.Bool
	preCancelled    atomic.Bool
}

func newTurnCancellationState(parent context.Context) (*turnCancellationState, context.Context) {
	ctx, cancel := context.WithCancel(parent)
	return &turnCancellationState{cancel: cancel}, ctx
}

func (s *turnCancellationState) setToken(token uint64) {
	if s != nil && token != 0 {
		s.token.Store(token)
	}
}

func (s *turnCancellationState) markAccepted() {
	if s != nil {
		s.accepted.Store(true)
	}
}

func (s *turnCancellationState) isAccepted() bool {
	return s != nil && s.accepted.Load()
}

func (s *turnCancellationState) markStarted() {
	if s != nil {
		s.started.Store(true)
		s.accepted.Store(true)
	}
}

func (s *turnCancellationState) isStarted() bool {
	return s != nil && s.started.Load()
}

func (s *turnCancellationState) markCancelRequested() {
	if s != nil {
		s.cancelRequested.Store(true)
	}
}

func (s *turnCancellationState) wasCancelRequested() bool {
	return s != nil && s.cancelRequested.Load()
}

func (s *turnCancellationState) markPreCancelled() {
	if s != nil {
		s.preCancelled.Store(true)
	}
}

func (s *turnCancellationState) wasPreCancelled() bool {
	return s != nil && s.preCancelled.Load()
}

func (s *turnCancellationState) turnToken() uint64 {
	if s == nil {
		return 0
	}
	return s.token.Load()
}

func (s *turnCancellationState) stop() {
	if s != nil && s.cancel != nil {
		s.cancel()
	}
}

type localErrorMsg struct {
	err error
}

type runtimeLeafSnapshotMsg struct {
	generation uint64
	leafID     string
	info       *session.SessionInfoEntry
	err        error
}

type runtimeCatalogUpdateMsg struct {
	generation uint64
	err        error
}

type inputHistoryResultMsg struct {
	generation uint64
	err        error
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
			historyCmd = m.persistInputHistory(m.runtimeOperationContext(), historyText)
		}
		m.resetComposerDraft()
		m, cmd := m.handleCommand(text)
		return m, sequenceCmds(cmd, historyCmd)
	}

	m.turnReducer().StartSubmit()
	m.Model.TurnSubmitRequest++
	requestID := m.Model.TurnSubmitRequest
	turnState, turnContext := newTurnCancellationState(m.runtimeOperationContext())
	m.replaceTurnCancellation(turnState)
	m.resetComposerDraft()
	return m, submitTurnCmd(
		m.Model.Runner,
		turnContext,
		turnState,
		m.Model.EventGeneration,
		requestID,
		text,
		draft,
		images,
	)
}

func submitTurnCmd(
	runner agent.Runtime,
	ctx context.Context,
	state *turnCancellationState,
	generation, requestID uint64,
	text, draft string,
	images []session.ImageContent,
) tea.Cmd {
	images = cloneImageAttachments(images)
	return func() tea.Msg {
		if runner != nil {
			promptContext := agent.WithTurnTokenSink(ctx, state.setToken)
			promptContext = agent.WithTurnAcceptanceSink(promptContext, state.markAccepted)
			_, err := runner.Prompt(promptContext, text, images...)
			return turnSubmitResultMsg{
				generation: generation,
				requestID:  requestID,
				text:       text,
				draft:      draft,
				images:     images,
				err:        err,
			}
		}
		return turnSubmitResultMsg{
			generation: generation,
			requestID:  requestID,
			text:       text,
			draft:      draft,
			images:     images,
			err:        errors.New("turn execution requires a configured provider and model"),
		}
	}
}

func isTurnCancellationError(err error) bool {
	if errors.Is(err, context.Canceled) {
		return true
	}
	var turnErr *agent.TurnError
	return errors.As(err, &turnErr) && turnErr.Kind == agent.KindCancellation
}

func (m Model) handleTurnSubmitResult(msg turnSubmitResultMsg) (Model, tea.Cmd) {
	// Successful Prompt results may arrive after the runtime has already
	// started a queued turn. They still own their accepted history/catalog
	// side effects, but a result from a future request is impossible to apply.
	if msg.generation != m.Model.EventGeneration ||
		msg.requestID == 0 || msg.requestID > m.Model.TurnSubmitRequest {
		return m, nil
	}
	// Errors can restore a draft only for the currently accepted turn. An old
	// error must not clear or overwrite the newer turn's composer state.
	if msg.err != nil && msg.requestID != m.Model.TurnSubmitRequest {
		return m, nil
	}
	if msg.err != nil && isTurnCancellationError(msg.err) && m.InFlight.Canceling {
		state := m.Model.turnCancellation
		if state != nil && state.wasPreCancelled() && !state.isAccepted() {
			m.clearTurnCancellation()
			m.turnReducer().RejectSubmit("")
		}
		return m, nil
	}
	m.refreshRuntimeSessionSnapshot()
	if msg.err == nil {
		historyText, historyChanged := m.appendInputHistory(msg.text)
		var historyCmd tea.Cmd
		if historyChanged {
			historyCmd = m.persistInputHistory(m.runtimeOperationContext(), historyText)
		}
		leafCmd := m.persistCurrentSessionInfoCmd()
		if msg.rearm || m.InFlight.Thinking {
			return m, batchCmds(leafCmd, historyCmd, m.awaitSessionEvent())
		}
		return m, sequenceCmds(leafCmd, historyCmd)
	}
	if !m.InFlight.Thinking {
		// A settled runtime result may arrive after the lifecycle event. Its
		// error is already represented by the terminal event and must not
		// restore a draft into a newer or queued turn.
		return m, nil
	}
	m.clearTurnCancellation()
	m.turnReducer().RejectSubmit("")
	var draftCmd tea.Cmd
	if strings.TrimSpace(m.Input.Composer.Value()) == "" {
		draftCmd = m.setComposerDraft(msg.draft)
		m.Input.Images = cloneImageAttachments(msg.images)
	}
	return m, tea.Batch(draftCmd, cmdError(msg.err.Error()))
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
		return m, busyInputCmd(runner, m.Model.EventGeneration, "steer", text, images)
	case BusyInputRouteFollowUp:
		m.resetComposerDraft()
		return m, busyInputCmd(runner, m.Model.EventGeneration, "follow-up", text, images)
	default:
		m.resetComposerDraft()
		return m, busyInputCmd(runner, m.Model.EventGeneration, "next-turn", text, images)
	}
}

func (m Model) queueBusyInput(text string) (Model, tea.Cmd) {
	if m.InFlight.Thinking && !m.Progress.Compacting && m.Model.Runner != nil {
		m.resetComposerDraft()
		return m, busyInputCmd(m.Model.Runner, m.Model.EventGeneration, "follow-up", text, nil)
	}
	m.resetComposerDraft()
	return m, busyInputCmd(m.Model.Runner, m.Model.EventGeneration, "next-turn", text, nil)
}

func busyInputCmd(runner agent.Runtime, generation uint64, action, text string, images []session.ImageContent) tea.Cmd {
	images = cloneImageAttachments(images)
	return func() tea.Msg {
		if runner == nil {
			return busyInputResultMsg{
				generation: generation,
				action:     action,
				text:       text,
				images:     images,
				err:        errors.New("session unavailable"),
			}
		}

		var err error
		switch action {
		case "steer":
			err = runner.Steer(text, images...)
		case "follow-up":
			err = runner.FollowUp(text, images...)
		case "next-turn":
			err = runner.NextTurn(text, images...)
		default:
			err = fmt.Errorf("unsupported busy input action %q", action)
		}
		return busyInputResultMsg{
			generation: generation,
			action:     action,
			text:       text,
			images:     images,
			err:        err,
		}
	}
}

func (m Model) handleBusyInputResult(msg busyInputResultMsg) (Model, tea.Cmd) {
	if msg.generation != m.Model.EventGeneration {
		return m, nil
	}
	if msg.err == nil {
		return m, nil
	}

	// The draft was cleared optimistically so the user can continue typing
	// while the bounded runtime command is accepted. Restore it only when the
	// composer is still empty; never overwrite newer user input.
	var restoreCmd tea.Cmd
	if strings.TrimSpace(m.Input.Composer.Value()) == "" && len(m.Input.Images) == 0 {
		m.Input.Images = cloneImageAttachments(msg.images)
		restoreCmd = m.setComposerDraft(msg.text)
	}
	return m, sequenceCmds(
		restoreCmd,
		cmdError(fmt.Sprintf("%s input: %v", msg.action, msg.err)),
	)
}

// queueFollowUp queues a follow-up message when Alt+Enter is pressed while the agent is streaming.
func (m Model) queueFollowUp() (Model, tea.Cmd) {
	text := strings.TrimSpace(m.Input.Composer.Value())
	if text == "" {
		return m, nil
	}
	return m.queueBusyInput(text)
}

func (m Model) recallQueuedTurns() (Model, tea.Cmd) {
	if len(m.InFlight.QueuedSteering) == 0 && len(m.InFlight.QueuedTurns) == 0 {
		return m, nil
	}
	return m, cmdError("queued input is owned by the runtime and cannot be recalled")
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

func (m *Model) replaceTurnCancellation(state *turnCancellationState) {
	if m == nil {
		return
	}
	if m.Model.turnCancellation != nil && m.Model.turnCancellation != state {
		m.Model.turnCancellation.stop()
	}
	m.Model.turnCancellation = state
}

func (m *Model) clearTurnCancellation() {
	if m == nil {
		return
	}
	if m.Model.turnCancellation != nil {
		m.Model.turnCancellation.stop()
		m.Model.turnCancellation = nil
	}
}

func (m *Model) preserveCancellationProjection() {
	if m == nil || m.Model.turnCancellation == nil ||
		!m.Model.turnCancellation.wasCancelRequested() {
		return
	}
	m.InFlight.Canceling = true
	if m.InFlight.Thinking {
		m.Progress.Mode = StateCancelled
	}
}

func (m Model) cancelRunningTurn(reason string) (Model, tea.Cmd) {
	decision := m.turnReducer().CancelTurn(reason, time.Now())
	entry, _ := session.EntrySystem(decision.EntryContent, time.Time{})
	state := m.Model.turnCancellation
	if state == nil {
		state, _ = newTurnCancellationState(m.runtimeOperationContext())
		m.replaceTurnCancellation(state)
	}
	state.markCancelRequested()
	// A prompt that has not reached controller acceptance has no runtime turn
	// to abort; cancel its context now. Accepted turns defer context
	// cancellation until the targeted AbortTurn command has cleared queues.
	if !state.isAccepted() {
		state.markPreCancelled()
		state.stop()
	}
	return m, batchCmds(
		m.terminalCommit().Entries(entry),
		cancelTurnCmd(
			m.Model.Runner,
			m.Model.EventGeneration,
			m.Model.TurnSubmitRequest,
			state,
		),
	)
}

func cancelTurnCmd(
	runner agent.Runtime,
	generation, requestID uint64,
	state *turnCancellationState,
) tea.Cmd {
	var canceler agent.TurnCanceler
	if runner != nil {
		canceler = runner
	}
	return func() tea.Msg {
		result := turnCancelResultMsg{generation: generation, requestID: requestID}
		if runner == nil {
			result.err = errors.New("session unavailable")
			return result
		}
		turnToken := state.turnToken()
		if turnToken == 0 {
			// The prompt may still be waiting for controller acceptance. The
			// canceled prompt context is the only valid effect until TurnStart
			// publishes its identity; never sample a later runtime turn here.
			return result
		}
		if canceler == nil {
			state.stop()
			result.err = errors.New("runtime does not support turn-scoped cancellation")
			return result
		}
		if _, _, err := canceler.AbortTurn(turnToken); err != nil {
			safeStale := errors.Is(err, agent.ErrNoActiveTurn) || errors.Is(err, agent.ErrTurnChanged)
			if !(safeStale && state.wasPreCancelled()) {
				result.err = err
				return result
			}
		}
		state.stop()
		return result
	}
}

func (m Model) handleTurnCancelResult(msg turnCancelResultMsg) (Model, tea.Cmd) {
	if msg.generation != m.Model.EventGeneration ||
		(msg.requestID != 0 && msg.requestID != m.Model.TurnSubmitRequest) {
		return m, nil
	}
	if msg.err != nil {
		return m, cmdError(fmt.Sprintf("cancel turn: %v", msg.err))
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
	if m.Model.EventSubscription == nil {
		if state := m.Model.EventSubscriptionState; state != nil {
			if state.pending && state.generation == generation {
				return nil
			}
			state.generation = generation
			state.pending = true
		}
		runner := m.Model.Runner
		after := m.Model.EventCursor
		return func() tea.Msg {
			if runner == nil {
				return runtimeSubscriptionMsg{
					generation: generation,
					err:        errors.New("session event stream unavailable"),
				}
			}
			source, ok := runner.(interface {
				Subscribe(context.Context, agent.EventCursor) (*agent.EventSubscription, error)
			})
			if !ok {
				return runtimeSubscriptionMsg{
					generation: generation,
					err:        errors.New("runtime subscription unavailable"),
				}
			}
			subscription, err := source.Subscribe(m.runtimeOperationContext(), after)
			return runtimeSubscriptionMsg{generation: generation, subscription: subscription, err: err}
		}
	}
	subscription := m.Model.EventSubscription
	var reader uint64
	if state := m.Model.EventSubscriptionState; state != nil {
		if state.readerBusy && state.generation == generation {
			return nil
		}
		state.generation = generation
		state.reader++
		reader = state.reader
		state.readerBusy = true
	}
	var done <-chan struct{}
	if m.Model.Runner != nil {
		if source, ok := m.Model.Runner.(interface{ Done() <-chan struct{} }); ok {
			done = source.Done()
		}
	}
	return func() tea.Msg {
		select {
		case envelope, ok := <-subscription.Events:
			if !ok {
				return streamClosedMsg{generation: generation, err: subscription.Err()}
			}
			return sessionEventMsg{
				generation: generation,
				reader:     reader,
				// EventCursor.Next is the next sequence the reducer expects.
				// The event itself must be compared at its own sequence; the
				// reducer advances Next after accepting it.
				cursor: agent.EventCursor{Stream: envelope.Stream, Next: envelope.Sequence},
				event:  envelope.Event,
			}
		case <-done:
			return streamClosedMsg{generation: generation, err: agent.ErrRuntimeClosed}
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
		return m.handleTurnFinished(msg)

	case session.QueueUpdate:
		return m.handleQueueUpdate(msg)

	case session.Settled:
		return m.handleSettled(msg)

	case session.RuntimeReady:
		// A user turn may be accepted between the operation result and this
		// lifecycle event. That turn owns cancellation, buffers, and queues; do
		// not let an older exclusive-operation completion clear them.
		if m.Model.turnCancellation != nil || m.InFlight.AwaitingSettlement || m.Progress.Compacting ||
			len(m.InFlight.QueuedSteering) > 0 || len(m.InFlight.QueuedTurns) > 0 {
			return m, m.awaitSessionEvent()
		}
		m.InFlight.Thinking = false
		m.InFlight.Canceling = false
		m.InFlight.AgentCommitted = false
		m.Progress.Compacting = false
		m.Progress.Mode = StateReady
		m.Progress.Status = ""
		return m, m.awaitSessionEvent()

	case session.Abort:
		return m.handleAbort(msg)

	case session.SavePoint:
		// Internal lifecycle — writes are durable. No user-visible effect needed.
		return m, m.awaitSessionEvent()

	case session.AgentStart:
		return m, m.awaitSessionEvent()

	case session.AgentEnd:
		return m, m.awaitSessionEvent()

	case session.ModelUpdate, session.ThinkingUpdate, session.ToolsUpdate:
		// Runtime setters already update the app's accepted snapshot. These
		// events are lifecycle notifications for subscribers, not a second
		// source of TUI state.
		return m, m.awaitSessionEvent()

	case session.MessageUpdate:
		return m.handleMessageUpdate(msg)

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
		m.addApprovalRequest(msg)
		return m, m.awaitSessionEvent()

	case session.ApprovalResolution:
		m.resolveApprovalRequest(msg.ID)
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
	steer, followUp, nextTurn := (agent.QueueSnapshot{
		Steer:    msg.Steer,
		FollowUp: msg.FollowUp,
		NextTurn: msg.NextTurn,
	}).Texts()
	m.InFlight.QueuedSteering = steer
	m.InFlight.QueuedTurns = append(followUp, nextTurn...)

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
	m.clearTurnCancellation()
	m.InFlight.AgentCommitted = false
	m.InFlight.Thinking = false
	m.InFlight.Canceling = false
	m.InFlight.AwaitingSettlement = false
	m.Progress.Compacting = false
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

// handleStreamClosed settles the projection when its runtime can no longer
// deliver lifecycle events. Lagged subscriptions take a separate resync path;
// every other close is terminal for this runtime generation.
func (m Model) handleStreamClosed(err error) (Model, tea.Cmd) {
	m.clearTurnCancellation()
	entryIf, _ := m.turnReducer().StreamClosed(time.Now())
	m.turnReducer().ClearActiveState(true)
	m.Picker.Approval = nil
	m.turnReducer().RestoreRuntimePhase(agent.PhaseClosed)

	message := "runtime event stream closed"
	if errors.Is(err, agent.ErrRuntimeClosed) {
		message = "runtime closed before the event stream settled"
	} else if err != nil {
		message = fmt.Sprintf("runtime event stream closed: %v", err)
	}
	notice, _ := session.EntrySystem("Error: "+message, time.Now())
	return m, tea.Sequence(
		m.terminalCommit().Entries(entryIf),
		m.terminalCommit().Entries(notice),
	)
}

func (m Model) handleSessionError(err error, awaitTerminal bool) (Model, tea.Cmd) {
	decision := DecideErrorSettlement(ErrorSettlementInput{
		Err:           err,
		AwaitTerminal: awaitTerminal,
	})
	var cmds []tea.Cmd
	entry, _ := session.EntrySystem(decision.EntryContent, time.Time{})
	cmds = append(cmds, m.terminalCommit().Entries(entry))

	m.turnReducer().FailTurn(decision.DisplayError, time.Now())

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

func (m Model) handleTurnStarted(msg session.TurnStart) (Model, tea.Cmd) {
	if !m.InFlight.Thinking {
		// Runtime-owned queued turns do not originate from submitComposer, so
		// advance the app request fence when their lifecycle starts.
		m.Model.TurnSubmitRequest++
	}
	if m.Model.turnCancellation == nil ||
		(msg.TurnToken != 0 && m.Model.turnCancellation.turnToken() != 0 &&
			m.Model.turnCancellation.turnToken() != msg.TurnToken) {
		state, _ := newTurnCancellationState(m.runtimeOperationContext())
		m.replaceTurnCancellation(state)
	}
	if msg.TurnToken != 0 {
		m.Model.turnCancellation.setToken(msg.TurnToken)
		m.Model.turnCancellation.markStarted()
	}
	m.turnReducer().StartTurn(msg.When(), time.Now())
	m.preserveCancellationProjection()
	return m, m.awaitSessionEvent()
}

func (m Model) handleTurnFinished(msg session.TurnEnd) (Model, tea.Cmd) {
	// TurnEnd closes one model/tool iteration, not the whole agent run. Keep
	// Thinking set across tool loops and queued follow-up boundaries. A final
	// response with no tool results can be projected idle eagerly; Settled
	// remains the runtime's authoritative terminal boundary and repairs any
	// projection race before the next command is accepted.
	terminalResponse := len(msg.ToolResults) == 0 &&
		len(m.InFlight.QueuedSteering) == 0 && len(m.InFlight.QueuedTurns) == 0
	var cmds []tea.Cmd
	var terminalFailure bool

	assistant, assistantCompleted, printAssistant := m.turnReducer().FinishPendingAssistant()
	if printAssistant {
		cmds = append(cmds, m.terminalCommit().Entries(assistant))
		if errText := assistantErrorText(assistant); errText != "" {
			terminalFailure = true
			notice, _ := session.EntrySystem("Error: "+errText, time.Now())
			cmds = append(cmds, m.terminalCommit().Entries(notice))
		}
	}
	if entry, ok := m.turnReducer().FinishTurnMode(assistantCompleted); ok {
		cmds = append(cmds, m.terminalCommit().Entries(entry))
	}
	m.turnReducer().RecordFinishedTurnSummary(time.Now())
	if terminalResponse || terminalFailure {
		m.turnReducer().StopThinking()
		m.turnReducer().MarkAwaitingSettlement()
		m.Progress.Mode = StateReady
		m.Progress.Status = ""
	}

	cmds = append(cmds, m.awaitSessionEvent())
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
	// Token usage rides on the durable assistant message (one per model call,
	// including tool-use turns). Also commit the assistant to mark the turn as
	// having an assistant response.
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
	if c.model.Model.runtimeRequestCancel != nil {
		c.model.Model.runtimeRequestCancel()
	}
	c.model.Model.runtimeRequestContext, c.model.Model.runtimeRequestCancel = context.WithCancel(
		c.model.runtimeOperationContext(),
	)
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
	if c.model.Model.runtimeRequestCancel != nil {
		c.model.Model.runtimeRequestCancel()
		c.model.Model.runtimeRequestCancel = nil
		c.model.Model.runtimeRequestContext = nil
	}
	if decision.ClearLocalStatus {
		c.model.progressReducer().clearLocalBusyStatus()
	}
	return true
}

func (c runtimeRequestController) clear() {
	decision := ClearRuntimeRequest()
	c.model.Model.RuntimeSwitchRequest = decision.Active
	if c.model.Model.runtimeRequestCancel != nil {
		c.model.Model.runtimeRequestCancel()
		c.model.Model.runtimeRequestCancel = nil
		c.model.Model.runtimeRequestContext = nil
	}
	if decision.ClearLocalStatus {
		c.model.progressReducer().clearLocalBusyStatus()
	}
}

func (m Model) persistCurrentSessionInfoCmd() tea.Cmd {
	// This command is scheduled after Prompt completion. TurnEnd is emitted
	// before the controller commits the durable turn, so refreshing here keeps
	// catalog metadata on the final committed leaf instead of publishing a
	// response-only predecessor.
	catalog := m.Model.SessionCatalog
	projectionReader, hasProjection := m.Model.Runner.(agent.SessionProjectionReader)
	reader, hasTree := m.Model.Runner.(agent.SessionReader)
	if !hasProjection && !hasTree {
		return nil
	}
	generation := m.Model.EventGeneration
	ctx := m.runtimeOperationContext()
	workdir := m.App.Workdir
	branch := m.App.Branch
	modelName := m.currentSessionModelName()
	return func() tea.Msg {
		var (
			id      string
			entries []session.Entry
			err     error
		)
		if hasProjection {
			var projection agent.SessionProjection
			projection, err = projectionReader.SessionProjection(ctx)
			if err == nil {
				id = strings.TrimSpace(projection.LeafID)
				entries = projection.Branch
			}
		} else {
			var tree agent.SessionTreeSnapshot
			tree, err = reader.SessionTree(ctx)
			if err == nil {
				id = strings.TrimSpace(tree.LeafID)
				entries, err = reader.SessionBranch(ctx)
			}
		}
		if err != nil {
			return runtimeLeafSnapshotMsg{
				generation: generation,
				err:        fmt.Errorf("load session projection for catalog: %w", err),
			}
		}
		if id == "" {
			return runtimeLeafSnapshotMsg{generation: generation}
		}
		var info *session.SessionInfoEntry
		if catalog != nil {
			candidate, ok := sessionInfoFromBranch(
				id, workdir, branch, modelName, entries, time.Now(),
			)
			if ok {
				info = &candidate
			}
		}
		return runtimeLeafSnapshotMsg{generation: generation, leafID: id, info: info}
	}
}

func sessionInfoFromBranch(
	id, workdir, branch, modelName string,
	entries []session.Entry,
	now time.Time,
) (session.SessionInfoEntry, bool) {
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
		EntryBase:   session.EntryBase{ID: id, Timestamp: now},
		Workdir:     workdir,
		Model:       modelName,
		Branch:      branch,
		Name:        truncateRunes(firstUser, 80),
		LastPreview: truncateRunes(lastUser, 120),
		UpdatedAt:   now,
	}, true
}

func (m Model) currentSessionModelName() string {
	provider := ""
	model := ""
	if m.Model.Info != nil {
		provider = strings.TrimSpace(m.Model.Info.Provider())
		model = strings.TrimSpace(m.Model.Info.Model())
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
