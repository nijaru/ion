package app

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

type recordingForkRunner struct {
	*stubRunner
	forkContext   context.Context
	forkParentID  string
	forkSessionID string
}

func (r *recordingForkRunner) ForkSession(ctx context.Context, parentID string) (string, error) {
	r.forkContext = ctx
	r.forkParentID = parentID
	return r.forkSessionID, nil
}

type cancelAwareSessionCatalog struct {
	started     chan struct{}
	listStarted chan struct{}
	listContext context.Context
}

type recordingSessionCatalog struct {
	updates []session.SessionInfoEntry
}

func (c *recordingSessionCatalog) ListSessions(context.Context, string) ([]session.SessionInfoEntry, error) {
	return nil, nil
}

func (c *recordingSessionCatalog) GetSessionInfo(context.Context, string) (session.SessionInfoEntry, error) {
	return session.SessionInfoEntry{}, nil
}

func (c *recordingSessionCatalog) UpdateSession(_ context.Context, info session.SessionInfoEntry) error {
	c.updates = append(c.updates, info)
	return nil
}

func (c *cancelAwareSessionCatalog) ListSessions(ctx context.Context, _ string) ([]session.SessionInfoEntry, error) {
	c.listContext = ctx
	if c.listStarted == nil {
		return nil, nil
	}
	close(c.listStarted)
	<-ctx.Done()
	return nil, ctx.Err()
}

func (c *cancelAwareSessionCatalog) GetSessionInfo(context.Context, string) (session.SessionInfoEntry, error) {
	return session.SessionInfoEntry{}, nil
}

func (c *cancelAwareSessionCatalog) UpdateSession(ctx context.Context, _ session.SessionInfoEntry) error {
	close(c.started)
	<-ctx.Done()
	return ctx.Err()
}

func TestApplyAgentRuntimeSnapshotRehydratesCompleteProjection(t *testing.T) {
	model := readyModel(t)
	model.InFlight.Thinking = true
	model.InFlight.QueuedSteering = []string{"stale steer"}
	model.InFlight.QueuedTurns = []string{"stale follow-up"}
	model.Picker.Approval = &approvalPromptState{
		request:     session.ApprovalRequest{ID: "approval-1"},
		resolving:   true,
		resolvingID: "approval-1",
	}

	snapshot := agent.RuntimeSnapshot{
		SessionID: "session-resumed",
		Phase:     agent.PhaseStreaming,
		Model: llm.Model{
			Provider: "anthropic",
			ID:       "claude-test",
		},
		Thinking:    session.ThinkingHigh,
		ActiveTools: []string{"read", "edit"},
		PendingApprovals: []session.ApprovalRequest{{
			ID:       "approval-1",
			ToolName: "write",
			Resource: "config.toml",
			Paths:    []string{"config.toml"},
		}},
		Queues: agent.QueueSnapshot{
			Steer: []session.Message{
				&session.UserMessage{Content: []session.Content{session.TextContent{Text: "steer me"}}},
			},
			FollowUp: []session.Message{
				&session.UserMessage{Content: []session.Content{session.TextContent{Text: "follow up"}}},
			},
			NextTurn: []session.Message{
				&session.UserMessage{Content: []session.Content{session.TextContent{Text: "next turn"}}},
			},
		},
	}

	model.applyAgentRuntimeSnapshot(snapshot)

	if model.Model.Runtime.SessionID != "session-resumed" ||
		model.Model.Runtime.Provider != "anthropic" ||
		model.Model.Runtime.Model != "claude-test" ||
		model.Model.Runtime.Reasoning != "high" {
		t.Fatalf("runtime projection = %#v", model.Model.Runtime)
	}
	if !model.InFlight.Thinking || model.Progress.Mode != StateStreaming ||
		model.Progress.Status != "Streaming..." {
		t.Fatalf("turn projection = %#v progress=%#v", model.InFlight, model.Progress)
	}
	if got, want := model.InFlight.QueuedSteering, []string{"steer me"}; !equalStrings(got, want) {
		t.Fatalf("steering queue = %#v, want %#v", got, want)
	}
	if got, want := model.InFlight.QueuedTurns, []string{"follow up", "next turn"}; !equalStrings(got, want) {
		t.Fatalf("turn queue = %#v, want %#v", got, want)
	}
	if !equalStrings(model.Model.ActiveTools, []string{"read", "edit"}) {
		t.Fatalf("active tools = %#v", model.Model.ActiveTools)
	}
	if model.Picker.Approval == nil || model.Picker.Approval.request.ID != "approval-1" ||
		model.Picker.Approval.request.Resource != "config.toml" || !model.Picker.Approval.resolving {
		t.Fatalf("approval projection = %#v, want approval-1 still resolving", model.Picker.Approval)
	}

	resolved := snapshot
	resolved.PendingApprovals = nil
	model.applyAgentRuntimeSnapshot(resolved)
	if model.Picker.Approval != nil {
		t.Fatalf("resolved approval remained after snapshot: %#v", model.Picker.Approval)
	}
}

func TestAuthoritativeRuntimeSnapshotClearsStaleSelections(t *testing.T) {
	model := readyModel(t)
	model.Model.Runtime = Snapshot{
		SessionID:    "old-session",
		Provider:     "old-provider",
		Model:        "old-model",
		Reasoning:    "high",
		Materialized: true,
	}
	model.Progress.ReasoningEffort = "high"

	model.applyAgentRuntimeSnapshot(agent.RuntimeSnapshot{
		SessionID: "current-session",
		Phase:     agent.PhaseReady,
		Model:     llm.Model{ID: "current-model"},
	})

	if model.Model.Runtime.SessionID != "current-session" ||
		model.Model.Runtime.Provider != "" || model.Model.Runtime.Model != "current-model" {
		t.Fatalf("runtime selection = %#v, want provider cleared and current model", model.Model.Runtime)
	}
	if want := normalizeThinkingValue(""); model.Model.Runtime.Reasoning != want ||
		model.Progress.ReasoningEffort != want {
		t.Fatalf(
			"reasoning projection = runtime=%q progress=%q, want %q",
			model.Model.Runtime.Reasoning,
			model.Progress.ReasoningEffort,
			want,
		)
	}
}

func TestRuntimeReadyClearsExclusivePersistenceProjection(t *testing.T) {
	model := readyModel(t)
	model.InFlight.Thinking = true
	model.Progress.Mode = StateWorking
	model.Progress.Status = "Persisting..."

	next, cmd := model.handleSessionEvent(session.RuntimeReady{})
	if cmd == nil {
		t.Fatal("RuntimeReady did not re-arm the event stream")
	}
	if next.InFlight.Thinking || next.Progress.Compacting ||
		next.Progress.Mode != StateReady || next.Progress.Status != "" {
		t.Fatalf("RuntimeReady projection = in-flight=%#v progress=%#v, want ready", next.InFlight, next.Progress)
	}
}

func TestRuntimeReadyPreservesQueuedTurnHandoff(t *testing.T) {
	model := readyModel(t)
	model.InFlight.QueuedTurns = []string{"queued"}
	model.InFlight.Thinking = true
	model.Progress.Mode = StateWorking

	next, cmd := model.handleSessionEvent(session.RuntimeReady{})
	if cmd == nil {
		t.Fatal("RuntimeReady did not re-arm the event stream")
	}
	if !next.InFlight.Thinking || len(next.InFlight.QueuedTurns) != 1 || next.InFlight.QueuedTurns[0] != "queued" {
		t.Fatalf("RuntimeReady cleared queued handoff: %#v", next.InFlight)
	}
}

func TestRuntimeReadyDoesNotClearActiveCompaction(t *testing.T) {
	model := readyModel(t)
	model.Progress.Compacting = true
	model.Progress.Mode = StateWorking
	model.Progress.Status = "Compacting context..."

	next, cmd := model.handleSessionEvent(session.RuntimeReady{})
	if cmd == nil {
		t.Fatal("RuntimeReady did not re-arm the event stream")
	}
	if !next.Progress.Compacting || next.Progress.Mode != StateWorking ||
		next.Progress.Status != "Compacting context..." {
		t.Fatalf("RuntimeReady cleared active compaction: %#v", next.Progress)
	}
}

func TestRuntimeReadyDoesNotClearAcceptedTurnProjection(t *testing.T) {
	model := readyModel(t)
	state, _ := newTurnCancellationState(context.Background())
	state.setToken(9)
	state.markStarted()
	model.Model.turnCancellation = state
	model.InFlight.Thinking = true
	model.InFlight.QueuedTurns = []string{"queued"}

	next, cmd := model.handleSessionEvent(session.RuntimeReady{})
	if cmd == nil {
		t.Fatal("RuntimeReady did not re-arm the event stream")
	}
	if !next.InFlight.Thinking || len(next.InFlight.QueuedTurns) != 1 || next.InFlight.QueuedTurns[0] != "queued" {
		t.Fatalf("RuntimeReady cleared accepted turn projection: %#v", next.InFlight)
	}
}

func TestPersistingResyncDoesNotStickCompactingAfterSettled(t *testing.T) {
	model := readyModel(t)
	model.InFlight.Thinking = true

	model.applyAgentRuntimeSnapshot(agent.RuntimeSnapshot{Phase: agent.PhasePersisting})
	if !model.InFlight.Thinking || model.Progress.Compacting || !model.localCommandBusy() {
		t.Fatalf(
			"persisting snapshot = in-flight=%#v progress=%#v, want busy non-compacting projection",
			model.InFlight,
			model.Progress,
		)
	}
	if model.Progress.Status != "Persisting..." {
		t.Fatalf("persisting status = %q, want Persisting...", model.Progress.Status)
	}

	settled, _ := model.handleSettled(session.Settled{})
	if settled.Progress.Compacting || settled.localCommandBusy() {
		t.Fatalf("settled projection = in-flight=%#v progress=%#v, want idle", settled.InFlight, settled.Progress)
	}
}

func TestQueueUpdateReplacesProjectedRuntimeQueues(t *testing.T) {
	model := readyModel(t)
	model.InFlight.QueuedSteering = []string{"stale steer"}
	model.InFlight.QueuedTurns = []string{"stale turn"}

	next, cmd := model.handleQueueUpdate(session.QueueUpdate{
		Steer: []session.Message{
			session.NewUserText("redirect", time.Now()),
		},
		NextTurn: []session.Message{
			session.NewUserText("continue later", time.Now()),
		},
	})
	if cmd == nil {
		t.Fatal("queue update should re-arm the runtime event stream")
	}
	if got, want := next.InFlight.QueuedSteering, []string{"redirect"}; !equalStrings(got, want) {
		t.Fatalf("steering projection = %#v, want %#v", got, want)
	}
	if got, want := next.InFlight.QueuedTurns, []string{"continue later"}; !equalStrings(got, want) {
		t.Fatalf("turn projection = %#v, want %#v", got, want)
	}
}

func TestStaleSubscriptionSnapshotCannotOverwriteNavigationProjection(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 1
	model.Model.TreeNavigationRequest = 2
	model.Model.LeafID = "selected-leaf"

	next, cmd, handled := model.dispatchTurnControllerMessage(runtimeSubscriptionMsg{
		generation:            1,
		treeNavigationRequest: 1,
		subscription: &agent.EventSubscription{
			Snapshot: agent.RuntimeSnapshot{LeafID: "stale-leaf", Phase: agent.PhaseReady},
			Events:   make(chan agent.EventEnvelope),
		},
	})
	if !handled {
		t.Fatal("stale subscription result was not handled")
	}
	if cmd == nil {
		t.Fatal("stale subscription did not request a replacement snapshot")
	}
	if next.Model.LeafID != "selected-leaf" {
		t.Fatalf("stale subscription changed selected leaf to %q", next.Model.LeafID)
	}
}

func TestSubscriptionRecoveryDefersUntilTreeNavigationSettles(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 1
	model.Model.TreeNavigationRequest = 2
	model.Model.EventSubscriptionState.generation = 1
	model.Picker.BranchSummary = &branchSummaryPromptState{navigating: true}

	next, cmd, handled := model.dispatchTurnControllerMessage(runtimeSubscriptionMsg{
		generation:            1,
		treeNavigationRequest: 1,
		subscription: &agent.EventSubscription{
			Snapshot: agent.RuntimeSnapshot{LeafID: "stale-leaf", Phase: agent.PhaseRecovering},
			Events:   make(chan agent.EventEnvelope),
		},
	})
	if !handled {
		t.Fatal("stale subscription result was not handled")
	}
	if cmd != nil {
		t.Fatal("subscription recovery started while navigation was active")
	}

	next, cmd = next.handleTreePickerMove(treePickerMoveMsg{
		generation: 1,
		requestID:  2,
		leafID:     "selected-leaf",
	})
	if cmd == nil {
		t.Fatal("navigation completion did not rearm subscription recovery")
	}
	if next.Model.TreeNavigationRequest != 3 || next.Model.LeafID != "selected-leaf" {
		t.Fatalf(
			"navigation projection = leaf %q/epoch %d, want selected-leaf/3",
			next.Model.LeafID,
			next.Model.TreeNavigationRequest,
		)
	}
}

func TestSubscriptionRecoveryDefersCurrentNavigationSnapshot(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 1
	model.Model.TreeNavigationRequest = 2
	model.Model.LeafID = "selected-leaf"
	model.Model.EventSubscriptionState.generation = 1
	model.Picker.BranchSummary = &branchSummaryPromptState{navigating: true}

	next, cmd, handled := model.dispatchTurnControllerMessage(runtimeSubscriptionMsg{
		generation:            1,
		treeNavigationRequest: 2,
		subscription: &agent.EventSubscription{
			Snapshot: agent.RuntimeSnapshot{LeafID: "stale-leaf", Phase: agent.PhaseRecovering},
			Events:   make(chan agent.EventEnvelope),
		},
	})
	if !handled {
		t.Fatal("current navigation subscription result was not handled")
	}
	if cmd != nil {
		t.Fatal("subscription recovery started while navigation was active")
	}
	if next.Model.LeafID != "selected-leaf" || next.Progress.Mode != StateReady {
		t.Fatalf("navigation snapshot changed projection: leaf=%q progress=%#v", next.Model.LeafID, next.Progress)
	}
	if state := next.Model.EventSubscriptionState; state == nil || !state.retryAfterNavigation {
		t.Fatalf("subscription state = %#v, want retry after navigation", state)
	}

	next, cmd = next.handleTreePickerMove(treePickerMoveMsg{
		generation: 1,
		requestID:  2,
		leafID:     "selected-leaf",
	})
	if cmd == nil {
		t.Fatal("navigation completion did not rearm deferred subscription recovery")
	}
	if next.Model.TreeNavigationRequest != 3 || next.Model.LeafID != "selected-leaf" {
		t.Fatalf(
			"navigation projection = leaf %q/epoch %d, want selected-leaf/3",
			next.Model.LeafID,
			next.Model.TreeNavigationRequest,
		)
	}
}

func TestInitialSubscriptionRehydratesPendingApproval(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 1
	sub := &agent.EventSubscription{
		Snapshot: agent.RuntimeSnapshot{
			Phase: agent.PhaseAwaitingApproval,
			PendingApprovals: []session.ApprovalRequest{{
				ID:       "approval-1",
				ToolName: "write",
				Resource: "config.toml",
			}},
		},
		Events: make(chan agent.EventEnvelope),
	}

	next, cmd, handled := model.dispatchTurnControllerMessage(runtimeSubscriptionMsg{
		generation:   1,
		subscription: sub,
	})
	if !handled {
		t.Fatal("initial subscription was not handled")
	}
	if cmd == nil {
		t.Fatal("initial subscription did not start event consumption")
	}
	if next.Picker.Approval == nil || next.Picker.Approval.request.ID != "approval-1" ||
		next.Progress.Mode != StateWorking || next.Progress.Status != "Awaiting approval..." {
		t.Fatalf("initial approval projection = %#v progress=%#v", next.Picker.Approval, next.Progress)
	}
}

func TestSessionEventCursorAdvancesAfterAcceptedSequence(t *testing.T) {
	model := readyModel(t)
	stream := agent.EventStreamID{1}
	model.Model.EventGeneration = 7
	model.Model.EventCursor = agent.EventCursor{Stream: stream, Next: 12}
	model.Model.EventSubscription = &agent.EventSubscription{}

	next, cmd, handled := model.dispatchTurnControllerMessage(sessionEventMsg{
		generation: 7,
		cursor:     agent.EventCursor{Stream: stream, Next: 12},
		event:      session.SavePoint{},
	})
	if !handled {
		t.Fatal("session event was not handled")
	}
	if got, want := next.Model.EventCursor.Next, uint64(13); got != want {
		t.Fatalf("event cursor next = %d, want %d", got, want)
	}
	if cmd == nil {
		t.Fatal("accepted event did not re-arm subscription")
	}
}

func TestAwaitSessionEventDeduplicatesPendingSubscription(t *testing.T) {
	model := readyModel(t)

	first := model.awaitSessionEvent()
	if first == nil {
		t.Fatal("first subscription request is nil")
	}
	second := model.awaitSessionEvent()
	if second != nil {
		t.Fatal("duplicate subscription request was started while the first was pending")
	}
	if state := model.Model.EventSubscriptionState; state == nil || !state.pending {
		t.Fatalf("subscription state = %#v, want pending", state)
	}
}

func TestAwaitSessionEventDeduplicatesActiveReader(t *testing.T) {
	model := readyModel(t)
	model.Model.EventSubscription = &agent.EventSubscription{
		Events: make(chan agent.EventEnvelope),
	}

	first := model.awaitSessionEvent()
	if first == nil {
		t.Fatal("first event reader is nil")
	}
	second := model.awaitSessionEvent()
	if second != nil {
		t.Fatal("duplicate event reader was started while the first was pending")
	}
	if state := model.Model.EventSubscriptionState; state == nil || !state.readerBusy {
		t.Fatalf("subscription state = %#v, want active reader", state)
	}
}

func TestClosedSubscriptionResultSettlesBusyTurnAndClearsApproval(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 1
	model.InFlight.Thinking = true
	model.Progress.Mode = StateStreaming
	model.Progress.Status = "Streaming..."
	model.Picker.Approval = &approvalPromptState{
		request: session.ApprovalRequest{ID: "approval-1"},
	}

	next, cmd, handled := model.dispatchTurnControllerMessage(runtimeSubscriptionMsg{
		generation: 1,
		err:        agent.ErrRuntimeClosed,
	})
	if !handled {
		t.Fatal("closed subscription result was not handled")
	}
	if cmd == nil || next.Progress.Mode != StateError || next.Picker.Approval != nil {
		t.Fatalf(
			"closed subscription projection = %#v/%#v, want terminal error without approval",
			next.Progress,
			next.Picker.Approval,
		)
	}
}

func TestStreamClosedSettlesBusyTurnAndSurfacesError(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 1
	model.Model.EventSubscription = &agent.EventSubscription{Events: make(chan agent.EventEnvelope)}
	model.InFlight.Thinking = true
	model.Progress.Mode = StateStreaming
	model.Progress.Status = "Streaming..."
	model.Picker.Approval = &approvalPromptState{
		request: session.ApprovalRequest{ID: "stale-approval"},
	}

	next, cmd, handled := model.dispatchTurnControllerMessage(streamClosedMsg{
		generation: 1,
		err:        agent.ErrRuntimeClosed,
	})
	if !handled {
		t.Fatal("stream close was not handled")
	}
	if cmd == nil {
		t.Fatal("stream close did not emit a terminal notice")
	}
	if next.Model.EventSubscription != nil {
		t.Fatal("closed runtime subscription was retained")
	}
	if next.InFlight.Thinking || next.localCommandBusy() {
		t.Fatalf("closed runtime left busy projection: in-flight=%#v progress=%#v", next.InFlight, next.Progress)
	}
	if next.Picker.Approval != nil {
		t.Fatalf("closed runtime retained approval prompt: %#v", next.Picker.Approval)
	}
	if next.Progress.Mode != StateError || next.Progress.Status != "Runtime closed" {
		t.Fatalf("closed runtime progress = %#v, want terminal error", next.Progress)
	}
}

func TestStaleEventReaderCannotAdvanceRuntimeCursor(t *testing.T) {
	model := readyModel(t)
	stream := agent.EventStreamID{1}
	model.Model.EventGeneration = 3
	model.Model.EventCursor = agent.EventCursor{Stream: stream, Next: 7}
	model.Model.EventSubscription = &agent.EventSubscription{}
	state := model.Model.EventSubscriptionState
	state.generation = 3
	state.reader = 2
	state.readerBusy = true

	next, cmd, handled := model.dispatchTurnControllerMessage(sessionEventMsg{
		generation: 3,
		reader:     1,
		cursor:     agent.EventCursor{Stream: stream, Next: 7},
		event:      session.SavePoint{},
	})
	if !handled {
		t.Fatal("stale event reader was not handled")
	}
	if cmd != nil {
		t.Fatal("stale event reader started a replacement command")
	}
	if got := next.Model.EventCursor.Next; got != 7 {
		t.Fatalf("cursor next = %d, want unchanged 7", got)
	}
	if !state.readerBusy {
		t.Fatal("current event reader was marked idle by stale result")
	}
}

func TestStaleTurnCommandsCannotMutateNewRuntimeGeneration(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 2
	model.Input.Composer.SetValue("new draft")
	model.Input.Images = []session.ImageContent{{Data: []byte("new"), MimeType: "text/plain"}}

	staleSubmit, cmd := model.handleTurnSubmitResult(turnSubmitResultMsg{
		generation: 1,
		draft:      "old draft",
		images:     []session.ImageContent{{Data: []byte("old")}},
		err:        errors.New("old runtime failed"),
	})
	if cmd != nil {
		t.Fatal("stale submit result returned a command")
	}
	if got := staleSubmit.Input.Composer.Value(); got != "new draft" {
		t.Fatalf("stale submit changed composer to %q", got)
	}
	if got := string(staleSubmit.Input.Images[0].Data); got != "new" {
		t.Fatalf("stale submit changed images to %q", got)
	}

	staleBusy, cmd := model.handleBusyInputResult(busyInputResultMsg{
		generation: 1,
		action:     "follow-up",
		text:       "old follow-up",
		images:     []session.ImageContent{{Data: []byte("old")}},
		err:        errors.New("old runtime failed"),
	})
	if cmd != nil {
		t.Fatal("stale busy-input result returned a command")
	}
	if got := staleBusy.Input.Composer.Value(); got != "new draft" {
		t.Fatalf("stale busy-input result changed composer to %q", got)
	}

	staleCancel, cmd := model.handleTurnCancelResult(turnCancelResultMsg{
		generation: 1,
		err:        errors.New("old runtime failed"),
	})
	if cmd != nil {
		t.Fatal("stale cancel result returned a command")
	}
	if got := staleCancel.Input.Composer.Value(); got != "new draft" {
		t.Fatalf("stale cancel result changed composer to %q", got)
	}
}

func TestStaleTurnSubmitResultCannotMutateNewTurn(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 1
	model.Model.TurnSubmitRequest = 2
	model.InFlight.Thinking = true
	model.Input.Composer.SetValue("new draft")

	next, cmd := model.handleTurnSubmitResult(turnSubmitResultMsg{
		generation: 1,
		requestID:  1,
		draft:      "old draft",
		err:        errors.New("old turn failed"),
	})
	if cmd != nil {
		t.Fatal("stale submit result returned a command")
	}
	if !next.InFlight.Thinking {
		t.Fatal("stale submit result cleared the newer turn")
	}
	if got := next.Input.Composer.Value(); got != "new draft" {
		t.Fatalf("stale submit result changed composer to %q", got)
	}
}

func TestIntermediateTurnEndKeepsRuntimeBusyAcrossToolLoop(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 1
	model.Model.TurnSubmitRequest = 1
	model.InFlight.Thinking = true
	model.InFlight.AgentCommitted = true

	afterEnd, _ := model.handleTurnFinished(session.TurnEnd{
		ToolResults: []session.ToolResultMessage{{ToolCallID: "tool-loop"}},
	})
	if !afterEnd.InFlight.Thinking || !afterEnd.localCommandBusy() {
		t.Fatalf("intermediate TurnEnd marked runtime idle: in-flight=%#v", afterEnd.InFlight)
	}

	afterNextStart, _ := afterEnd.handleTurnStarted(session.TurnStart{})
	if got, want := afterNextStart.Model.TurnSubmitRequest, uint64(1); got != want {
		t.Fatalf("tool-loop TurnStart advanced submit fence to %d, want %d", got, want)
	}
}

func TestTerminalTurnEndBlocksRuntimeReplacementUntilSettled(t *testing.T) {
	model := readyModel(t)
	model.InFlight.Thinking = true
	model.InFlight.AgentCommitted = true

	afterEnd, _ := model.handleTurnFinished(session.TurnEnd{})
	if afterEnd.InFlight.Thinking {
		t.Fatal("terminal TurnEnd should still allow the eager output projection")
	}
	if !afterEnd.InFlight.AwaitingSettlement || !afterEnd.localCommandBusy() {
		t.Fatalf("terminal TurnEnd lost the settlement barrier: in-flight=%#v", afterEnd.InFlight)
	}

	_, cmd := afterEnd.handleCommand("/model model-b")
	if cmd == nil {
		t.Fatal("model replacement was accepted before Settled")
	}
	if err := localErrorFromMsg(t, cmd()); !strings.Contains(err.Error(), "Finish or cancel the current turn") {
		t.Fatalf("error = %v, want settlement barrier", err)
	}

	settled, _ := afterEnd.handleSettled(session.Settled{})
	if settled.InFlight.AwaitingSettlement || settled.localCommandBusy() {
		t.Fatalf("Settled retained the settlement barrier: in-flight=%#v", settled.InFlight)
	}
}

func TestSuccessfulTurnResultAppliesAfterQueuedTurnStarts(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 1
	model.Model.TurnSubmitRequest = 2
	model.InFlight.Thinking = true
	model.Input.Composer.SetValue("new draft")

	next, _ := model.handleTurnSubmitResult(turnSubmitResultMsg{
		generation: 1,
		requestID:  1,
		text:       "completed old turn",
	})
	if len(next.Input.History) != 1 || next.Input.History[0] != "completed old turn" {
		t.Fatalf("history = %#v, want completed old turn", next.Input.History)
	}
	if !next.InFlight.Thinking {
		t.Fatal("successful old result cleared the queued turn")
	}
	if got := next.Input.Composer.Value(); got != "new draft" {
		t.Fatalf("successful old result changed composer to %q", got)
	}
}

func TestSettledTurnResultCannotRestoreDraftBeforeQueuedTurn(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 1
	model.Model.TurnSubmitRequest = 1
	model.InFlight.Thinking = true

	settled, _ := model.handleSettled(session.Settled{NextTurnCount: 1})
	settled.Input.Composer.SetValue("")
	stale, cmd := settled.handleTurnSubmitResult(turnSubmitResultMsg{
		generation: 1,
		requestID:  1,
		draft:      "old draft",
		err:        errors.New("old turn failed"),
	})
	if cmd != nil {
		t.Fatal("settled stale result returned a command")
	}
	if got := stale.Input.Composer.Value(); got != "" {
		t.Fatalf("settled stale result restored draft %q", got)
	}

	started, _ := stale.handleTurnStarted(session.TurnStart{})
	if got, want := started.Model.TurnSubmitRequest, uint64(2); got != want {
		t.Fatalf("queued turn request fence = %d, want %d", got, want)
	}
	started.Input.Composer.SetValue("new draft")
	newer, cmd := started.handleTurnSubmitResult(turnSubmitResultMsg{
		generation: 1,
		requestID:  1,
		draft:      "old draft",
		err:        errors.New("old turn failed"),
	})
	if cmd != nil {
		t.Fatal("queued-turn stale result returned a command")
	}
	if !newer.InFlight.Thinking || newer.Input.Composer.Value() != "new draft" {
		t.Fatalf(
			"queued-turn stale result mutated current turn: thinking=%v draft=%q",
			newer.InFlight.Thinking,
			newer.Input.Composer.Value(),
		)
	}
}

func TestSubmitTurnCommandCarriesRequestIdentity(t *testing.T) {
	model := readyModel(t)
	model.Model.Runner = &stubRunner{}
	model.Input.Composer.SetValue("hello")

	next, cmd := model.submitComposer()
	if cmd == nil {
		t.Fatal("submit command = nil")
	}
	if next.Model.TurnSubmitRequest != 1 {
		t.Fatalf("turn submit request = %d, want 1", next.Model.TurnSubmitRequest)
	}
	result, ok := cmd().(turnSubmitResultMsg)
	if !ok {
		t.Fatalf("submit result = %T, want turnSubmitResultMsg", result)
	}
	if result.requestID != next.Model.TurnSubmitRequest {
		t.Fatalf("result request ID = %d, want %d", result.requestID, next.Model.TurnSubmitRequest)
	}
}

func TestStaleSessionCostCannotRenderNewRuntime(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 2
	model.App.PrintedTranscript = false

	next, cmd, handled := model.dispatchAppControlMessage(sessionCostMsg{
		generation: 1,
		notice:     "old runtime cost",
	})
	if !handled {
		t.Fatal("stale session cost was not handled")
	}
	if cmd != nil {
		t.Fatal("stale session cost returned a command")
	}
	if next.App.PrintedTranscript {
		t.Fatal("stale session cost rendered into the new runtime")
	}
}

func TestStaleSessionUsageCannotOverwriteNavigatedBranch(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 1
	model.Model.TreeNavigationRequest = 2
	model.Progress.TokensSent = 3
	model.Progress.TokensReceived = 4
	model.Progress.TotalCost = 0.5

	next, cmd, handled := model.dispatchAppControlMessage(sessionUsageLoadedMsg{
		generation:            1,
		treeNavigationRequest: 1,
		input:                 90,
		output:                80,
		cost:                  12.5,
	})
	if !handled {
		t.Fatal("stale session usage was not handled")
	}
	if cmd != nil {
		t.Fatal("stale session usage returned a command")
	}
	if next.Progress.TokensSent != 3 || next.Progress.TokensReceived != 4 || next.Progress.TotalCost != 0.5 {
		t.Fatalf("stale session usage changed progress: %#v", next.Progress)
	}
}

func TestStaleBranchSessionCommandsCannotRenderAfterNavigation(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 1
	model.Model.TreeNavigationRequest = 2
	model.App.PrintedTranscript = false

	next, cmd, handled := model.dispatchAppControlMessage(sessionCostMsg{
		generation:            1,
		treeNavigationRequest: 1,
		notice:                "old branch cost",
	})
	if !handled {
		t.Fatal("stale session cost was not handled")
	}
	if cmd != nil || next.App.PrintedTranscript {
		t.Fatalf("stale session cost = (cmd=%v, printed=%v), want ignored", cmd != nil, next.App.PrintedTranscript)
	}

	next, cmd = next.handleLocalEntries(localEntriesMsg{
		generation:            1,
		treeNavigationRequest: 1,
		entries:               []session.Entry{systemEntry("old branch session")},
	})
	if cmd != nil || next.App.PrintedTranscript {
		t.Fatalf("stale session info = (cmd=%v, printed=%v), want ignored", cmd != nil, next.App.PrintedTranscript)
	}
}

func TestStaleSessionInfoCannotRenderNewRuntime(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 2
	model.App.PrintedTranscript = false

	next, cmd := model.handleLocalEntries(localEntriesMsg{
		generation: 1,
		entries:    []session.Entry{systemEntry("old runtime session")},
	})
	if cmd != nil {
		t.Fatal("stale session info returned a command")
	}
	if next.App.PrintedTranscript {
		t.Fatal("stale session info rendered into the new runtime")
	}

	next, cmd = model.handleLocalEntries(localEntriesMsg{
		generation: 1,
		err:        errors.New("old runtime session read failed"),
	})
	if cmd != nil {
		t.Fatal("stale session info error returned a command")
	}
	if next.App.PrintedTranscript {
		t.Fatal("stale session info error rendered into the new runtime")
	}
}

func TestStaleSessionForkCannotResumeNewRuntime(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 2
	model.App.PrintedTranscript = false

	next, cmd := model.handleSessionForked(sessionForkedMsg{
		generation: 1,
		sessionID:  "old-fork",
	})
	if cmd != nil {
		t.Fatal("stale session fork returned a command")
	}
	if next.App.PrintedTranscript {
		t.Fatal("stale session fork resumed into the new runtime")
	}

	next, cmd = model.handleSessionForked(sessionForkedMsg{
		generation: 1,
		err:        errors.New("old fork failed"),
	})
	if cmd != nil {
		t.Fatal("stale session fork error returned a command")
	}
	if next.Progress.Status != model.Progress.Status {
		t.Fatalf("stale session fork error changed status to %q", next.Progress.Status)
	}
}

func TestSessionForkUsesRuntimeOperationContext(t *testing.T) {
	model := readyModel(t)
	runner := &recordingForkRunner{
		stubRunner:    &stubRunner{},
		forkSessionID: "forked",
	}
	model.Model.Runner = runner
	generation := model.Model.EventGeneration

	next, cmd := model.forkSessionFromPicker("parent")
	if cmd == nil {
		t.Fatal("session fork did not return a command")
	}
	if next.Picker.Session != nil {
		t.Fatal("session picker remained open while fork was running")
	}

	result := cmd()
	msg, ok := result.(sessionForkedMsg)
	if !ok {
		t.Fatalf("fork command returned %T, want sessionForkedMsg", result)
	}
	if msg.generation != generation || msg.sessionID != "forked" {
		t.Fatalf("fork result = %#v, want generation %d and forked session", msg, generation)
	}
	if runner.forkContext != model.Model.runtimeContext {
		t.Fatal("session fork did not receive the runtime operation context")
	}
	if runner.forkParentID != "parent" {
		t.Fatalf("fork parent ID = %q, want parent", runner.forkParentID)
	}
}

func TestCopyRejectsProjectionFromChangedLeafBeforeClipboardWrite(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 1
	model.Model.TreeNavigationRequest = 2
	model.Model.LeafID = "leaf-a"
	model.Model.Runner = &projectionTestRunner{
		stubRunner: &stubRunner{},
		projection: agent.SessionProjection{
			LeafID: "leaf-b",
			Branch: []session.Entry{agentMsgEntry("branch b response")},
		},
	}

	_, cmd := model.copyLastResponse()
	if cmd == nil {
		t.Fatal("copy command is nil")
	}
	msg, ok := cmd().(sessionCopiedMsg)
	if !ok {
		t.Fatalf("copy result = %T, want sessionCopiedMsg", cmd())
	}
	if msg.err == nil || !strings.Contains(msg.err.Error(), "active session leaf changed") {
		t.Fatalf("copy result = %#v, want changed-leaf error", msg)
	}
	if msg.generation != 1 || msg.treeNavigationRequest != 2 {
		t.Fatalf("copy fence = generation %d/navigation %d, want 1/2", msg.generation, msg.treeNavigationRequest)
	}
}

func TestStaleClipboardResultCannotRenderNewRuntime(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 2
	model.App.PrintedTranscript = false

	next, cmd := model.handleSessionCopied(sessionCopiedMsg{
		generation: 1,
	})
	if cmd != nil {
		t.Fatal("stale clipboard result returned a command")
	}
	if next.App.PrintedTranscript {
		t.Fatal("stale clipboard result rendered into the new runtime")
	}

	next, cmd = model.handleSessionCopied(sessionCopiedMsg{
		generation: 1,
		err:        errors.New("old clipboard failure"),
	})
	if cmd != nil {
		t.Fatal("stale clipboard error returned a command")
	}
	if next.App.PrintedTranscript {
		t.Fatal("stale clipboard error rendered into the new runtime")
	}
}

func TestStaleSessionImportAndCloneCannotResumeNewRuntime(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 2
	model.App.PrintedTranscript = false

	imported, cmd := model.handleSessionImported(sessionImportedMsg{
		generation: 1,
		sessionID:  "old-import",
		filename:   "old.json",
	})
	if cmd != nil {
		t.Fatal("stale import returned a resume command")
	}
	if imported.App.PrintedTranscript {
		t.Fatal("stale import rendered into the new runtime")
	}

	cloned, cmd := model.handleSessionCloned(sessionClonedMsg{
		generation:   1,
		newSessionID: "old-clone",
	})
	if cmd != nil {
		t.Fatal("stale clone returned a resume command")
	}
	if cloned.App.PrintedTranscript {
		t.Fatal("stale clone rendered into the new runtime")
	}

	failedImport, cmd := model.handleSessionImported(sessionImportedMsg{
		generation: 1,
		err:        errors.New("old import failed"),
	})
	if cmd != nil {
		t.Fatal("stale import error returned a command")
	}
	if failedImport.App.PrintedTranscript {
		t.Fatal("stale import error rendered into the new runtime")
	}
}

func TestSessionInfoProjectionCarriesNavigationFence(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 4
	model.Model.TreeNavigationRequest = 7
	model.Model.Runner = &projectionTestRunner{
		stubRunner: &stubRunner{},
		projection: agent.SessionProjection{LeafID: "leaf-7"},
	}

	msg, ok := model.persistCurrentSessionInfoCmd()().(runtimeLeafSnapshotMsg)
	if !ok {
		t.Fatalf("projection message = %T, want runtimeLeafSnapshotMsg", model.persistCurrentSessionInfoCmd()())
	}
	if msg.generation != 4 || msg.treeNavigationRequest != 7 || msg.leafID != "leaf-7" {
		t.Fatalf("projection message = %#v, want generation 4/navigation 7/leaf-7", msg)
	}
}

func TestStaleSameRuntimeLeafSnapshotCannotOverwriteCurrentProjection(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration = 1
	model.Model.TreeNavigationRequest = 2
	model.Model.LeafID = "selected-leaf"

	next, cmd, handled := model.dispatchAppControlMessage(runtimeLeafSnapshotMsg{
		generation:            1,
		treeNavigationRequest: 1,
		leafID:                "stale-leaf",
	})
	if !handled {
		t.Fatal("stale leaf snapshot was not handled")
	}
	if cmd == nil {
		t.Fatal("stale leaf snapshot did not request a fresh projection")
	}
	if next.Model.LeafID != "selected-leaf" {
		t.Fatalf("stale leaf snapshot changed selected leaf to %q", next.Model.LeafID)
	}
}

func TestStaleLeafSnapshotStillPersistsItsCatalogMetadata(t *testing.T) {
	model := readyModel(t)
	catalog := &recordingSessionCatalog{}
	model.Model.EventGeneration = 1
	model.Model.TreeNavigationRequest = 2
	model.Model.LeafID = "selected-leaf"
	model.Model.SessionCatalog = catalog

	info := session.SessionInfoEntry{EntryBase: session.EntryBase{ID: "stale-leaf"}}
	next, cmd, handled := model.dispatchAppControlMessage(runtimeLeafSnapshotMsg{
		generation:            1,
		treeNavigationRequest: 1,
		leafID:                "stale-leaf",
		info:                  &info,
	})
	if !handled {
		t.Fatal("stale leaf snapshot was not handled")
	}
	if cmd == nil {
		t.Fatal("stale leaf snapshot dropped its catalog update")
	}
	result, ok := cmd().(runtimeCatalogUpdateMsg)
	if !ok || result.err != nil {
		t.Fatalf("catalog update result = %#v, want successful update", result)
	}
	if len(catalog.updates) != 1 || catalog.updates[0].ID() != "stale-leaf" {
		t.Fatalf("catalog updates = %#v, want stale-leaf metadata", catalog.updates)
	}
	if next.Model.LeafID != "selected-leaf" {
		t.Fatalf("stale catalog projection changed selected leaf to %q", next.Model.LeafID)
	}
}

func TestStaleCatalogProjectionWriteIsCanceledOnRuntimeSwitch(t *testing.T) {
	model := readyModel(t)
	catalog := &cancelAwareSessionCatalog{started: make(chan struct{})}
	model.Model.SessionCatalog = catalog
	model.Model.EventGeneration = 1

	_, cmd, handled := model.dispatchAppControlMessage(runtimeLeafSnapshotMsg{
		generation: 1,
		leafID:     "leaf-1",
		info: &session.SessionInfoEntry{
			EntryBase: session.EntryBase{ID: "leaf-1"},
			Model:     "old/model",
		},
	})
	if !handled || cmd == nil {
		t.Fatalf("catalog projection dispatch = (handled=%v, cmd=%v), want asynchronous command", handled, cmd != nil)
	}

	result := make(chan any, 1)
	go func() { result <- cmd() }()
	select {
	case <-catalog.started:
	case <-time.After(time.Second):
		t.Fatal("catalog update did not start")
	}

	model.Model.RuntimeSwitchRequest = 1
	model.applyRuntimeSwitched(runtimeSwitchedMsg{
		switchID: 1,
		runtime:  Accepted{},
	})

	select {
	case msg := <-result:
		update, ok := msg.(runtimeCatalogUpdateMsg)
		if !ok || update.generation != 1 || !errors.Is(update.err, context.Canceled) {
			t.Fatalf("catalog update result = %#v, want generation-1 canceled update", msg)
		}
		next, cmd, handled := model.dispatchAppControlMessage(msg)
		if !handled || cmd != nil || next.Progress.LastError != "" {
			t.Fatalf(
				"stale catalog cancellation handling = (handled=%v, cmd=%v, error=%q), want ignored",
				handled,
				cmd != nil,
				next.Progress.LastError,
			)
		}
	case <-time.After(time.Second):
		t.Fatal("stale catalog update ignored runtime cancellation")
	}
}

func TestSessionPickerUsesCancelableRuntimeContext(t *testing.T) {
	model := readyModel(t)
	catalog := &cancelAwareSessionCatalog{listStarted: make(chan struct{})}
	model.Model.SessionCatalog = catalog

	updated, cmd := model.openSessionPicker()
	if cmd == nil {
		t.Fatal("session picker returned no load command")
	}
	expectedContext := updated.runtimeOperationContext()
	resultCh := make(chan any, 1)
	go func() { resultCh <- cmd() }()
	select {
	case <-catalog.listStarted:
	case <-time.After(time.Second):
		t.Fatal("session picker list did not start")
	}
	if catalog.listContext != expectedContext {
		t.Fatalf("session picker context = %v, want accepted runtime context", catalog.listContext)
	}

	updated.rotateRuntimeContext()
	result, ok := (<-resultCh).(sessionPickerLoadedMsg)
	if !ok || !errors.Is(result.err, context.Canceled) {
		t.Fatalf("canceled session picker result = %#v, want context cancellation", result)
	}
}

func equalStrings(got, want []string) bool {
	if len(got) != len(want) {
		return false
	}
	for i := range got {
		if got[i] != want[i] {
			return false
		}
	}
	return true
}
