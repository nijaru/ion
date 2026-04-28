package backend

import (
	"context"
	"errors"
	"testing"

	"github.com/nijaru/ion/internal/config"
)

func TestUnconfiguredSubmitTurnFailsSynchronouslyOnly(t *testing.T) {
	reason := errors.New("No provider configured")
	b := NewUnconfigured(&config.Config{}, reason)
	sess := b.Session()

	if err := sess.SubmitTurn(context.Background(), "hello"); !errors.Is(err, reason) {
		t.Fatalf("submit error = %v, want %v", err, reason)
	}

	select {
	case ev := <-sess.Events():
		t.Fatalf("unconfigured submit queued backend event %#v", ev)
	default:
	}
}
