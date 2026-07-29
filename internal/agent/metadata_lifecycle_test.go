package agent

import (
	"context"
	"sync"
	"testing"
	"time"

	"github.com/nijaru/ion/session"
)

type blockingMetadataSession struct {
	session.Session
	started chan struct{}
	release chan struct{}
}

type blockingNavigationSession struct {
	session.Session
	started     chan struct{}
	release     chan struct{}
	startedOnce sync.Once
}

func (s *blockingNavigationSession) GetEntry(ctx context.Context, id string) (session.Entry, error) {
	s.startedOnce.Do(func() { close(s.started) })
	select {
	case <-s.release:
	case <-ctx.Done():
		return nil, ctx.Err()
	}
	return s.Session.GetEntry(ctx, id)
}

func (s *blockingMetadataSession) wait(ctx context.Context) error {
	close(s.started)
	select {
	case <-s.release:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

func (s *blockingMetadataSession) AppendSessionInfo(ctx context.Context, name string) (string, error) {
	if err := s.wait(ctx); err != nil {
		return "", err
	}
	return s.Session.AppendSessionInfo(ctx, name)
}

func (s *blockingMetadataSession) AppendLabel(
	ctx context.Context,
	targetID, label string,
) (string, error) {
	if err := s.wait(ctx); err != nil {
		return "", err
	}
	return s.Session.AppendLabel(ctx, targetID, label)
}

func TestNavigationPublishesReadyBeforeReply(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t)
	base := session.NewSession(store, 64)
	oldLeafID, err := base.AppendMessage(ctx, session.NewUserText("old branch", time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	if _, err := base.AppendMessage(ctx, session.NewUserText("current branch", time.Now())); err != nil {
		t.Fatal(err)
	}
	sess := &blockingNavigationSession{
		Session: base,
		started: make(chan struct{}),
		release: make(chan struct{}),
	}
	h := NewController(ControllerConfig{Session: sess, Store: store})
	defer h.Close()

	navigationResult := make(chan error, 1)
	go func() {
		_, err := h.NavigateTree(ctx, oldLeafID, NavigateOptions{})
		navigationResult <- err
	}()
	<-sess.started

	sub, err := h.Subscribe(ctx, EventCursor{})
	if err != nil {
		t.Fatal(err)
	}
	defer sub.Close()
	if sub.Snapshot.Phase != PhaseRecovering {
		t.Fatalf("navigation snapshot phase = %s, want recovering", sub.Snapshot.Phase)
	}

	close(sess.release)
	if err := <-navigationResult; err != nil {
		t.Fatalf("navigation: %v", err)
	}
	select {
	case event := <-sub.Events:
		if _, ok := event.Event.(session.RuntimeReady); !ok {
			t.Fatalf("navigation completion event = %T, want RuntimeReady", event.Event)
		}
	case <-time.After(time.Second):
		t.Fatal("navigation did not publish RuntimeReady")
	}
}

func TestNavigationHonorsRuntimeClose(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t)
	base := session.NewSession(store, 64)
	oldLeafID, err := base.AppendMessage(ctx, session.NewUserText("old branch", time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	if _, err := base.AppendMessage(ctx, session.NewUserText("current branch", time.Now())); err != nil {
		t.Fatal(err)
	}
	sess := &blockingNavigationSession{
		Session: base,
		started: make(chan struct{}),
		release: make(chan struct{}),
	}
	h := NewController(ControllerConfig{Session: sess, Store: store})

	navigationResult := make(chan error, 1)
	go func() {
		_, err := h.NavigateTree(ctx, oldLeafID, NavigateOptions{})
		navigationResult <- err
	}()
	<-sess.started

	closed := make(chan error, 1)
	go func() { closed <- h.Close() }()
	select {
	case err := <-navigationResult:
		if err == nil {
			t.Fatal("navigation succeeded after runtime close")
		}
	case <-time.After(time.Second):
		t.Fatal("navigation did not observe runtime close")
	}
	select {
	case err := <-closed:
		if err != nil {
			t.Fatalf("close: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("runtime close did not join navigation")
	}
}

func TestNavigationReservesBeforeMetadataCommand(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t)
	base := session.NewSession(store, 64)
	oldLeafID, err := base.AppendMessage(ctx, session.NewUserText("old branch", time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	currentLeafID, err := base.AppendMessage(ctx, session.NewUserText("current branch", time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	sess := &blockingNavigationSession{
		Session: base,
		started: make(chan struct{}),
		release: make(chan struct{}),
	}
	h := NewController(ControllerConfig{Session: sess, Store: store})
	defer h.Close()

	navigationResult := make(chan error, 1)
	go func() {
		_, err := h.NavigateTree(ctx, oldLeafID, NavigateOptions{})
		navigationResult <- err
	}()
	<-sess.started

	if _, err := h.AppendSessionInfo(ctx, currentLeafID, "must wait"); err == nil {
		t.Fatal("metadata append succeeded while navigation was active")
	}
	close(sess.release)
	if err := <-navigationResult; err != nil {
		t.Fatalf("navigation: %v", err)
	}
}

func TestSessionMetadataExcludesConcurrentNavigation(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t)
	base := session.NewSession(store, 64)
	oldLeafID, err := base.AppendMessage(ctx, session.NewUserText("old branch", time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	currentLeafID, err := base.AppendMessage(ctx, session.NewUserText("current branch", time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	sess := &blockingMetadataSession{
		Session: base,
		started: make(chan struct{}),
		release: make(chan struct{}),
	}
	h := NewController(ControllerConfig{Session: sess, Store: store})
	defer h.Close()

	metadataResult := make(chan error, 1)
	go func() {
		_, err := h.AppendSessionInfo(ctx, currentLeafID, "current name")
		metadataResult <- err
	}()
	<-sess.started

	if _, err := h.NavigateTree(ctx, oldLeafID, NavigateOptions{}); err == nil {
		t.Fatal("navigation succeeded while metadata append was active")
	}
	close(sess.release)
	if err := <-metadataResult; err != nil {
		t.Fatalf("metadata append: %v", err)
	}
}

func TestBranchLabelFollowsMetadataLeaves(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	leafID, err := sess.AppendMessage(ctx, session.NewUserText("branch", time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	h := NewController(ControllerConfig{Session: sess, Store: store})
	defer h.Close()

	labelLeafID, err := h.AppendLabel(ctx, leafID, leafID, "release")
	if err != nil {
		t.Fatalf("append label: %v", err)
	}
	if got, err := h.GetBranchLabel(ctx, labelLeafID); err != nil || got != "release" {
		t.Fatalf("branch label after label append = %q, %v", got, err)
	}

	nameLeafID, err := h.AppendSessionInfo(ctx, labelLeafID, "release candidate")
	if err != nil {
		t.Fatalf("append session info: %v", err)
	}
	if got, err := h.GetBranchLabel(ctx, nameLeafID); err != nil || got != "release" {
		t.Fatalf("branch label after name append = %q, %v", got, err)
	}
}

func TestSessionMetadataAppendHonorsRuntimeClose(t *testing.T) {
	for _, operation := range []string{"name", "label"} {
		t.Run(operation, func(t *testing.T) {
			ctx := context.Background()
			store := newTestStore(t)
			base := session.NewSession(store, 64)
			leafID, err := base.AppendMessage(ctx, session.NewUserText("branch", time.Now()))
			if err != nil {
				t.Fatal(err)
			}
			sess := &blockingMetadataSession{
				Session: base,
				started: make(chan struct{}),
				release: make(chan struct{}),
			}
			h := NewController(ControllerConfig{Session: sess, Store: store})
			result := make(chan error, 1)
			go func() {
				var err error
				if operation == "name" {
					_, err = h.AppendSessionInfo(ctx, leafID, "blocked")
				} else {
					_, err = h.AppendLabel(ctx, leafID, leafID, "blocked")
				}
				result <- err
			}()
			<-sess.started

			closed := make(chan error, 1)
			go func() { closed <- h.Close() }()
			select {
			case err := <-result:
				if err == nil {
					t.Fatal("metadata append succeeded after runtime close")
				}
			case <-time.After(time.Second):
				t.Fatal("metadata append did not observe runtime close")
			}
			select {
			case err := <-closed:
				if err != nil {
					t.Fatalf("close: %v", err)
				}
			case <-time.After(time.Second):
				t.Fatal("runtime close did not join metadata append")
			}
		})
	}
}

func TestSessionMetadataRejectsStaleExpectedLeaf(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	oldLeafID, err := sess.AppendMessage(ctx, session.NewUserText("old branch", time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	if _, err := sess.AppendMessage(ctx, session.NewUserText("new branch", time.Now())); err != nil {
		t.Fatal(err)
	}

	h := NewController(ControllerConfig{Session: sess, Store: store})
	defer h.Close()

	if _, err := h.AppendSessionInfo(ctx, oldLeafID, "stale name"); err == nil {
		t.Fatal("stale session name append succeeded")
	}
	if _, err := h.AppendLabel(ctx, oldLeafID, oldLeafID, "stale label"); err == nil {
		t.Fatal("stale label append succeeded")
	}

	entries, err := sess.Branch(ctx)
	if err != nil {
		t.Fatal(err)
	}
	for _, entry := range entries {
		switch entry := entry.(type) {
		case *session.SessionInfoEntry:
			if entry.Name == "stale name" {
				t.Fatal("stale session name was persisted")
			}
		case *session.LabelEntry:
			if entry.Label == "stale label" {
				t.Fatal("stale label was persisted")
			}
		}
	}
}
