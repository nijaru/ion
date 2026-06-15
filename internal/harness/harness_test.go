package harness

import (
	"context"
	"testing"

	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/llm"
)

func TestHarness_New(t *testing.T) {
	a := agent.New(agent.AgentConfig{
		Model: llm.Model{ID: "test"},
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{
				{Content: "hello"},
			}}, nil
		},
	})

	h := New(Config{Agent: a})
	if h.Agent() != a {
		t.Fatal("expected agent to be set")
	}
	if h.Hooks() == nil {
		t.Fatal("expected hooks to be set")
	}
}

func TestHarness_Run(t *testing.T) {
	a := agent.New(agent.AgentConfig{
		Model: llm.Model{ID: "test"},
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{
				{Content: "hello"},
			}}, nil
		},
	})

	h := New(Config{Agent: a})

	messages, err := h.Run(context.Background(), []agent.AgentMessage{
		{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "hi"}}},
	})
	if err != nil {
		t.Fatalf("run: %v", err)
	}
	if len(messages) == 0 {
		t.Fatal("expected messages")
	}
}

func TestHarness_Hooks(t *testing.T) {
	a := agent.New(agent.AgentConfig{
		Model: llm.Model{ID: "test"},
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{
				{Content: "hello"},
			}}, nil
		},
	})

	hooks := NewHookDispatcher()
	var hookCalled bool
	hooks.On(BeforeAgentStart, func(ctx context.Context, event HookEvent) (HookResult, error) {
		hookCalled = true
		return HookResult{}, nil
	})

	h := New(Config{Agent: a, Hooks: hooks})

	_, err := h.Run(context.Background(), []agent.AgentMessage{
		{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "hi"}}},
	})
	if err != nil {
		t.Fatalf("run: %v", err)
	}
	if !hookCalled {
		t.Fatal("expected hook to be called")
	}
}

func TestHarness_HooksAbort(t *testing.T) {
	a := agent.New(agent.AgentConfig{
		Model: llm.Model{ID: "test"},
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{
				{Content: "hello"},
			}}, nil
		},
	})

	hooks := NewHookDispatcher()
	hooks.On(BeforeAgentStart, func(ctx context.Context, event HookEvent) (HookResult, error) {
		return HookResult{Abort: true, Reason: "test abort"}, nil
	})

	h := New(Config{Agent: a, Hooks: hooks})

	_, err := h.Run(context.Background(), []agent.AgentMessage{
		{Role: "user", Parts: []llm.ContentPart{{Type: llm.ContentPartText, Text: "hi"}}},
	})
	if err == nil {
		t.Fatal("expected error from abort")
	}
}

func TestHookDispatcher_Clear(t *testing.T) {
	d := NewHookDispatcher()
	d.On(BeforeAgentStart, func(ctx context.Context, event HookEvent) (HookResult, error) {
		return HookResult{}, nil
	})
	if !d.HasHandlers(BeforeAgentStart) {
		t.Fatal("expected handlers")
	}
	d.Clear(BeforeAgentStart)
	if d.HasHandlers(BeforeAgentStart) {
		t.Fatal("expected no handlers")
	}
}

func TestHookDispatcher_ClearAll(t *testing.T) {
	d := NewHookDispatcher()
	d.On(BeforeAgentStart, func(ctx context.Context, event HookEvent) (HookResult, error) {
		return HookResult{}, nil
	})
	d.On(AfterAgentRun, func(ctx context.Context, event HookEvent) (HookResult, error) {
		return HookResult{}, nil
	})
	d.ClearAll()
	if d.HasHandlers(BeforeAgentStart) || d.HasHandlers(AfterAgentRun) {
		t.Fatal("expected no handlers")
	}
}

// mockStream is a simple mock for testing.
type mockStream struct {
	chunks []*llm.Chunk
	idx    int
}

func (m *mockStream) Next() (*llm.Chunk, bool) {
	if m.idx >= len(m.chunks) {
		return nil, false
	}
	chunk := m.chunks[m.idx]
	m.idx++
	return chunk, true
}

func (m *mockStream) Err() error {
	return nil
}

func (m *mockStream) Close() error {
	return nil
}
