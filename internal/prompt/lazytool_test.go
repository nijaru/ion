package prompt

import (
	"context"
	"fmt"
	"strings"
	"testing"

	"github.com/go-json-experiment/json"

	"github.com/nijaru/ion/internal/llm"
	"github.com/nijaru/ion/internal/storage/session"
	"github.com/nijaru/ion/internal/tool"
)

// mockTool is a minimal Tool implementation for tests.
type mockTool struct{ name string }

func (m *mockTool) Spec() llm.Spec {
	return llm.Spec{Name: m.name, Description: "desc of " + m.name}
}
func (m *mockTool) Execute(_ context.Context, _ string) (string, error) { return "", nil }

func makeRegistry(n int) *tool.Registry {
	reg := tool.NewRegistry()
	for i := range n {
		reg.Register(&mockTool{name: fmt.Sprintf("tool_%d", i)})
	}
	return reg
}

func TestLazyToolProcessor_BelowThreshold(t *testing.T) {
	reg := makeRegistry(5)
	p := NewLazyTools(reg)
	p.Threshold = 10

	sess := session.New("s1")
	req := &llm.Request{}
	if err := p.ApplyRequest(context.Background(), nil, "", sess, req); err != nil {
		t.Fatal(err)
	}
	if len(req.Tools) != 5 {
		t.Errorf("expected 5 tools, got %d", len(req.Tools))
	}
}

func TestLazyToolProcessor_AboveThreshold_OnlySearchTool(t *testing.T) {
	reg := makeRegistry(25)
	p := NewLazyTools(reg)
	p.Threshold = 10

	sess := session.New("s2")
	req := &llm.Request{}
	if err := p.ApplyRequest(context.Background(), nil, "", sess, req); err != nil {
		t.Fatal(err)
	}
	// Only search_tools should be in req.Tools (no prior history).
	if len(req.Tools) != 1 || req.Tools[0].Name != "search_tools" {
		t.Errorf("expected only search_tools, got %v", req.Tools)
	}
}

func TestLazyToolProcessor_DeferredToolsStayHiddenBelowThreshold(t *testing.T) {
	reg := tool.NewRegistry()
	reg.Register(tool.Func("always_on", "Always available", map[string]any{"type": "object"}, func(
		context.Context,
		string,
	) (string, error) {
		return "", nil
	}))
	reg.Register(tool.FuncWithMetadata(
		"expensive_tool",
		"Deferred tool",
		map[string]any{"type": "object"},
		tool.Metadata{Deferred: true},
		func(context.Context, string) (string, error) {
			return "", nil
		},
	))

	p := NewLazyTools(reg)
	p.Threshold = 10

	req := &llm.Request{}
	if err := p.ApplyRequest(t.Context(), nil, "", session.New("s-deferred"), req); err != nil {
		t.Fatal(err)
	}

	if len(req.Tools) != 2 {
		t.Fatalf("expected search_tools plus visible tool, got %d tools", len(req.Tools))
	}
	if req.Tools[0].Name != tool.SearchToolName || req.Tools[1].Name != "always_on" {
		t.Fatalf("unexpected tool order: %s, %s", req.Tools[0].Name, req.Tools[1].Name)
	}
	if strings.Contains(req.Messages[0].Content, "expensive_tool") {
		t.Fatalf("expected compact hint, got %q", req.Messages[0].Content)
	}
}

func TestLazyToolProcessor_PreservesCachePrefixWhenInsertedAfterHistory(t *testing.T) {
	reg := makeRegistry(25)
	p := NewLazyTools(reg)
	p.Threshold = 10

	sess := session.New("s-lazy-cache")
	if err := sess.AppendContext(t.Context(), session.ContextEntry{
		Kind:      session.ContextKindBootstrap,
		Placement: session.ContextPlacementPrefix,
		Content:   "stable context",
	}); err != nil {
		t.Fatal(err)
	}
	if err := sess.Append(t.Context(), session.NewMessage(sess.ID(), llm.Message{
		Role:    llm.RoleUser,
		Content: "hello",
	})); err != nil {
		t.Fatal(err)
	}

	req := &llm.Request{}
	if err := History().ApplyRequest(t.Context(), nil, "", sess, req); err != nil {
		t.Fatal(err)
	}
	if req.CachePrefixMessages != 1 {
		t.Fatalf("cache prefix before lazy tools = %d, want 1", req.CachePrefixMessages)
	}

	if err := p.ApplyRequest(t.Context(), nil, "", sess, req); err != nil {
		t.Fatal(err)
	}

	if req.CachePrefixMessages != 2 {
		t.Fatalf("cache prefix after lazy tools = %d, want 2", req.CachePrefixMessages)
	}
	if req.Messages[0].Role != llm.RoleSystem ||
		!strings.Contains(req.Messages[0].Content, "Additional tools are available") {
		t.Fatalf("expected lazy tool hint to be prepended as system, got %#v", req.Messages[0])
	}
	if req.Messages[1].Content != "stable context" || req.Messages[2].Content != "hello" {
		t.Fatalf("unexpected message order after lazy tools: %#v", req.Messages)
	}
}

func TestSearchUnlockedTools(t *testing.T) {
	sess := session.New("s-tools")
	specs := []llm.Spec{{Name: "tool_1", Description: "desc of tool_1"}}
	data, err := json.Marshal(specs)
	if err != nil {
		t.Fatalf("marshal specs: %v", err)
	}
	if err := sess.Append(context.Background(), session.NewToolCompletedEvent(sess.ID(), session.ToolCompletedData{
		Tool:   "search_tools",
		ID:     "call_1",
		Output: string(data),
	})); err != nil {
		t.Fatalf("append tool completion: %v", err)
	}

	unlocked, err := SearchUnlockedTools(sess)
	if err != nil {
		t.Fatalf("search unlocked tools: %v", err)
	}
	if _, ok := unlocked["tool_1"]; !ok {
		t.Fatal("expected tool_1 to be unlocked")
	}
}

func TestLazyToolProcessor_UnlocksFromSessionState(t *testing.T) {
	reg := makeRegistry(3) // tool_0, tool_1, tool_2
	p := NewLazyTools(reg)
	p.Threshold = 2 // 4 total > 2 → lazy mode

	// Seed session with a prior search_tools result that unlocked tool_1.
	sess := session.New("s3")
	specs := []llm.Spec{{Name: "tool_1", Description: "desc of tool_1"}}
	data, err := json.Marshal(specs)
	if err != nil {
		t.Fatalf("marshal specs: %v", err)
	}
	if err := sess.Append(context.Background(), session.NewToolCompletedEvent(sess.ID(), session.ToolCompletedData{
		Tool:   "search_tools",
		ID:     "call_1",
		Output: string(data),
	})); err != nil {
		t.Fatalf("append tool completion: %v", err)
	}

	req := &llm.Request{}
	if err := p.ApplyRequest(context.Background(), nil, "", sess, req); err != nil {
		t.Fatal(err)
	}

	names := make(map[string]bool)
	for _, spec := range req.Tools {
		names[spec.Name] = true
	}
	if !names["search_tools"] {
		t.Error("expected search_tools in req.Tools")
	}
	if !names["tool_1"] {
		t.Error("expected tool_1 (unlocked) in req.Tools")
	}
	if names["tool_0"] || names["tool_2"] {
		t.Error("expected tool_0 and tool_2 NOT in req.Tools (not unlocked)")
	}
}

func TestLazyToolProcessor_UnlockedToolsAreSorted(t *testing.T) {
	reg := tool.NewRegistry()
	reg.Register(&mockTool{name: "tool_b"})
	reg.Register(&mockTool{name: "tool_a"})

	p := NewLazyTools(reg)
	p.Threshold = 1

	sess := session.New("s4")
	specs := []llm.Spec{
		{Name: "tool_b", Description: "desc of tool_b"},
		{Name: "tool_a", Description: "desc of tool_a"},
	}
	data, err := json.Marshal(specs)
	if err != nil {
		t.Fatalf("marshal specs: %v", err)
	}
	if err := sess.Append(context.Background(), session.NewToolCompletedEvent(sess.ID(), session.ToolCompletedData{
		Tool:   "search_tools",
		ID:     "call_1",
		Output: string(data),
	})); err != nil {
		t.Fatalf("append tool completion: %v", err)
	}

	req := &llm.Request{}
	if err := p.ApplyRequest(context.Background(), nil, "", sess, req); err != nil {
		t.Fatal(err)
	}
	if len(req.Tools) != 2 {
		t.Fatalf("expected 2 tools, got %d", len(req.Tools))
	}
	if req.Tools[0].Name != "tool_a" || req.Tools[1].Name != "tool_b" {
		t.Fatalf(
			"unexpected tool order: %v",
			[]string{req.Tools[0].Name, req.Tools[1].Name},
		)
	}
}
