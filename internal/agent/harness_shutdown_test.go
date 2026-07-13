package agent

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
)

func TestHarnessCloseCancelsActiveProviderRequest(t *testing.T) {
	started := make(chan struct{})
	h := NewHarness(HarnessConfig{
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
}

func TestHarnessShutdownHonorsDeadlineBeforeNonCooperativeProviderReturns(t *testing.T) {
	started := make(chan struct{})
	release := make(chan struct{})
	h := NewHarness(HarnessConfig{
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
