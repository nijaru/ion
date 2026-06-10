package llm_test

import (
	"testing"

	"github.com/nijaru/ion/llm"
)

func TestStreamAccumulatorBlockText(t *testing.T) {
	var acc llm.StreamAccumulator
	acc.Add(&llm.Chunk{Block: llm.TextBlock{Text: "hel"}})
	acc.Add(&llm.Chunk{Block: llm.TextBlock{Text: "lo"}})

	resp := acc.Response()
	if len(resp.Blocks) != 1 {
		t.Fatalf("got %d blocks, want 1", len(resp.Blocks))
	}
	tb, ok := resp.Blocks[0].(llm.TextBlock)
	if !ok {
		t.Fatalf("want TextBlock, got %T", resp.Blocks[0])
	}
	if tb.Text != "hello" {
		t.Errorf("text = %q, want %q", tb.Text, "hello")
	}
	// Flat fields should also be populated
	if resp.Content != "hello" {
		t.Errorf("Content = %q, want %q", resp.Content, "hello")
	}
}

func TestStreamAccumulatorBlockThinking(t *testing.T) {
	var acc llm.StreamAccumulator
	acc.Add(&llm.Chunk{Block: llm.ThinkingBlock{Thinking: "step 1"}})
	acc.Add(&llm.Chunk{Block: llm.ThinkingBlock{Thinking: " step 2"}})

	resp := acc.Response()
	if len(resp.Blocks) != 1 {
		t.Fatalf("got %d blocks, want 1", len(resp.Blocks))
	}
	tb, ok := resp.Blocks[0].(llm.ThinkingBlock)
	if !ok {
		t.Fatalf("want ThinkingBlock, got %T", resp.Blocks[0])
	}
	if tb.Thinking != "step 1 step 2" {
		t.Errorf("thinking = %q, want %q", tb.Thinking, "step 1 step 2")
	}
}

func TestStreamAccumulatorBlockThinkingSignatureBreaksMerge(t *testing.T) {
	var acc llm.StreamAccumulator
	acc.Add(&llm.Chunk{Block: llm.ThinkingBlock{Thinking: "step 1"}})
	acc.Add(&llm.Chunk{Block: llm.ThinkingBlock{Thinking: "step 2", Signature: "sig"}})

	resp := acc.Response()
	if len(resp.Blocks) != 2 {
		t.Fatalf("got %d blocks, want 2", len(resp.Blocks))
	}
}

func TestStreamAccumulatorBlockThinkingRedactedBreaksMerge(t *testing.T) {
	var acc llm.StreamAccumulator
	acc.Add(&llm.Chunk{Block: llm.ThinkingBlock{Thinking: "visible"}})
	acc.Add(&llm.Chunk{Block: llm.ThinkingBlock{Redacted: true, Signature: "redacted_sig"}})

	resp := acc.Response()
	if len(resp.Blocks) != 2 {
		t.Fatalf("got %d blocks, want 2", len(resp.Blocks))
	}
}

func TestStreamAccumulatorBlockToolCall(t *testing.T) {
	var acc llm.StreamAccumulator
	acc.Add(&llm.Chunk{Block: llm.ToolCallBlock{ID: "c1", Name: "read", Arguments: `{"p"}}`}})
	acc.Add(&llm.Chunk{Block: llm.ToolCallBlock{ID: "c1", Name: "read", Arguments: `{"path":"/f"}`}})

	resp := acc.Response()
	if len(resp.Blocks) != 1 {
		t.Fatalf("got %d blocks, want 1", len(resp.Blocks))
	}
	cb, ok := resp.Blocks[0].(llm.ToolCallBlock)
	if !ok {
		t.Fatalf("want ToolCallBlock, got %T", resp.Blocks[0])
	}
	if cb.Arguments != `{"path":"/f"}` {
		t.Errorf("arguments = %q, want %q", cb.Arguments, `{"path":"/f"}`)
	}
}

func TestStreamAccumulatorBlockMixed(t *testing.T) {
	var acc llm.StreamAccumulator
	acc.Add(&llm.Chunk{Block: llm.ThinkingBlock{Thinking: "reasoning"}})
	acc.Add(&llm.Chunk{Block: llm.TextBlock{Text: "answer"}})
	acc.Add(&llm.Chunk{Block: llm.ToolCallBlock{ID: "c1", Name: "grep", Arguments: "{}"}})

	resp := acc.Response()
	if len(resp.Blocks) != 3 {
		t.Fatalf("got %d blocks, want 3", len(resp.Blocks))
	}

	// Flat fields should be populated from blocks
	if resp.Content != "answer" {
		t.Errorf("Content = %q, want %q", resp.Content, "answer")
	}
	if resp.Reasoning != "reasoning" {
		t.Errorf("Reasoning = %q, want %q", resp.Reasoning, "reasoning")
	}
	if len(resp.Calls) != 1 {
		t.Fatalf("Calls len = %d, want 1", len(resp.Calls))
	}
	if resp.Calls[0].Function.Name != "grep" {
		t.Errorf("Calls[0].Name = %q, want %q", resp.Calls[0].Function.Name, "grep")
	}
}

func TestStreamAccumulatorFlatBackwardCompat(t *testing.T) {
	// Providers that still emit flat fields continue to work.
	// Flat fields now write to Blocks (Phase 6b).
	var acc llm.StreamAccumulator
	acc.Add(&llm.Chunk{Content: "hel"})
	acc.Add(&llm.Chunk{Content: "lo"})
	acc.Add(&llm.Chunk{Reasoning: "think"})

	resp := acc.Response()
	if resp.Content != "hello" {
		t.Errorf("Content = %q, want %q", resp.Content, "hello")
	}
	if resp.Reasoning != "think" {
		t.Errorf("Reasoning = %q, want %q", resp.Reasoning, "think")
	}
	if len(resp.Blocks) != 2 {
		t.Errorf("Blocks should have 2 entries (text + thinking), got %d", len(resp.Blocks))
	}
}

func TestStreamAccumulatorMixedBlockAndFlatToolCalls(t *testing.T) {
	// Text/reasoning use Chunk.Block, tool calls use flat Calls.
	// Response() must merge both into a complete Blocks list.
	var acc llm.StreamAccumulator
	acc.Add(&llm.Chunk{Content: "Hello", Block: llm.TextBlock{Text: "Hello"}})
	acc.Add(&llm.Chunk{
		Reasoning:      "thinking...",
		ThinkingBlocks: []llm.ThinkingBlock{{Thinking: "thinking..."}},
		Block:          llm.ThinkingBlock{Thinking: "thinking..."},
	})
	acc.Add(&llm.Chunk{
		Calls: []llm.Call{{
			ID:       "call-1",
			Type:     "function",
			Function: struct {
				Name      string `json:"name"`
				Arguments string `json:"arguments"`
			}{Name: "bash", Arguments: "{\"cmd\":\"ls\"}"},
		}},
	})

	resp := acc.Response()

	// Flat fields should be populated.
	if resp.Content != "Hello" {
		t.Errorf("Content = %q, want %q", resp.Content, "Hello")
	}
	if resp.Reasoning != "thinking..." {
		t.Errorf("Reasoning = %q, want %q", resp.Reasoning, "thinking...")
	}
	if len(resp.Calls) != 1 || resp.Calls[0].ID != "call-1" {
		t.Errorf("Calls = %v, want 1 call with ID call-1", resp.Calls)
	}

	// Blocks should contain all three: text, thinking, tool call.
	if len(resp.Blocks) != 3 {
		t.Fatalf("Blocks = %d, want 3", len(resp.Blocks))
	}
	if _, ok := resp.Blocks[0].(llm.TextBlock); !ok {
		t.Errorf("Blocks[0] = %T, want TextBlock", resp.Blocks[0])
	}
	if _, ok := resp.Blocks[1].(llm.ThinkingBlock); !ok {
		t.Errorf("Blocks[1] = %T, want ThinkingBlock", resp.Blocks[1])
	}
	if tcb, ok := resp.Blocks[2].(llm.ToolCallBlock); !ok {
		t.Errorf("Blocks[2] = %T, want ToolCallBlock", resp.Blocks[2])
	} else if tcb.ID != "call-1" || tcb.Name != "bash" {
		t.Errorf("Blocks[2] = %+v, want ID=call-1 Name=bash", tcb)
	}
}
