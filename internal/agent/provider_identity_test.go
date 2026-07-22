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
	h := NewController(ControllerConfig{
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

func TestHarnessAssistantMessagesPreserveResolvedProviderMetadata(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	h := NewController(ControllerConfig{
		Session: sess,
		Store:   store,
		Model: llm.Model{
			ID:       "model-identity",
			API:      "openai-completions",
			Provider: "provider-identity",
		},
		StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{{Content: "ok", StopReason: "stop"}}}, nil
		},
	})
	defer h.Close()

	events, err := collectWithSubscribe(t, h, "preserve identity")
	if err != nil {
		t.Fatalf("Prompt() error = %v", err)
	}
	streamed := false
	for _, event := range events {
		start, ok := event.(session.MessageStart)
		if !ok {
			continue
		}
		assistant, ok := start.Message.(*session.AssistantMessage)
		if !ok {
			continue
		}
		if assistant.API != "openai-completions" || assistant.Provider != "provider-identity" {
			t.Fatalf("streamed assistant metadata = API %q provider %q, want resolved model metadata", assistant.API, assistant.Provider)
		}
		streamed = true
		break
	}
	if !streamed {
		t.Fatal("no assistant MessageStart event")
	}

	entries, err := store.Entries(context.Background())
	if err != nil {
		t.Fatalf("read persisted entries: %v", err)
	}
	for _, entry := range entries {
		messageEntry, ok := entry.(*session.MessageEntry)
		if !ok {
			continue
		}
		assistant, ok := messageEntry.Message.(*session.AssistantMessage)
		if !ok {
			continue
		}
		if assistant.API != "openai-completions" || assistant.Provider != "provider-identity" {
			t.Fatalf("persisted assistant metadata = API %q provider %q, want resolved model metadata", assistant.API, assistant.Provider)
		}
		return
	}
	t.Fatal("no persisted assistant message")
}
