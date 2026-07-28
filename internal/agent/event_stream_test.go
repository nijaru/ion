package agent

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

type blockingModelChangeSession struct {
	session.Session
	started chan struct{}
	release chan struct{}
}

type blockingBranchSession struct {
	session.Session
	started chan struct{}
	release chan struct{}
}

func (s *blockingModelChangeSession) AppendModelChange(
	ctx context.Context,
	provider, modelID string,
) (string, error) {
	close(s.started)
	select {
	case <-s.release:
		return s.Session.AppendModelChange(ctx, provider, modelID)
	case <-ctx.Done():
		return "", ctx.Err()
	}
}

func TestEventStreamBroadcastsIndependentOrderedSubscriptions(t *testing.T) {
	h := NewController(ControllerConfig{Session: newTestSession(t)})
	defer h.Close()

	first, err := h.Subscribe(context.Background(), EventCursor{})
	if err != nil {
		t.Fatal(err)
	}
	second, err := h.Subscribe(context.Background(), EventCursor{})
	if err != nil {
		t.Fatal(err)
	}
	defer first.Close()
	defer second.Close()

	for _, event := range []session.Event{session.AgentStart{}, session.TurnStart{}, session.Settled{}} {
		h.emit(event)
	}
	for i := range 3 {
		left := receiveEvent(t, first)
		right := receiveEvent(t, second)
		if left.Stream != right.Stream || left.Sequence != right.Sequence {
			t.Fatalf("subscription %d identity mismatch: %#v vs %#v", i, left, right)
		}
		if got, want := left.Sequence, uint64(i+1); got != want {
			t.Fatalf("event %d sequence = %d, want %d", i, got, want)
		}
	}
}

func TestEventStreamDetachesSlowSubscriberAtBound(t *testing.T) {
	h := NewController(ControllerConfig{Session: newTestSession(t)})
	defer h.Close()

	sub, err := h.Subscribe(context.Background(), EventCursor{})
	if err != nil {
		t.Fatal(err)
	}
	for i := 0; i < eventSubscriberCapacity+1; i++ {
		h.emit(session.ProviderRetry{Attempt: i + 1})
	}
	if !errors.Is(sub.Err(), ErrSubscriptionLagged) {
		t.Fatalf("subscription error = %v, want ErrSubscriptionLagged", sub.Err())
	}
	count := 0
	for range sub.Events {
		count++
	}
	if count != eventSubscriberCapacity {
		t.Fatalf("delivered events = %d, want bounded prefix %d", count, eventSubscriberCapacity)
	}
}

func TestEventStreamResubscriptionRestoresPendingApproval(t *testing.T) {
	h := NewController(ControllerConfig{
		Session:             newTestSession(t),
		ApprovalMode:        ApprovalConfirm,
		ApprovalInteractive: true,
	})
	defer h.Close()

	first, err := h.Subscribe(context.Background(), EventCursor{})
	if err != nil {
		t.Fatal(err)
	}
	initial := first.Snapshot.Cursor
	requestDone := make(chan approvalOutcome, 1)
	go func() {
		requestDone <- h.approvals.Request(context.Background(), session.ApprovalRequest{
			ToolName:  "write",
			Operation: "write",
			Resource:  "config.toml",
			Paths:     []string{"config.toml"},
		})
	}()

	event := receiveEvent(t, first)
	request, ok := event.Event.(session.ApprovalRequest)
	if !ok {
		t.Fatalf("event = %T, want ApprovalRequest", event.Event)
	}
	first.Close()

	second, err := h.Subscribe(context.Background(), initial)
	if err != nil {
		t.Fatal(err)
	}
	if !second.Snapshot.Resynced {
		t.Fatal("resubscription did not mark snapshot as resynced")
	}
	if second.Snapshot.Phase != PhaseAwaitingApproval {
		t.Fatalf("resynced phase = %s, want awaiting approval", second.Snapshot.Phase)
	}
	if len(second.Snapshot.PendingApprovals) != 1 {
		t.Fatalf("pending approvals = %#v, want one request", second.Snapshot.PendingApprovals)
	}
	pending := second.Snapshot.PendingApprovals[0]
	if pending.ID != request.ID || pending.Resource != request.Resource ||
		len(pending.Paths) != 1 || pending.Paths[0] != "config.toml" {
		t.Fatalf("pending approval = %#v, want %#v", pending, request)
	}
	second.Close()

	if err := h.ResolveApproval(request.ID, session.ApprovalDeny); err != nil {
		t.Fatalf("resolve approval: %v", err)
	}
	select {
	case outcome := <-requestDone:
		if outcome.decision != session.ApprovalDeny {
			t.Fatalf("approval outcome = %q, want deny", outcome.decision)
		}
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for approval request")
	}

	third, err := h.Subscribe(context.Background(), second.Snapshot.Cursor)
	if err != nil {
		t.Fatal(err)
	}
	defer third.Close()
	if len(third.Snapshot.PendingApprovals) != 0 {
		t.Fatalf("resolved pending approvals = %#v, want none", third.Snapshot.PendingApprovals)
	}
}

func TestEventStreamExclusiveSnapshotSettlesAfterPersistence(t *testing.T) {
	base := newTestSession(t)
	blocking := &blockingModelChangeSession{
		Session: base,
		started: make(chan struct{}),
		release: make(chan struct{}),
	}
	h := NewController(ControllerConfig{
		Session: blocking,
		Model:   llm.Model{Provider: "old-provider", ID: "old-model"},
	})
	defer h.Close()

	initial, err := h.Subscribe(context.Background(), EventCursor{})
	if err != nil {
		t.Fatal(err)
	}
	defer initial.Close()

	setModelDone := make(chan error, 1)
	go func() {
		setModelDone <- h.SetModel(llm.Model{Provider: "new-provider", ID: "new-model"})
	}()
	select {
	case <-blocking.started:
	case <-time.After(time.Second):
		t.Fatal("model persistence did not start")
	}

	recovery, err := h.Subscribe(context.Background(), initial.Snapshot.Cursor)
	if err != nil {
		t.Fatal(err)
	}
	defer recovery.Close()
	if recovery.Snapshot.Phase != PhasePersisting {
		t.Fatalf("in-flight recovery phase = %s, want persisting", recovery.Snapshot.Phase)
	}

	close(blocking.release)
	select {
	case err := <-setModelDone:
		if err != nil {
			t.Fatalf("SetModel: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("SetModel did not finish")
	}

	var ready EventEnvelope
	for {
		select {
		case envelope := <-recovery.Events:
			if _, ok := envelope.Event.(session.RuntimeReady); ok {
				ready = envelope
				goto readyEvent
			}
		case <-time.After(time.Second):
			t.Fatal("recovery subscription did not receive RuntimeReady")
		}
	}

readyEvent:
	fresh, err := h.Subscribe(context.Background(), EventCursor{
		Stream: ready.Stream,
		Next:   ready.Sequence + 1,
	})
	if err != nil {
		t.Fatal(err)
	}
	defer fresh.Close()
	if fresh.Snapshot.Phase != PhaseReady {
		t.Fatalf("fresh snapshot phase = %s, want ready", fresh.Snapshot.Phase)
	}
	if fresh.Snapshot.Model.ID != "new-model" {
		t.Fatalf("fresh snapshot model = %q, want new-model", fresh.Snapshot.Model.ID)
	}
}

func TestCloseCancelsIdleModelPersistence(t *testing.T) {
	blocking := &blockingModelChangeSession{
		Session: newTestSession(t),
		started: make(chan struct{}),
		release: make(chan struct{}),
	}
	h := NewController(ControllerConfig{
		Session: blocking,
		Model:   llm.Model{Provider: "old-provider", ID: "old-model"},
	})

	setModelDone := make(chan error, 1)
	go func() {
		setModelDone <- h.SetModel(llm.Model{Provider: "new-provider", ID: "new-model"})
	}()
	select {
	case <-blocking.started:
	case <-time.After(time.Second):
		t.Fatal("model persistence did not start")
	}

	closeDone := make(chan error, 1)
	go func() { closeDone <- h.Close() }()
	select {
	case err := <-closeDone:
		if err != nil {
			t.Fatalf("Close: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("Close did not cancel idle model persistence")
	}
	select {
	case err := <-setModelDone:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("SetModel error = %v, want context cancellation", err)
		}
	case <-time.After(time.Second):
		t.Fatal("SetModel did not settle after Close")
	}
}

func (s *blockingBranchSession) BranchAt(ctx context.Context, leafID string) ([]session.Entry, error) {
	close(s.started)
	select {
	case <-s.release:
		return s.Session.BranchAt(ctx, leafID)
	case <-ctx.Done():
		return nil, ctx.Err()
	}
}

func TestCloseCancelsBlockedSubscription(t *testing.T) {
	blocking := &blockingBranchSession{
		Session: newTestSession(t),
		started: make(chan struct{}),
		release: make(chan struct{}),
	}
	h := NewController(ControllerConfig{Session: blocking})

	subscribeDone := make(chan error, 1)
	go func() {
		_, err := h.Subscribe(context.Background(), EventCursor{})
		subscribeDone <- err
	}()
	select {
	case <-blocking.started:
	case <-time.After(time.Second):
		t.Fatal("subscription branch read did not start")
	}

	closeDone := make(chan error, 1)
	go func() { closeDone <- h.Close() }()
	select {
	case err := <-closeDone:
		if err != nil {
			t.Fatalf("Close: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("Close did not cancel blocked subscription")
	}
	select {
	case err := <-subscribeDone:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("Subscribe error = %v, want context cancellation", err)
		}
	case <-time.After(time.Second):
		t.Fatal("Subscribe did not settle after Close")
	}
}

func TestEventStreamResubscriptionReturnsFreshSnapshot(t *testing.T) {
	h := NewController(ControllerConfig{Session: newTestSession(t)})
	defer h.Close()

	first, err := h.Subscribe(context.Background(), EventCursor{})
	if err != nil {
		t.Fatal(err)
	}
	initial := first.Snapshot.Cursor
	first.Close()
	h.emit(session.AgentStart{})

	second, err := h.Subscribe(context.Background(), initial)
	if err != nil {
		t.Fatal(err)
	}
	defer second.Close()
	if !second.Snapshot.Resynced {
		t.Fatal("resubscription did not mark snapshot as resynced")
	}
	if got := second.Snapshot.Cursor.Next; got != initial.Next+1 {
		t.Fatalf("snapshot next cursor = %d, want %d", got, initial.Next+1)
	}
}

func TestEventStreamCloseDoesNotBlockWithoutSubscribers(t *testing.T) {
	h := NewController(ControllerConfig{Session: newTestSession(t)})
	done := make(chan error, 1)
	go func() { done <- h.Close() }()
	select {
	case err := <-done:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(time.Second):
		t.Fatal("Close blocked without subscribers")
	}
}

func receiveEvent(t *testing.T, sub *EventSubscription) EventEnvelope {
	t.Helper()
	select {
	case event, ok := <-sub.Events:
		if !ok {
			t.Fatalf("subscription closed: %v", sub.Err())
		}
		return event
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for event")
		return EventEnvelope{}
	}
}
