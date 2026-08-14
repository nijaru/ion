package agent

import (
	"context"
	"path/filepath"
	"testing"

	"github.com/nijaru/ion/session"
)

func TestControllerTurnRecoveryListsAndDiscardsInterruptedTurn(t *testing.T) {
	ctx := context.Background()
	dbPath := filepath.Join(t.TempDir(), "ion.db")
	store, err := session.NewSQLiteStore(dbPath, "turn-recovery")
	if err != nil {
		t.Fatal(err)
	}
	turn, err := store.BeginTurn(ctx, "interrupted-turn", "draft survives restart", nil, "context-1")
	if err != nil {
		t.Fatal(err)
	}
	if err := store.Close(); err != nil {
		t.Fatal(err)
	}

	reopened, err := session.NewSQLiteStore(dbPath, "turn-recovery")
	if err != nil {
		t.Fatal(err)
	}
	defer reopened.Close()
	h := NewController(ControllerConfig{
		Session: session.NewSession(reopened, 64),
		Store:   reopened,
		Durable: reopened,
	})
	defer h.Close()

	turns, err := h.InterruptedTurns(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(turns) != 1 || turns[0].ID != turn.ID || turns[0].State != session.TurnInterrupted {
		t.Fatalf("interrupted turns = %#v", turns)
	}

	settled, err := h.AbortInterruptedTurn(ctx, turn.ID, "user discarded interrupted turn")
	if err != nil {
		t.Fatal(err)
	}
	if settled.State != session.TurnAborted || settled.Error != "user discarded interrupted turn" {
		t.Fatalf("settled turn = %#v", settled)
	}
	turns, err = h.InterruptedTurns(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(turns) != 0 {
		t.Fatalf("interrupted turns after discard = %#v", turns)
	}
}

func TestControllerTurnRecoveryRejectsCommittedTurn(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t)
	turn, err := store.BeginTurn(ctx, "committed-turn", "already saved", nil, "context-1")
	if err != nil {
		t.Fatal(err)
	}
	if err := store.CommitTurn(ctx, turn.ID); err != nil {
		t.Fatal(err)
	}
	h := NewController(ControllerConfig{
		Session: session.NewSession(store, 64),
		Store:   store,
		Durable: store,
	})
	defer h.Close()

	if _, err := h.AbortInterruptedTurn(ctx, turn.ID, "must reject"); err == nil {
		t.Fatal("committed turn discard unexpectedly succeeded")
	}
}
