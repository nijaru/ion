package agent

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/nijaru/ion/session"
)

func TestControllerRoutesSessionTransportThroughCommands(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	if _, err := sess.AppendMessage(context.Background(), session.NewUserText("transport", time.Now())); err != nil {
		t.Fatal(err)
	}
	h := NewController(ControllerConfig{Session: sess, Store: store})
	defer h.Close()

	bundle, err := h.ExportSessionBundle(context.Background(), sess.GetLeafID())
	if err != nil {
		t.Fatalf("ExportSessionBundle: %v", err)
	}
	if len(bundle.Sessions) != 1 || len(bundle.Sessions[0].Events) == 0 {
		t.Fatalf("exported bundle = %#v, want one non-empty session", bundle)
	}
	bundle.RootSessionID = ""
	imported, err := h.ImportSessionBundle(context.Background(), bundle)
	if err != nil {
		t.Fatalf("ImportSessionBundle: %v", err)
	}
	if imported == "" {
		t.Fatal("ImportSessionBundle returned an empty leaf ID")
	}
}

type blockingLabelSession struct {
	session.Session
	started chan struct{}
	release chan struct{}
}

func (s *blockingLabelSession) GetLabel(ctx context.Context, _ string) (string, error) {
	close(s.started)
	select {
	case <-s.release:
		return "label", nil
	case <-ctx.Done():
		return "", ctx.Err()
	}
}

func TestControllerGetLabelHonorsRuntimeClose(t *testing.T) {
	store := newTestStore(t)
	base := session.NewSession(store, 64)
	sess := &blockingLabelSession{
		Session: base,
		started: make(chan struct{}),
		release: make(chan struct{}),
	}
	h := NewController(ControllerConfig{Session: sess, Store: store})

	result := make(chan error, 1)
	go func() {
		_, err := h.GetLabel(context.Background(), "missing")
		result <- err
	}()
	<-sess.started

	closed := make(chan error, 1)
	go func() { closed <- h.Close() }()
	select {
	case err := <-result:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("GetLabel error = %v, want context canceled", err)
		}
	case <-time.After(time.Second):
		t.Fatal("GetLabel did not observe runtime close")
	}
	select {
	case err := <-closed:
		if err != nil {
			t.Fatalf("Close: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("Close did not join GetLabel")
	}
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
