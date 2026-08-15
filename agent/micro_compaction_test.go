package agent

import (
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func TestPruneHistoricalToolOutputs(t *testing.T) {
	now := time.Now()

	// Turn 1 (Historical)
	user1 := session.NewUserText("read file1", now)
	tool1 := &session.ToolResultMessage{
		ToolCallID: "call-1",
		ToolName:   "read",
		Content: []session.Content{
			session.TextContent{
				Text: "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\nline 9\nline 10\nline 11\nline 12",
			},
		},
	}
	asst1 := &session.AssistantMessage{
		Content: []session.Content{session.TextContent{Text: "file1 read complete"}},
	}

	// Turn 2 (Recent)
	user2 := session.NewUserText("read file2", now.Add(time.Second))
	tool2 := &session.ToolResultMessage{
		ToolCallID: "call-2",
		ToolName:   "read",
		Content: []session.Content{
			session.TextContent{
				Text: "line A\nline B\nline C\nline D\nline E\nline F\nline G\nline H\nline I\nline J",
			},
		},
	}
	asst2 := &session.AssistantMessage{
		Content: []session.Content{session.TextContent{Text: "file2 read complete"}},
	}

	// Turn 3 (Recent)
	user3 := session.NewUserText("summarize both", now.Add(2*time.Second))

	msgs := []session.Message{user1, tool1, asst1, user2, tool2, asst2, user3}

	opts := MicroCompactionOptions{
		Enabled:                     true,
		KeepRecentTurns:             2,
		MaxLinesPerHistoricalResult: 4, // 2 head, 2 tail
		MaxBytesPerHistoricalResult: 1024,
	}

	pruned := PruneHistoricalToolOutputs(msgs, opts)

	if len(pruned) != len(msgs) {
		t.Fatalf("pruned length = %d, want %d", len(pruned), len(msgs))
	}

	// Turn 1 tool result should be pruned
	prunedTool1, ok := pruned[1].(*session.ToolResultMessage)
	if !ok {
		t.Fatalf("pruned[1] = %T, want *session.ToolResultMessage", pruned[1])
	}
	text1 := prunedTool1.Content[0].(session.TextContent).Text
	if !strings.Contains(text1, "lines pruned for context efficiency") {
		t.Fatalf("turn 1 tool output was not pruned: %q", text1)
	}
	if !strings.HasPrefix(text1, "line 1\nline 2\n") {
		t.Fatalf("turn 1 tool output missing head: %q", text1)
	}
	if !strings.HasSuffix(text1, "\nline 11\nline 12") {
		t.Fatalf("turn 1 tool output missing tail: %q", text1)
	}

	// Turn 2 tool result should NOT be pruned (within recent 2 turns)
	prunedTool2, ok := pruned[4].(*session.ToolResultMessage)
	if !ok {
		t.Fatalf("pruned[4] = %T, want *session.ToolResultMessage", pruned[4])
	}
	text2 := prunedTool2.Content[0].(session.TextContent).Text
	if strings.Contains(text2, "lines pruned") {
		t.Fatalf("turn 2 tool output was unexpectedly pruned: %q", text2)
	}

	// Original msgs slice must not be modified
	origText1 := tool1.Content[0].(session.TextContent).Text
	if strings.Contains(origText1, "lines pruned") {
		t.Fatalf("original tool1 text was mutated: %q", origText1)
	}
}

func TestPruneHistoricalToolOutputsPreservesErrors(t *testing.T) {
	now := time.Now()

	// Turn 1 (Historical) with tool error
	user1 := session.NewUserText("run command", now)
	tool1 := &session.ToolResultMessage{
		ToolCallID: "call-err",
		ToolName:   "bash",
		IsError:    true,
		Content: []session.Content{
			session.TextContent{
				Text: strings.Repeat("error line\n", 20),
			},
		},
	}
	asst1 := &session.AssistantMessage{
		Content: []session.Content{session.TextContent{Text: "failed"}},
	}

	// Turn 2 & 3
	user2 := session.NewUserText("turn 2", now)
	user3 := session.NewUserText("turn 3", now)

	msgs := []session.Message{user1, tool1, asst1, user2, user3}
	opts := DefaultMicroCompactionOptions()

	pruned := PruneHistoricalToolOutputs(msgs, opts)
	prunedTool1 := pruned[1].(*session.ToolResultMessage)
	text1 := prunedTool1.Content[0].(session.TextContent).Text
	if strings.Contains(text1, "lines pruned") {
		t.Fatalf("historical tool error was pruned, want preserved in full: %q", text1)
	}
}

func TestConvertWithMicroCompactionIntegration(t *testing.T) {
	now := time.Now()

	user1 := session.NewUserText("read file1", now)
	tool1 := &session.ToolResultMessage{
		ToolCallID: "call-1",
		ToolName:   "read",
		Content: []session.Content{
			session.TextContent{Text: strings.Repeat("log row\n", 50)},
		},
	}
	asst1 := &session.AssistantMessage{
		Content: []session.Content{session.TextContent{Text: "read done"}},
	}
	user2 := session.NewUserText("turn 2", now)
	user3 := session.NewUserText("turn 3", now)

	msgs := []session.Message{user1, tool1, asst1, user2, user3}
	opts := DefaultMicroCompactionOptions()

	llmMsgs := ConvertWithMicroCompaction(msgs, opts)

	// Verify llm.Message for tool1 has pruned text and role llm.RoleTool
	if len(llmMsgs) < 2 {
		t.Fatalf("llmMsgs len = %d, want at least 2", len(llmMsgs))
	}
	toolMsg := llmMsgs[1]
	if toolMsg.Role != llm.RoleTool {
		t.Fatalf("toolMsg role = %q, want %q", toolMsg.Role, llm.RoleTool)
	}
	if !strings.Contains(toolMsg.Content, "lines pruned for context efficiency") {
		t.Fatalf("converted toolMsg content was not pruned: %q", toolMsg.Content)
	}
}
