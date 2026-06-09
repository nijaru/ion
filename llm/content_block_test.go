package llm_test

import (
	"encoding/json"
	"testing"

	"github.com/nijaru/ion/llm"
)

func TestContentBlockSealed(t *testing.T) {
	// Verify all three concrete types implement ContentBlock.
	var _ llm.ContentBlock = llm.TextBlock{}
	var _ llm.ContentBlock = llm.ThinkingBlock{}
	var _ llm.ContentBlock = llm.ToolCallBlock{}
}

func TestTextBlockJSON(t *testing.T) {
	b := llm.TextBlock{Text: "hello"}
	data, err := json.Marshal(b)
	if err != nil {
		t.Fatal(err)
	}
	var decoded llm.TextBlock
	if err := json.Unmarshal(data, &decoded); err != nil {
		t.Fatal(err)
	}
	if decoded.Text != "hello" {
		t.Errorf("got %q, want %q", decoded.Text, "hello")
	}
}

func TestThinkingBlockRedacted(t *testing.T) {
	b := llm.ThinkingBlock{Thinking: "reasoning", Signature: "sig123", Redacted: false}
	if b.Redacted {
		t.Error("expected Redacted=false")
	}

	r := llm.ThinkingBlock{Signature: "sig456", Redacted: true}
	if !r.Redacted {
		t.Error("expected Redacted=true")
	}
	if r.Thinking != "" {
		t.Errorf("expected empty Thinking for redacted block, got %q", r.Thinking)
	}
}

func TestToolCallBlockJSON(t *testing.T) {
	b := llm.ToolCallBlock{ID: "call_1", Name: "read_file", Arguments: `{"path":"/tmp/f"}`}
	data, err := json.Marshal(b)
	if err != nil {
		t.Fatal(err)
	}
	var decoded llm.ToolCallBlock
	if err := json.Unmarshal(data, &decoded); err != nil {
		t.Fatal(err)
	}
	if decoded.ID != "call_1" || decoded.Name != "read_file" || decoded.Arguments != `{"path":"/tmp/f"}` {
		t.Errorf("round-trip mismatch: %+v", decoded)
	}
}

func TestStopReasonConstants(t *testing.T) {
	cases := []struct {
		name string
		got  llm.StopReason
		want string
	}{
		{"stop", llm.StopReasonStop, "stop"},
		{"length", llm.StopReasonLength, "length"},
		{"toolUse", llm.StopReasonToolUse, "toolUse"},
		{"error", llm.StopReasonError, "error"},
		{"aborted", llm.StopReasonAborted, "aborted"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if string(tc.got) != tc.want {
				t.Errorf("got %q, want %q", tc.got, tc.want)
			}
		})
	}
}

func TestCostBreakdownJSON(t *testing.T) {
	cb := llm.CostBreakdown{
		Input:         0.01,
		Output:        0.03,
		CacheRead:     0.005,
		CacheCreation: 0.002,
		Total:         0.047,
	}
	data, err := json.Marshal(cb)
	if err != nil {
		t.Fatal(err)
	}
	var decoded llm.CostBreakdown
	if err := json.Unmarshal(data, &decoded); err != nil {
		t.Fatal(err)
	}
	if decoded.Total != 0.047 {
		t.Errorf("got Total=%f, want 0.047", decoded.Total)
	}
	if decoded.CacheRead != 0.005 {
		t.Errorf("got CacheRead=%f, want 0.005", decoded.CacheRead)
	}
}

func TestMessageGetContentBlocksFromFlat(t *testing.T) {
	m := llm.Message{
		Role:    llm.RoleAssistant,
		Content: "hello",
		ThinkingBlocks: []llm.ThinkingBlock{{
			Thinking:  "reasoning",
			Signature: "sig1",
		}},
		Calls: []llm.Call{{
			ID:   "call_1",
			Type: "function",
			Function: struct {
				Name      string `json:"name"`
				Arguments string `json:"arguments"`
			}{Name: "read_file", Arguments: `{"path":"/f"}`},
		}},
	}

	blocks := m.GetContentBlocks()
	if len(blocks) != 3 {
		t.Fatalf("got %d blocks, want 3", len(blocks))
	}

	// Order: ThinkingBlocks, Content, Calls
	if _, ok := blocks[0].(llm.ThinkingBlock); !ok {
		t.Errorf("block[0]: want ThinkingBlock, got %T", blocks[0])
	}
	if tb, ok := blocks[1].(llm.TextBlock); !ok {
		t.Errorf("block[1]: want TextBlock, got %T", blocks[1])
	} else if tb.Text != "hello" {
		t.Errorf("block[1].Text = %q, want %q", tb.Text, "hello")
	}
	if cb, ok := blocks[2].(llm.ToolCallBlock); !ok {
		t.Errorf("block[2]: want ToolCallBlock, got %T", blocks[2])
	} else if cb.Name != "read_file" {
		t.Errorf("block[2].Name = %q, want %q", cb.Name, "read_file")
	}
}

func TestMessageGetContentBlocksFromBlocks(t *testing.T) {
	blocks := []llm.ContentBlock{
		llm.TextBlock{Text: "from blocks"},
		llm.ToolCallBlock{ID: "c1", Name: "tool", Arguments: "{}"},
	}
	m := llm.Message{Role: llm.RoleAssistant, Blocks: blocks}

	got := m.GetContentBlocks()
	if len(got) != 2 {
		t.Fatalf("got %d blocks, want 2", len(got))
	}
	// Should return the same slice, not convert from flat
	if got[0].(llm.TextBlock).Text != "from blocks" {
		t.Errorf("unexpected text: %v", got[0])
	}
}

func TestMessageSetContentBlocks(t *testing.T) {
	var m llm.Message
	m.SetContentBlocks([]llm.ContentBlock{
		llm.ThinkingBlock{Thinking: "step 1"},
		llm.TextBlock{Text: "answer"},
		llm.ToolCallBlock{ID: "c1", Name: "grep", Arguments: `{"pattern":"foo"}`},
	})

	if m.Content != "answer" {
		t.Errorf("Content = %q, want %q", m.Content, "answer")
	}
	if m.Reasoning != "step 1" {
		t.Errorf("Reasoning = %q, want %q", m.Reasoning, "step 1")
	}
	if len(m.ThinkingBlocks) != 1 {
		t.Errorf("ThinkingBlocks len = %d, want 1", len(m.ThinkingBlocks))
	}
	if len(m.Calls) != 1 {
		t.Fatalf("Calls len = %d, want 1", len(m.Calls))
	}
	if m.Calls[0].Function.Name != "grep" {
		t.Errorf("Calls[0].Name = %q, want %q", m.Calls[0].Function.Name, "grep")
	}
	if len(m.Blocks) != 3 {
		t.Errorf("Blocks len = %d, want 3", len(m.Blocks))
	}
}

func TestResponseGetContentBlocks(t *testing.T) {
	r := llm.Response{
		Content:        "text",
		ThinkingBlocks: []llm.ThinkingBlock{{Thinking: "thought"}},
		Calls: []llm.Call{{
			ID:   "c1",
			Type: "function",
			Function: struct {
				Name      string `json:"name"`
				Arguments string `json:"arguments"`
			}{Name: "tool", Arguments: "{}"},
		}},
	}

	blocks := r.GetContentBlocks()
	if len(blocks) != 3 {
		t.Fatalf("got %d blocks, want 3", len(blocks))
	}

	// Order: ThinkingBlocks, Content, Calls
	if _, ok := blocks[0].(llm.ThinkingBlock); !ok {
		t.Errorf("block[0]: want ThinkingBlock, got %T", blocks[0])
	}
	if _, ok := blocks[1].(llm.TextBlock); !ok {
		t.Errorf("block[1]: want TextBlock, got %T", blocks[1])
	}
	if _, ok := blocks[2].(llm.ToolCallBlock); !ok {
		t.Errorf("block[2]: want ToolCallBlock, got %T", blocks[2])
	}
}

func TestResponseGetContentBlocksPrefersBlocks(t *testing.T) {
	r := llm.Response{
		Content: "flat text",
		Blocks: []llm.ContentBlock{llm.TextBlock{Text: "block text"}},
	}

	blocks := r.GetContentBlocks()
	if len(blocks) != 1 {
		t.Fatalf("got %d blocks, want 1", len(blocks))
	}
	if blocks[0].(llm.TextBlock).Text != "block text" {
		t.Errorf("expected block text, got flat text")
	}
}
