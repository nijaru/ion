package llm_test

import (
	"testing"

	"github.com/nijaru/ion/llm"
)

func TestPrepareRequestAcceptsStructuredAssistantToolCalls(t *testing.T) {
	req := &llm.Request{
		Messages: []llm.Message{
			{Role: llm.RoleUser, Content: "read the file"},
			{Role: llm.RoleAssistant, Blocks: []llm.ContentBlock{
				llm.ToolCallBlock{ID: "call/1", Name: "read", Arguments: "{}"},
			}},
			{Role: llm.RoleTool, ToolID: "call/1", Content: "contents"},
		},
	}

	prepared, err := llm.PrepareRequestForCapabilities(req, llm.DefaultCapabilities())
	if err != nil {
		t.Fatalf("PrepareRequestForCapabilities() error = %v", err)
	}
	if got := prepared.Messages[1].BlocksToolCalls()[0].ID; got != "call_1" {
		t.Fatalf("normalized block call ID = %q, want call_1", got)
	}
	if got := prepared.Messages[2].ToolID; got != "call_1" {
		t.Fatalf("normalized tool result ID = %q, want call_1", got)
	}
	if got := req.Messages[1].BlocksToolCalls()[0].ID; got != "call/1" {
		t.Fatalf("original block call ID = %q, want unchanged", got)
	}
	if prepared.CapabilitySnapshot == nil || prepared.CapabilitySnapshot.SystemRole != llm.RoleSystem {
		t.Fatalf("capability snapshot = %#v, want prepared capabilities", prepared.CapabilitySnapshot)
	}
}

func TestPrepareRequestMarksMissingToolResultsAsErrors(t *testing.T) {
	req := &llm.Request{Messages: []llm.Message{
		{Role: llm.RoleAssistant, Calls: []llm.Call{{
			ID: "call-1",
			Function: struct {
				Name      string `json:"name"`
				Arguments string `json:"arguments"`
			}{Name: "read", Arguments: "{}"},
		}}},
	}}
	prepared, err := llm.PrepareRequestForCapabilities(req, llm.DefaultCapabilities())
	if err != nil {
		t.Fatalf("PrepareRequestForCapabilities() error = %v", err)
	}
	if len(prepared.Messages) != 2 || prepared.Messages[1].Role != llm.RoleTool ||
		!prepared.Messages[1].IsError || prepared.Messages[1].ToolID != "call-1" {
		t.Fatalf("prepared messages = %#v, want synthesized error result", prepared.Messages)
	}
}

func TestPrepareRequestOmitsToolsForModelWithoutToolSupport(t *testing.T) {
	req := &llm.Request{
		Tools:    []*llm.Spec{{Name: "read", Parameters: map[string]any{"type": "object"}}},
		Messages: []llm.Message{{Role: llm.RoleUser, Content: "hello"}},
	}
	caps := llm.DefaultCapabilities()
	caps.Tools = false

	prepared, err := llm.PrepareRequestForCapabilities(req, caps)
	if err != nil {
		t.Fatalf("PrepareRequestForCapabilities() error = %v", err)
	}
	if prepared.Tools != nil {
		t.Fatalf("prepared tools = %#v, want nil", prepared.Tools)
	}
	if req.Tools == nil {
		t.Fatal("original request tools were mutated")
	}
}

func TestPrepareRequestDowngradesImagesForTextOnlyModels(t *testing.T) {
	req := &llm.Request{Messages: []llm.Message{{
		Role:  llm.RoleUser,
		Parts: []llm.ContentPart{llm.TextPart("look"), llm.ImagePart("image/png", "aW1hZ2U=")},
	}}}
	caps := llm.DefaultCapabilities()
	caps.InputModalities = []string{"text"}
	prepared, err := llm.PrepareRequestForCapabilities(req, caps)
	if err != nil {
		t.Fatalf("PrepareRequestForCapabilities() error = %v", err)
	}
	if len(prepared.Messages[0].Parts) != 2 || prepared.Messages[0].Parts[1].Type != llm.ContentPartText ||
		prepared.Messages[0].Parts[1].Text != "[image omitted: model does not accept image input]" {
		t.Fatalf("prepared image parts = %#v, want explicit text placeholder", prepared.Messages[0].Parts)
	}
	if req.Messages[0].Parts[1].Type != llm.ContentPartImage {
		t.Fatal("original request image was mutated")
	}
}

func TestPrepareRequestMapsEffortToThinkingBudget(t *testing.T) {
	request := &llm.Request{
		Model:           "claude-sonnet-4-20250514",
		ReasoningEffort: "xhigh",
		Messages:        []llm.Message{{Role: llm.RoleUser, Content: "hello"}},
	}
	caps := llm.Capabilities{
		Streaming: true,
		Tools:     true,
		Reasoning: llm.ReasoningCapabilities{
			Kind:                llm.ReasoningKindBudget,
			CanDisable:          true,
			BudgetMinTokens:     1024,
			BudgetDefaultTokens: 4096,
			BudgetMaxTokens:     32768,
		},
	}

	prepared, err := llm.PrepareRequestForCapabilities(request, caps)
	if err != nil {
		t.Fatalf("PrepareRequestForCapabilities() error = %v", err)
	}
	if prepared.ThinkingBudget != 16384 || prepared.ReasoningEffort != "" {
		t.Fatalf(
			"prepared reasoning = effort %q, budget %d; want empty effort and 16384 budget",
			prepared.ReasoningEffort,
			prepared.ThinkingBudget,
		)
	}
	if request.ThinkingBudget != 0 || request.ReasoningEffort != "xhigh" {
		t.Fatalf("original request mutated: %#v", request)
	}
}
