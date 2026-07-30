package agent

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func TestHarnessCloseCancelsActiveProviderRequest(t *testing.T) {
	started := make(chan struct{})
	h := NewController(ControllerConfig{
		Session: newTestSession(t),
		Model:   llm.Model{ID: "test"},
		StreamFn: func(ctx context.Context, _ *llm.Request) (llm.Stream, error) {
			close(started)
			<-ctx.Done()
			return nil, ctx.Err()
		},
	})
	promptDone := make(chan error, 1)
	go func() {
		_, err := h.Prompt(context.Background(), "cancel me")
		promptDone <- err
	}()
	select {
	case <-started:
	case <-time.After(time.Second):
		t.Fatal("provider did not start")
	}

	closeDone := make(chan error, 1)
	go func() { closeDone <- h.Close() }()
	select {
	case err := <-closeDone:
		if err != nil {
			t.Fatalf("Close: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("Close did not cancel active provider")
	}
	select {
	case <-promptDone:
	case <-time.After(time.Second):
		t.Fatal("Prompt did not finish after Close cancellation")
	}
	if phase := h.currentPhase(); phase != PhaseClosed {
		t.Fatalf("phase after Close = %s, want closed", phase)
	}
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.runDone != nil || h.runCancel != nil || h.activeTurnID != "" {
		t.Fatalf(
			"turn state retained after Close: done=%v cancel=%v turn=%q",
			h.runDone != nil,
			h.runCancel != nil,
			h.activeTurnID,
		)
	}
}

func TestHarnessConcurrentCloseJoinsFirstCall(t *testing.T) {
	h := NewController(ControllerConfig{Session: newTestSession(t)})
	release := make(chan struct{})
	started := make(chan struct{})
	h.startOperation(func() {
		close(started)
		<-release
	})
	<-started

	firstDone := make(chan error, 1)
	go func() { firstDone <- h.Close() }()
	deadline := time.Now().Add(time.Second)
	for {
		h.mu.Lock()
		closed := h.closed
		h.mu.Unlock()
		if closed {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("first Close did not enter shutdown")
		}
		time.Sleep(time.Millisecond)
	}

	secondDone := make(chan error, 1)
	go func() { secondDone <- h.Close() }()
	select {
	case err := <-secondDone:
		t.Fatalf("second Close returned before first cleanup: %v", err)
	case <-time.After(20 * time.Millisecond):
	}

	close(release)
	for name, done := range map[string]chan error{"first": firstDone, "second": secondDone} {
		select {
		case err := <-done:
			if err != nil {
				t.Fatalf("%s Close: %v", name, err)
			}
		case <-time.After(time.Second):
			t.Fatalf("%s Close did not finish", name)
		}
	}
}

func TestHarnessCloseJoinsAuxiliaryOperation(t *testing.T) {
	h := NewController(ControllerConfig{Session: newTestSession(t)})
	finish, err := h.beginExclusive(PhasePersisting)
	if err != nil {
		t.Fatalf("begin exclusive: %v", err)
	}
	started := make(chan struct{})
	release := make(chan struct{})
	h.startOperation(func() {
		close(started)
		<-release
		finish()
	})
	<-started

	closed := make(chan error, 1)
	go func() { closed <- h.Close() }()
	select {
	case err := <-closed:
		t.Fatalf("Close returned before auxiliary operation released: %v", err)
	case <-time.After(20 * time.Millisecond):
	}

	close(release)
	select {
	case err := <-closed:
		if err != nil {
			t.Fatalf("Close: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("Close did not join auxiliary operation")
	}
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.phase != PhaseClosed || h.runDone != nil || h.runCancel != nil {
		t.Fatalf(
			"auxiliary state retained after Close: phase=%s done=%v cancel=%v",
			h.phase,
			h.runDone != nil,
			h.runCancel != nil,
		)
	}
}

type blockingShutdownSession struct {
	session.Session
	started   chan struct{}
	release   chan struct{}
	completed chan error
}

func (s *blockingShutdownSession) AppendSessionInfo(ctx context.Context, name string) (string, error) {
	close(s.started)
	select {
	case <-s.release:
		id, err := s.Session.AppendSessionInfo(ctx, name)
		s.completed <- err
		return id, err
	case <-ctx.Done():
		s.completed <- ctx.Err()
		return "", ctx.Err()
	}
}

func TestHarnessShutdownBoundsPendingFlushWait(t *testing.T) {
	store := newTestStore(t)
	base := session.NewSession(store, 64)
	sess := &blockingShutdownSession{
		Session:   base,
		started:   make(chan struct{}),
		release:   make(chan struct{}),
		completed: make(chan error, 1),
	}
	h := NewController(ControllerConfig{Session: sess, Store: store})

	h.mu.Lock()
	h.pending = []pendingWrite{{
		apply: func(ctx context.Context, s session.Session) error {
			_, err := s.AppendSessionInfo(ctx, "shutdown pending")
			return err
		},
	}}
	h.mu.Unlock()

	ctx, cancel := context.WithTimeout(context.Background(), 25*time.Millisecond)
	defer cancel()
	shutdownDone := make(chan error, 1)
	go func() { shutdownDone <- h.Shutdown(ctx) }()
	select {
	case <-sess.started:
	case <-time.After(time.Second):
		t.Fatal("pending flush did not start")
	}

	select {
	case err := <-shutdownDone:
		if !errors.Is(err, context.DeadlineExceeded) {
			t.Fatalf("Shutdown error = %v, want deadline exceeded", err)
		}
	case <-time.After(time.Second):
		t.Fatal("Shutdown did not honor pending-flush deadline")
	}

	close(sess.release)
	select {
	case err := <-sess.completed:
		if err != nil {
			t.Fatalf("pending flush: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("pending flush did not complete after shutdown returned")
	}
	if err := h.Close(); err != nil {
		t.Fatalf("Close after pending flush: %v", err)
	}
	entries, err := base.Branch(context.Background())
	if err != nil {
		t.Fatalf("read persisted shutdown entry: %v", err)
	}
	found := false
	for _, entry := range entries {
		if info, ok := entry.(*session.SessionInfoEntry); ok && info.Name == "shutdown pending" {
			found = true
			break
		}
	}
	if !found {
		t.Fatal("pending flush did not persist after caller timeout")
	}
}

func TestHarnessShutdownBoundsCloseAfterSuccessfulFlush(t *testing.T) {
	h := NewController(ControllerConfig{Session: newTestSession(t)})
	release := make(chan struct{})
	h.startOperation(func() { <-release })

	ctx, cancel := context.WithTimeout(context.Background(), 25*time.Millisecond)
	defer cancel()
	started := time.Now()
	err := h.Shutdown(ctx)
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("Shutdown error = %v, want deadline exceeded", err)
	}
	if elapsed := time.Since(started); elapsed > time.Second {
		t.Fatalf("Shutdown took %s after successful flush", elapsed)
	}

	close(release)
	if err := h.Close(); err != nil {
		t.Fatalf("Close after bounded shutdown: %v", err)
	}
}

func TestHarnessShutdownBoundsCloseAfterFlushQueueFailure(t *testing.T) {
	h := NewController(ControllerConfig{Session: newTestSession(t)})
	release := make(chan struct{})
	h.startOperation(func() { <-release })

	h.mu.Lock()
	h.runtimeBusy = true
	h.runtimeQueue = make([]runtimeRequest, runtimeOperationCapacity)
	h.mu.Unlock()

	ctx, cancel := context.WithTimeout(context.Background(), 25*time.Millisecond)
	defer cancel()
	started := time.Now()
	err := h.Shutdown(ctx)
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("Shutdown error = %v, want deadline exceeded", err)
	}
	if elapsed := time.Since(started); elapsed > time.Second {
		t.Fatalf("Shutdown took %s after flush queue failure", elapsed)
	}

	close(release)
	if err := h.Close(); err != nil {
		t.Fatalf("Close after bounded shutdown: %v", err)
	}
}

func TestHarnessShutdownHonorsDeadlineBeforeNonCooperativeProviderReturns(t *testing.T) {
	started := make(chan struct{})
	release := make(chan struct{})
	h := NewController(ControllerConfig{
		Session: newTestSession(t),
		Model:   llm.Model{ID: "test"},
		StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
			close(started)
			<-release
			return nil, errors.New("released")
		},
	})
	promptDone := make(chan error, 1)
	go func() {
		_, err := h.Prompt(context.Background(), "deadline")
		promptDone <- err
	}()
	select {
	case <-started:
	case <-time.After(time.Second):
		t.Fatal("provider did not start")
	}

	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Millisecond)
	defer cancel()
	startedShutdown := time.Now()
	err := h.Shutdown(ctx)
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("Shutdown error = %v, want deadline exceeded", err)
	}
	if elapsed := time.Since(startedShutdown); elapsed > time.Second {
		t.Fatalf("Shutdown took %s after deadline", elapsed)
	}

	close(release)
	select {
	case <-promptDone:
	case <-time.After(time.Second):
		t.Fatal("Prompt did not finish after provider release")
	}
	if err := h.Close(); err != nil {
		t.Fatalf("Close after release: %v", err)
	}
}
