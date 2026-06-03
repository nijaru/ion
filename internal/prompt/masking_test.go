package prompt_test

import (
	"strings"
	"testing"

	"github.com/nijaru/ion/llm"
	prompt "github.com/nijaru/ion/internal/prompt"
	"github.com/nijaru/ion/session"
)

func TestObservationMaskerMaskEntriesMasksOlderLargeToolOutputs(t *testing.T) {
	masker := prompt.NewObservationMasker(prompt.NewBudgetGuard(120))
	masker.MaxContentTokens = 10
	masker.MinKeepMessages = 2

	large := strings.Repeat("large output ", 20)
	entries := []session.HistoryEntry{
		{
			EventID: "evt-user",
			Message: llm.Message{Role: llm.RoleUser, Content: "request"},
		},
		{
			EventID: "evt-tool",
			Message: llm.Message{Role: llm.RoleTool, Content: large, ToolID: "tool-1"},
		},
		{
			EventID: "evt-assistant",
			Message: llm.Message{Role: llm.RoleAssistant, Content: "done"},
		},
		{
			EventID: "evt-recent",
			Message: llm.Message{Role: llm.RoleTool, Content: large, ToolID: "recent"},
		},
	}

	masked, status := masker.MaskEntries(t.Context(), nil, "", nil, entries)
	if status.Level != prompt.BudgetWarning {
		t.Fatalf("expected masked history to fall back out of terminal range, got %s", status.Level)
	}
	if masked[1].Message.Content == large {
		t.Fatal("expected older tool output to be masked")
	}
	if !strings.Contains(masked[1].Message.Content, "evt-tool") {
		t.Fatalf("expected placeholder to include event id, got %q", masked[1].Message.Content)
	}
	if !strings.Contains(masked[1].Message.Content, "tool-1") {
		t.Fatalf("expected placeholder to include tool id, got %q", masked[1].Message.Content)
	}
	if masked[3].Message.Content != large {
		t.Fatal("expected recent tool output to be preserved")
	}
}

func TestObservationMaskerMaskEntriesLeavesHistoryUntouchedBelowBudget(t *testing.T) {
	masker := prompt.NewObservationMasker(prompt.NewBudgetGuard(10_000))
	masker.MaxContentTokens = 10

	large := strings.Repeat("large output ", 20)
	entries := []session.HistoryEntry{
		{
			EventID: "evt-tool",
			Message: llm.Message{Role: llm.RoleTool, Content: large, ToolID: "tool-1"},
		},
	}

	masked, status := masker.MaskEntries(t.Context(), nil, "", nil, entries)
	if status.Level != prompt.BudgetOK {
		t.Fatalf("expected ok status, got %s", status.Level)
	}
	if masked[0].Message.Content != large {
		t.Fatalf("expected no masking below budget, got %q", masked[0].Message.Content)
	}
}

func TestObservationMaskerHistoryProcessorAppendsMaskedHistory(t *testing.T) {
	sess := session.New("masked-history")
	if err := sess.AppendContext(t.Context(), session.ContextEntry{
		Kind:    session.ContextKindBootstrap,
		Content: "workspace context",
	}); err != nil {
		t.Fatalf("append context: %v", err)
	}
	large := strings.Repeat("large output ", 20)
	call := llm.Call{ID: "tool-1", Type: "function"}
	call.Function.Name = "read"
	history := []llm.Message{
		{Role: llm.RoleUser, Content: "request"},
		{Role: llm.RoleAssistant, Calls: []llm.Call{call}},
		{Role: llm.RoleTool, Content: large, ToolID: "tool-1"},
		{Role: llm.RoleAssistant, Content: "done"},
		{Role: llm.RoleUser, Content: "recent"},
	}
	for _, msg := range history {
		if err := sess.Append(t.Context(), session.NewMessage(sess.ID(), msg)); err != nil {
			t.Fatalf("append history: %v", err)
		}
	}

	masker := prompt.NewObservationMasker(prompt.NewBudgetGuard(80))
	masker.MaxContentTokens = 10
	masker.MinKeepMessages = 2

	req := &llm.Request{
		Messages: []llm.Message{{Role: llm.RoleSystem, Content: "instructions"}},
	}
	if err := masker.History().ApplyRequest(t.Context(), nil, "", sess, req); err != nil {
		t.Fatalf("masked history apply: %v", err)
	}

	if len(req.Messages) != 7 {
		t.Fatalf("expected 7 messages, got %d", len(req.Messages))
	}
	if req.CachePrefixMessages != 2 {
		t.Fatalf("expected system plus context cache prefix, got %d", req.CachePrefixMessages)
	}
	if req.Messages[1].Content != "workspace context" {
		t.Fatalf("expected stable context after system prefix, got %q", req.Messages[1].Content)
	}
	if !strings.Contains(req.Messages[4].Content, "Observation masked") {
		t.Fatalf("expected masked tool output, got %q", req.Messages[4].Content)
	}
	if req.Messages[6].Content != "recent" {
		t.Fatalf("expected recent message to remain unchanged, got %q", req.Messages[6].Content)
	}
}
