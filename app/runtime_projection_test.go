package app

import (
	"context"
	"errors"
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

	snapshot := agent.RuntimeSnapshot{
		SessionID: "session-resumed",
		Phase:     agent.PhaseStreaming,
		Model: llm.Model{
			Provider: "anthropic",
			ID:       "claude-test",
		},
		Thinking:    session.ThinkingHigh,
		ActiveTools: []string{"read", "edit"},
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

	runner := &stubRunner{}
	model.Model.Runner = runner
	staleQueued, cmd := model.handleQueuedTurn(queuedTurnMsg{
		generation: 1,
		text:       "old queued turn",
	})
	if cmd != nil {
		t.Fatal("stale queued turn returned a command")
	}
	if len(runner.promptTexts) != 0 {
		t.Fatalf("stale queued turn submitted prompts %#v", runner.promptTexts)
	}
	if got := staleQueued.Input.Composer.Value(); got != "new draft" {
		t.Fatalf("stale queued turn changed composer to %q", got)
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
			t.Fatalf("stale catalog cancellation handling = (handled=%v, cmd=%v, error=%q), want ignored", handled, cmd != nil, next.Progress.LastError)
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
