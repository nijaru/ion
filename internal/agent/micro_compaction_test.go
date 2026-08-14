package agent

import (
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/session"
)

func TestMicroCompactMessagesPrunesHistoricalToolResults(t *testing.T) {
	now := time.Now()

	// Build a 5-turn conversation
	var msgs []session.Message

	longOutput := strings.Repeat("A very long line of tool output that consumes token context.\n", 100) // ~6000 chars

	// Turn 1 (Old)
	msgs = append(msgs, session.NewUserText("Turn 1 prompt", now))
	msgs = append(msgs, &session.AssistantMessage{
		Content: []session.Content{
			&session.ToolCall{ID: "call_1", Name: "grep_search", Arguments: map[string]any{"query": "test"}},
		},
		Timestamp: now,
	})
	msgs = append(msgs, &session.ToolResultMessage{
		ToolCallID: "call_1",
		ToolName:   "grep_search",
		Content:    []session.Content{session.TextContent{Text: longOutput}},
		Timestamp:  now,
	})

	// Turn 2 (Old)
	msgs = append(msgs, session.NewUserText("Turn 2 prompt", now))
	msgs = append(msgs, &session.AssistantMessage{
		Content: []session.Content{
			&session.ToolCall{ID: "call_2", Name: "view_file", Arguments: map[string]any{"path": "main.go"}},
		},
		Timestamp: now,
	})
	msgs = append(msgs, &session.ToolResultMessage{
		ToolCallID: "call_2",
		ToolName:   "view_file",
		Content:    []session.Content{session.TextContent{Text: longOutput}},
		Timestamp:  now,
	})

	// Turn 3 (Recent 1)
	msgs = append(msgs, session.NewUserText("Turn 3 prompt", now))
	msgs = append(msgs, &session.AssistantMessage{
		Content:   []session.Content{session.TextContent{Text: "Turn 3 answer"}},
		Timestamp: now,
	})

	// Turn 4 (Recent 2)
	msgs = append(msgs, session.NewUserText("Turn 4 prompt", now))
	msgs = append(msgs, &session.AssistantMessage{
		Content: []session.Content{
			&session.ToolCall{ID: "call_4", Name: "view_file", Arguments: map[string]any{"path": "recent.go"}},
		},
		Timestamp: now,
	})
	msgs = append(msgs, &session.ToolResultMessage{
		ToolCallID: "call_4",
		ToolName:   "view_file",
		Content:    []session.Content{session.TextContent{Text: longOutput}},
		Timestamp:  now,
	})

	// Turn 5 (Recent 3 - active)
	msgs = append(msgs, session.NewUserText("Turn 5 prompt", now))

	// Run micro-compaction keeping 3 recent turns
	compacted := MicroCompactMessages(msgs, 3, 2000)

	if len(compacted) != len(msgs) {
		t.Fatalf("expected length %d, got %d", len(msgs), len(compacted))
	}

	// Turn 1 tool result should be truncated
	t1Result := compacted[2].(*session.ToolResultMessage)
	t1Text := t1Result.Content[0].(session.TextContent).Text
	if !strings.Contains(t1Text, "trimmed by micro-compaction") {
		t.Fatalf("expected Turn 1 tool result to be micro-compacted, got len=%d", len(t1Text))
	}

	// Turn 2 tool result should be truncated
	t2Result := compacted[5].(*session.ToolResultMessage)
	t2Text := t2Result.Content[0].(session.TextContent).Text
	if !strings.Contains(t2Text, "trimmed by micro-compaction") {
		t.Fatalf("expected Turn 2 tool result to be micro-compacted, got len=%d", len(t2Text))
	}

	// Turn 4 tool result (part of recent 3 turns) must NOT be truncated
	t4Result := compacted[10].(*session.ToolResultMessage)
	t4Text := t4Result.Content[0].(session.TextContent).Text
	if strings.Contains(t4Text, "trimmed by micro-compaction") {
		t.Fatal("expected recent Turn 4 tool result to remain full fidelity")
	}
	if len(t4Text) != len(longOutput) {
		t.Fatalf("expected Turn 4 length %d, got %d", len(longOutput), len(t4Text))
	}
}
