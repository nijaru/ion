package agent

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/nijaru/ion/session"
)

func TestControllerSessionProjectionCapabilities(t *testing.T) {
	store := newTestStore(t)
	controller := NewController(ControllerConfig{
		Session: session.NewSession(store, 64),
		Store:   store,
	})
	t.Cleanup(func() {
		if err := controller.Close(); err != nil {
			t.Errorf("close controller: %v", err)
		}
	})

	ctx := t.Context()
	if err := controller.AppendMessage(ctx, session.NewUserText("hello", time.Now())); err != nil {
		t.Fatalf("append user message: %v", err)
	}

	branch, err := controller.SessionBranch(ctx)
	if err != nil {
		t.Fatalf("session branch: %v", err)
	}
	if len(branch) != 1 || session.EntryText(branch[0]) != "hello" {
		t.Fatalf("branch = %#v, want one hello entry", branch)
	}

	tree, err := controller.SessionTree(ctx)
	if err != nil {
		t.Fatalf("session tree: %v", err)
	}
	if tree.LeafID == "" || len(tree.Entries) != 1 || tree.Entries[0].ID() != tree.LeafID {
		t.Fatalf("tree = %#v, want one entry selected as leaf", tree)
	}
	if controller.SessionID() != store.Meta().ID {
		t.Fatalf("session id = %q, want %q", controller.SessionID(), store.Meta().ID)
	}

	info := session.SessionInfoEntry{
		EntryBase:   session.EntryBase{ID: controller.SessionID(), Timestamp: time.Now()},
		Workdir:     "/tmp/ion-projection",
		Model:       "test/model",
		Name:        "hello",
		LastPreview: "hello",
		UpdatedAt:   time.Now(),
	}
	if err := controller.UpdateSession(ctx, info); err != nil {
		t.Fatalf("update session catalog: %v", err)
	}
	got, err := controller.GetSessionInfo(ctx, controller.SessionID())
	if err != nil {
		t.Fatalf("get session catalog: %v", err)
	}
	if got.Model != info.Model || got.LastPreview != info.LastPreview {
		t.Fatalf("catalog entry = %#v, want %#v", got, info)
	}
	sessions, err := controller.ListSessions(ctx, info.Workdir)
	if err != nil {
		t.Fatalf("list sessions: %v", err)
	}
	if len(sessions) != 1 || sessions[0].ID() != controller.SessionID() {
		t.Fatalf("sessions = %#v, want current session", sessions)
	}

	if err := controller.AddInput(ctx, info.Workdir, "hello"); err != nil {
		t.Fatalf("add input history: %v", err)
	}
	inputs, err := controller.GetInputs(ctx, info.Workdir, 10)
	if err != nil {
		t.Fatalf("get input history: %v", err)
	}
	if len(inputs) != 1 || inputs[0] != "hello" {
		t.Fatalf("inputs = %#v, want hello", inputs)
	}
}

func TestControllerSessionProjectionRejectsClosedAndCanceledQueries(t *testing.T) {
	store := newTestStore(t)
	controller := NewController(ControllerConfig{
		Session: session.NewSession(store, 64),
		Store:   store,
	})

	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	if _, err := controller.SessionBranch(ctx); !errors.Is(err, context.Canceled) {
		t.Fatalf("canceled branch error = %v, want context canceled", err)
	}

	if err := controller.Close(); err != nil {
		t.Fatalf("close controller: %v", err)
	}
	if _, err := controller.SessionTree(t.Context()); !errors.Is(err, ErrRuntimeClosed) {
		t.Fatalf("closed tree error = %v, want runtime closed", err)
	}
	if err := controller.AddInput(t.Context(), "/tmp", "late"); !errors.Is(err, ErrRuntimeClosed) {
		t.Fatalf("closed input history error = %v, want runtime closed", err)
	}
}
