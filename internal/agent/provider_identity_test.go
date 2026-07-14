package agent

import (
	"context"
	"sync"
	"testing"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func TestHarnessProviderRequestsUseStableSessionIdentity(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	var mu sync.Mutex
	var requestIDs []string
	h := NewHarness(HarnessConfig{
		Session: sess,
		Store:   store,
		Model:   llm.Model{ID: "test"},
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			mu.Lock()
			requestIDs = append(requestIDs, req.SessionID)
			mu.Unlock()
			return &mockStream{chunks: []*llm.Chunk{{Content: "ok", StopReason: "stop"}}}, nil
		},
	})
	defer h.Close()

	stableID := sess.Meta().ID
	if stableID == "" {
		t.Fatal("session metadata has no stable ID")
	}
	if _, err := h.Prompt(context.Background(), "first"); err != nil {
		t.Fatal(err)
	}
	firstLeaf := store.GetLeafID()
	if _, err := h.Prompt(context.Background(), "second"); err != nil {
		t.Fatal(err)
	}
	secondLeaf := store.GetLeafID()
	if firstLeaf == secondLeaf {
		t.Fatalf("session leaf did not change across turns: %q", firstLeaf)
	}

	mu.Lock()
	defer mu.Unlock()
	if len(requestIDs) != 2 {
		t.Fatalf("request count = %d, want 2", len(requestIDs))
	}
	for i, requestID := range requestIDs {
		if requestID != stableID {
			t.Errorf("request %d SessionID = %q, want stable %q", i, requestID, stableID)
		}
		if requestID == firstLeaf || requestID == secondLeaf {
			t.Errorf("request %d used changing leaf ID %q", i, requestID)
		}
	}
}
