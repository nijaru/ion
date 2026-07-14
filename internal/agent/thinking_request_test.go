package agent

import (
	"context"
	"testing"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func TestHarnessProviderRequestsUseConfiguredThinkingLevel(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	var effort string
	h := NewHarness(HarnessConfig{
		Session:  sess,
		Store:    store,
		Model:    llm.Model{ID: "test"},
		Thinking: session.ThinkingXHigh,
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			effort = req.ReasoningEffort
			return &mockStream{chunks: []*llm.Chunk{{Content: "ok", StopReason: "stop"}}}, nil
		},
	})
	defer h.Close()

	if _, err := h.Prompt(context.Background(), "thinking"); err != nil {
		t.Fatal(err)
	}
	if effort != string(session.ThinkingXHigh) {
		t.Fatalf("request reasoning effort = %q, want %q", effort, session.ThinkingXHigh)
	}
}
