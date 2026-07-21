package agent

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/nijaru/ion/session"
)

func TestControllerCommandBoundaryAcceptsNilContext(t *testing.T) {
	store := newTestStore(t)
	h := NewController(ControllerConfig{
		Session: session.NewSession(store, 64),
		Store:   store,
	})
	defer h.Close()

	if err := h.SetThinking(nil, session.ThinkingHigh); err != nil {
		t.Fatalf("SetThinking with nil context: %v", err)
	}
}

type blockingLabelSession struct {
	session.Session
	started chan struct{}
	release chan struct{}
}

func (s *blockingLabelSession) GetLabel(context.Context, string) (string, error) {
	close(s.started)
	<-s.release
	return "label", nil
}

func TestControllerCommandWaitHonorsCancellation(t *testing.T) {
	store := newTestStore(t)
	base := session.NewSession(store, 64)
	sess := &blockingLabelSession{
		Session: base,
		started: make(chan struct{}),
		release: make(chan struct{}),
	}
	h := NewController(ControllerConfig{Session: sess, Store: store})

	ctx, cancel := context.WithTimeout(context.Background(), 25*time.Millisecond)
	defer cancel()
	result := make(chan error, 1)
	go func() {
		_, err := h.GetLabel(ctx, "missing")
		result <- err
	}()
	<-sess.started

	select {
	case err := <-result:
		if !errors.Is(err, context.DeadlineExceeded) {
			t.Fatalf("GetLabel error = %v, want deadline exceeded", err)
		}
	case <-time.After(time.Second):
		t.Fatal("GetLabel did not honor canceled command context")
	}

	close(sess.release)
	if err := h.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
}
