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
