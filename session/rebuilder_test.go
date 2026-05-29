package session

import (
	"strings"
	"testing"

	"github.com/nijaru/ion/llm"
)

func appendLegacyEvent(sess *Session, e Event) {
	sess.events = append(sess.events, e)
}

func TestRebuilderRebuildEntriesWithoutCompactionFallsBackToRawHistory(t *testing.T) {
	sess := New("raw")
	for _, msg := range []llm.Message{
		{Role: llm.RoleUser, Content: "one"},
		{Role: llm.RoleAssistant, Content: "two"},
	} {
		if err := sess.Append(t.Context(), NewMessage(sess.ID(), msg)); err != nil {
			t.Fatalf("append message: %v", err)
		}
	}

	entries, err := NewRebuilder().RebuildEntries(sess)
	if err != nil {
		t.Fatalf("RebuildEntries: %v", err)
	}
	if len(entries) != 2 {
		t.Fatalf("expected 2 entries, got %d", len(entries))
	}
	if entries[0].Message.Content != "one" || entries[1].Message.Content != "two" {
		t.Fatalf("unexpected rebuilt entries: %#v", entries)
	}
}

func TestRebuilderDropsEmptyAssistantMessagesFromRawHistory(t *testing.T) {
	sess := New("raw-empty-assistant")
	for _, msg := range []llm.Message{
		{Role: llm.RoleUser, Content: "before"},
		{Role: llm.RoleAssistant},
		{Role: llm.RoleAssistant, Content: "after"},
	} {
		e := NewMessage(sess.ID(), msg)
		if msg.Role == llm.RoleAssistant && msg.Content == "" {
			appendLegacyEvent(sess, e)
			continue
		}
		if err := sess.Append(t.Context(), e); err != nil {
			t.Fatalf("append message: %v", err)
		}
	}

	messages, err := sess.EffectiveMessages()
	if err != nil {
		t.Fatalf("EffectiveMessages: %v", err)
	}
	if len(messages) != 2 {
		t.Fatalf("expected empty assistant to be omitted, got %#v", messages)
	}
	if messages[0].Content != "before" || messages[1].Content != "after" {
		t.Fatalf("unexpected effective messages: %#v", messages)
	}
}

func TestRebuilderDropsUnknownRoleMessagesFromRawHistory(t *testing.T) {
	sess := New("raw-unknown-role")
	appendLegacyEvent(sess, NewMessage(sess.ID(), llm.Message{
		Content: "missing role",
	}))
	if err := sess.Append(t.Context(), NewMessage(sess.ID(), llm.Message{
		Role:    llm.RoleUser,
		Content: "kept",
	})); err != nil {
		t.Fatalf("append message: %v", err)
	}

	messages, err := sess.EffectiveMessages()
	if err != nil {
		t.Fatalf("EffectiveMessages: %v", err)
	}
	if len(messages) != 1 || messages[0].Content != "kept" {
		t.Fatalf("unexpected effective messages: %#v", messages)
	}
}

func TestRebuilderPreservesAssistantPayloadKinds(t *testing.T) {
	call := llm.Call{ID: "call-1", Type: "function"}
	call.Function.Name = "read"
	call.Function.Arguments = `{"path":"README.md"}`

	sess := New("assistant-payload-kinds")
	for _, msg := range []llm.Message{
		{Role: llm.RoleAssistant, Reasoning: "reasoning only"},
		{Role: llm.RoleAssistant, ThinkingBlocks: []llm.ThinkingBlock{{Type: "thinking", Thinking: "thinking only"}}},
		{Role: llm.RoleAssistant, Calls: []llm.Call{call}},
		{Role: llm.RoleTool, ToolID: "call-1", Name: "read", Content: "result"},
	} {
		if err := sess.Append(t.Context(), NewMessage(sess.ID(), msg)); err != nil {
			t.Fatalf("append message: %v", err)
		}
	}

	messages, err := sess.EffectiveMessages()
	if err != nil {
		t.Fatalf("EffectiveMessages: %v", err)
	}
	if len(messages) != 4 {
		t.Fatalf("expected all payload-bearing messages to remain, got %#v", messages)
	}
	if messages[0].Reasoning == "" || len(messages[1].ThinkingBlocks) != 1 ||
		len(messages[2].Calls) != 1 || messages[3].Role != llm.RoleTool {
		t.Fatalf("unexpected effective messages: %#v", messages)
	}
}

func TestRebuilderDropsUnmatchedToolMessages(t *testing.T) {
	call := llm.Call{ID: "call-1", Type: "function"}
	call.Function.Name = "read"
	call.Function.Arguments = `{"path":"README.md"}`

	replayer := NewReplayer()
	sess := replayer.NewSession("legacy-unmatched-tool")
	for _, msg := range []llm.Message{
		{Role: llm.RoleTool, ToolID: "orphan", Name: "read", Content: "orphan result"},
		{Role: llm.RoleAssistant, Calls: []llm.Call{call}},
		{Role: llm.RoleTool, ToolID: "wrong", Name: "read", Content: "wrong result"},
		{Role: llm.RoleTool, ToolID: "call-1", Name: "read", Content: "kept result"},
		{Role: llm.RoleTool, ToolID: "call-1", Name: "read", Content: "duplicate result"},
	} {
		if err := replayer.Apply(sess, NewMessage(sess.ID(), msg)); err != nil {
			t.Fatalf("replay legacy message: %v", err)
		}
	}

	messages, err := sess.EffectiveMessages()
	if err != nil {
		t.Fatalf("EffectiveMessages: %v", err)
	}
	if len(messages) != 2 {
		t.Fatalf("expected assistant and one matched tool result, got %#v", messages)
	}
	if messages[0].Role != llm.RoleAssistant || len(messages[0].Calls) != 1 {
		t.Fatalf("expected assistant tool call first, got %#v", messages[0])
	}
	if messages[1].Role != llm.RoleTool || messages[1].ToolID != "call-1" ||
		messages[1].Content != "kept result" {
		t.Fatalf("unexpected matched tool result: %#v", messages[1])
	}
}

func TestRebuilderDropsLateToolMessageAfterTurnBoundary(t *testing.T) {
	call := llm.Call{ID: "call-1", Type: "function"}
	call.Function.Name = "read"

	replayer := NewReplayer()
	sess := replayer.NewSession("legacy-late-tool")
	for _, msg := range []llm.Message{
		{Role: llm.RoleAssistant, Calls: []llm.Call{call}},
		{Role: llm.RoleUser, Content: "next turn"},
		{Role: llm.RoleTool, ToolID: "call-1", Name: "read", Content: "late result"},
	} {
		if err := replayer.Apply(sess, NewMessage(sess.ID(), msg)); err != nil {
			t.Fatalf("replay legacy message: %v", err)
		}
	}

	messages, err := sess.EffectiveMessages()
	if err != nil {
		t.Fatalf("EffectiveMessages: %v", err)
	}
	if len(messages) != 1 {
		t.Fatalf(
			"expected dangling assistant and late tool result to be omitted, got %#v",
			messages,
		)
	}
	if messages[0].Role != llm.RoleUser {
		t.Fatalf("unexpected effective history: %#v", messages)
	}
}

func TestRebuilderAnnotatesToolHistoryFromLifecycleEvents(t *testing.T) {
	call := llm.Call{ID: "call-1", Type: "function"}
	call.Function.Name = "read"
	call.Function.Arguments = `{"file_path":"AGENTS.md"}`

	sess := New("tool-history")
	if err := sess.Append(t.Context(), NewMessage(sess.ID(), llm.Message{
		Role:  llm.RoleAssistant,
		Calls: []llm.Call{call},
	})); err != nil {
		t.Fatalf("append assistant call: %v", err)
	}
	if err := sess.Append(t.Context(), NewToolStartedEvent(sess.ID(), ToolStartedData{
		Tool:           "read",
		Arguments:      `{"file_path":"AGENTS.md"}`,
		ID:             "call-1",
		IdempotencyKey: "turn-1:call-1",
	})); err != nil {
		t.Fatalf("append tool started: %v", err)
	}
	if err := sess.Append(t.Context(), NewMessage(sess.ID(), llm.Message{
		Role:    llm.RoleTool,
		ToolID:  "call-1",
		Name:    "read",
		Content: "file contents",
	})); err != nil {
		t.Fatalf("append tool result message: %v", err)
	}
	if err := sess.Append(t.Context(), NewToolCompletedEvent(sess.ID(), ToolCompletedData{
		Tool:           "read",
		ID:             "call-1",
		IdempotencyKey: "turn-1:call-1",
		Output:         "file contents",
	})); err != nil {
		t.Fatalf("append tool completed: %v", err)
	}

	entries, err := sess.EffectiveEntries()
	if err != nil {
		t.Fatalf("EffectiveEntries: %v", err)
	}
	if len(entries) != 2 {
		t.Fatalf("entries len = %d, want 2: %#v", len(entries), entries)
	}
	tool := entries[1].Tool
	if tool == nil {
		t.Fatalf("tool metadata missing from entry: %#v", entries[1])
	}
	if tool.ID != "call-1" || tool.Name != "read" ||
		tool.Arguments != `{"file_path":"AGENTS.md"}` ||
		tool.IdempotencyKey != "turn-1:call-1" ||
		tool.IsError {
		t.Fatalf("unexpected tool metadata: %#v", tool)
	}
}

func TestRebuilderAnnotatesToolErrorsFromLifecycleEvents(t *testing.T) {
	call := llm.Call{ID: "call-err", Type: "function"}
	call.Function.Name = "bash"

	sess := New("tool-error-history")
	if err := sess.Append(t.Context(), NewMessage(sess.ID(), llm.Message{
		Role:  llm.RoleAssistant,
		Calls: []llm.Call{call},
	})); err != nil {
		t.Fatalf("append assistant call: %v", err)
	}
	if err := sess.Append(t.Context(), NewMessage(sess.ID(), llm.Message{
		Role:    llm.RoleTool,
		ToolID:  "call-err",
		Name:    "bash",
		Content: "exit status 1",
	})); err != nil {
		t.Fatalf("append tool result message: %v", err)
	}
	if err := sess.Append(t.Context(), NewToolCompletedEvent(sess.ID(), ToolCompletedData{
		Tool:  "bash",
		ID:    "call-err",
		Error: "exit status 1",
	})); err != nil {
		t.Fatalf("append tool completed: %v", err)
	}

	entries, err := sess.EffectiveEntries()
	if err != nil {
		t.Fatalf("EffectiveEntries: %v", err)
	}
	tool := entries[1].Tool
	if tool == nil || !tool.IsError || tool.Error != "exit status 1" {
		t.Fatalf("unexpected tool error metadata: %#v", tool)
	}
}

func TestRebuilderRecoversMissingToolMessageFromCompletedLifecycle(t *testing.T) {
	call := llm.Call{ID: "call-recovered", Type: "function"}
	call.Function.Name = "read"
	call.Function.Arguments = `{"file_path":"AGENTS.md"}`

	sess := New("tool-message-recovery")
	if err := sess.Append(t.Context(), NewMessage(sess.ID(), llm.Message{
		Role:  llm.RoleAssistant,
		Calls: []llm.Call{call},
	})); err != nil {
		t.Fatalf("append assistant call: %v", err)
	}
	if err := sess.Append(t.Context(), NewToolStartedEvent(sess.ID(), ToolStartedData{
		Tool:           "read",
		Arguments:      `{"file_path":"AGENTS.md"}`,
		ID:             "call-recovered",
		IdempotencyKey: "turn-1:call-recovered",
	})); err != nil {
		t.Fatalf("append tool started: %v", err)
	}
	completed := NewToolCompletedEvent(sess.ID(), ToolCompletedData{
		Tool:           "read",
		ID:             "call-recovered",
		IdempotencyKey: "turn-1:call-recovered",
		Output:         "file contents",
		Parts: []llm.ContentPart{
			llm.TextPart("file contents"),
			llm.ImagePart("image/png", "aW1hZ2U="),
		},
	})
	if err := sess.Append(t.Context(), completed); err != nil {
		t.Fatalf("append tool completed: %v", err)
	}

	entries, err := sess.EffectiveEntries()
	if err != nil {
		t.Fatalf("EffectiveEntries: %v", err)
	}
	if len(entries) != 2 {
		t.Fatalf("entries len = %d, want 2: %#v", len(entries), entries)
	}
	if entries[1].EventID != completed.ID.String() || entries[1].EventType != ToolCompleted {
		t.Fatalf(
			"recovered entry source = (%q, %q), want tool completed event",
			entries[1].EventID,
			entries[1].EventType,
		)
	}
	if entries[1].Message.Role != llm.RoleTool ||
		entries[1].Message.ToolID != "call-recovered" ||
		entries[1].Message.Content != "file contents" {
		t.Fatalf("unexpected recovered tool message: %#v", entries[1].Message)
	}
	if len(entries[1].Message.Parts) != 2 ||
		entries[1].Message.Parts[1].Type != llm.ContentPartImage ||
		entries[1].Message.Parts[1].Data != "aW1hZ2U=" {
		t.Fatalf("recovered tool parts = %+v, want text plus image", entries[1].Message.Parts)
	}
	tool := entries[1].Tool
	if tool == nil || tool.Name != "read" || tool.Arguments != `{"file_path":"AGENTS.md"}` {
		t.Fatalf("unexpected recovered tool metadata: %#v", tool)
	}
}

func TestRebuilderScopesToolHistoryToMatchingOccurrence(t *testing.T) {
	firstCall := llm.Call{ID: "call-1", Type: "function"}
	firstCall.Function.Name = "read"
	firstCall.Function.Arguments = `{"file_path":"first.md"}`
	secondCall := llm.Call{ID: "call-1", Type: "function"}
	secondCall.Function.Name = "read"
	secondCall.Function.Arguments = `{"file_path":"second.md"}`

	sess := New("tool-history-reused-id")
	for _, event := range []Event{
		NewMessage(sess.ID(), llm.Message{Role: llm.RoleAssistant, Calls: []llm.Call{firstCall}}),
		NewToolStartedEvent(sess.ID(), ToolStartedData{
			Tool:           "read",
			Arguments:      `{"file_path":"first.md"}`,
			ID:             "call-1",
			IdempotencyKey: "turn-1:call-1",
		}),
		NewMessage(sess.ID(), llm.Message{
			Role:    llm.RoleTool,
			ToolID:  "call-1",
			Name:    "read",
			Content: "first contents",
		}),
		NewToolCompletedEvent(sess.ID(), ToolCompletedData{
			Tool:           "read",
			ID:             "call-1",
			IdempotencyKey: "turn-1:call-1",
			Output:         "first contents",
		}),
		NewMessage(sess.ID(), llm.Message{Role: llm.RoleUser, Content: "next turn"}),
		NewMessage(sess.ID(), llm.Message{Role: llm.RoleAssistant, Calls: []llm.Call{secondCall}}),
		NewToolStartedEvent(sess.ID(), ToolStartedData{
			Tool:           "read",
			Arguments:      `{"file_path":"second.md"}`,
			ID:             "call-1",
			IdempotencyKey: "turn-2:call-1",
		}),
		NewMessage(sess.ID(), llm.Message{
			Role:    llm.RoleTool,
			ToolID:  "call-1",
			Name:    "read",
			Content: "second contents",
		}),
		NewToolCompletedEvent(sess.ID(), ToolCompletedData{
			Tool:           "read",
			ID:             "call-1",
			IdempotencyKey: "turn-2:call-1",
			Output:         "second contents",
		}),
	} {
		if err := sess.Append(t.Context(), event); err != nil {
			t.Fatalf("append %s: %v", event.Type, err)
		}
	}

	entries, err := sess.EffectiveEntries()
	if err != nil {
		t.Fatalf("EffectiveEntries: %v", err)
	}
	if len(entries) != 5 {
		t.Fatalf("entries len = %d, want 5: %#v", len(entries), entries)
	}
	if tool := entries[1].Tool; tool == nil ||
		tool.Arguments != `{"file_path":"first.md"}` ||
		tool.IdempotencyKey != "turn-1:call-1" {
		t.Fatalf("first tool metadata = %#v", tool)
	}
	if tool := entries[4].Tool; tool == nil ||
		tool.Arguments != `{"file_path":"second.md"}` ||
		tool.IdempotencyKey != "turn-2:call-1" {
		t.Fatalf("second tool metadata = %#v", tool)
	}
}

func TestRebuilderDoesNotRecoverToolLifecycleAcrossTurnBoundary(t *testing.T) {
	firstCall := llm.Call{ID: "call-1", Type: "function"}
	firstCall.Function.Name = "read"
	secondCall := llm.Call{ID: "call-1", Type: "function"}
	secondCall.Function.Name = "read"

	sess := New("tool-recovery-reused-id")
	for _, event := range []Event{
		NewMessage(sess.ID(), llm.Message{Role: llm.RoleAssistant, Calls: []llm.Call{firstCall}}),
		NewMessage(sess.ID(), llm.Message{Role: llm.RoleUser, Content: "next turn"}),
		NewMessage(sess.ID(), llm.Message{Role: llm.RoleAssistant, Calls: []llm.Call{secondCall}}),
		NewToolStartedEvent(sess.ID(), ToolStartedData{
			Tool:           "read",
			Arguments:      `{"file_path":"second.md"}`,
			ID:             "call-1",
			IdempotencyKey: "turn-2:call-1",
		}),
		NewToolCompletedEvent(sess.ID(), ToolCompletedData{
			Tool:           "read",
			ID:             "call-1",
			IdempotencyKey: "turn-2:call-1",
			Output:         "second contents",
		}),
	} {
		if err := sess.Append(t.Context(), event); err != nil {
			t.Fatalf("append %s: %v", event.Type, err)
		}
	}

	messages, err := sess.EffectiveMessages()
	if err != nil {
		t.Fatalf("EffectiveMessages: %v", err)
	}
	if len(messages) != 3 {
		t.Fatalf("expected first dangling assistant omitted, got %#v", messages)
	}
	if messages[0].Role != llm.RoleUser ||
		messages[1].Role != llm.RoleAssistant ||
		messages[2].Role != llm.RoleTool ||
		messages[2].Content != "second contents" {
		t.Fatalf("unexpected effective history: %#v", messages)
	}
}

func TestRebuilderDropsDanglingAssistantToolCalls(t *testing.T) {
	call := llm.Call{ID: "call-missing", Type: "function"}
	call.Function.Name = "read"

	sess := New("dangling-tool-call")
	appendLegacyEvent(sess, NewMessage(sess.ID(), llm.Message{
		Role:  llm.RoleAssistant,
		Calls: []llm.Call{call},
	}))
	if err := sess.Append(t.Context(), NewMessage(sess.ID(), llm.Message{
		Role:    llm.RoleUser,
		Content: "next turn",
	})); err != nil {
		t.Fatalf("append user: %v", err)
	}

	messages, err := sess.EffectiveMessages()
	if err != nil {
		t.Fatalf("EffectiveMessages: %v", err)
	}
	if len(messages) != 1 || messages[0].Role != llm.RoleUser ||
		messages[0].Content != "next turn" {
		t.Fatalf("unexpected effective messages: %#v", messages)
	}
}

func TestRebuilderDropsEmptyAssistantMessagesFromSnapshots(t *testing.T) {
	sess := New("snapshot-empty-assistant")
	for _, msg := range []llm.Message{
		{Role: llm.RoleUser, Content: "old"},
		{Role: llm.RoleAssistant, Content: "cutoff"},
	} {
		if err := sess.Append(t.Context(), NewMessage(sess.ID(), msg)); err != nil {
			t.Fatalf("append message: %v", err)
		}
	}

	events := sess.Events()
	snapshot := ProjectionSnapshot{
		Strategy:      string(ProjectionTriggerManual),
		CutoffEventID: events[1].ID.String(),
		Entries: []HistoryEntry{
			{
				Message: llm.Message{
					Role:    llm.RoleSystem,
					Content: "<conversation_summary>\nsummary\n</conversation_summary>",
				},
			},
			{Message: llm.Message{Role: llm.RoleAssistant}},
			{Message: llm.Message{Role: llm.RoleUser, Content: "kept"}},
		},
	}
	if err := sess.Append(t.Context(), NewProjectionSnapshot(sess.ID(), snapshot)); err != nil {
		t.Fatalf("append projection snapshot: %v", err)
	}
	appendLegacyEvent(sess, NewMessage(sess.ID(), llm.Message{Role: llm.RoleAssistant}))
	if err := sess.Append(t.Context(), NewMessage(sess.ID(), llm.Message{Role: llm.RoleAssistant, Content: "after"})); err != nil {
		t.Fatalf("append post-snapshot assistant: %v", err)
	}

	messages, err := sess.EffectiveMessages()
	if err != nil {
		t.Fatalf("EffectiveMessages: %v", err)
	}
	if len(messages) != 3 {
		t.Fatalf("expected snapshot and post-snapshot empty assistants omitted, got %#v", messages)
	}
	if messages[0].Role != llm.RoleUser ||
		!strings.Contains(messages[0].Content, "<conversation_summary>") ||
		messages[1].Content != "kept" ||
		messages[2].Content != "after" {
		t.Fatalf("unexpected effective messages: %#v", messages)
	}
}

func TestRebuilderSanitizesSnapshotToolMetadata(t *testing.T) {
	sess := New("snapshot-tool-metadata")
	call := llm.Call{ID: "call-1", Type: "function"}
	call.Function.Name = "read"

	assistant := llm.Message{Role: llm.RoleAssistant, Calls: []llm.Call{call}}
	toolResult := llm.Message{
		Role:    llm.RoleTool,
		ToolID:  "call-1",
		Name:    "read",
		Content: "result",
	}
	for _, msg := range []llm.Message{
		{Role: llm.RoleUser, Content: "before"},
		assistant,
		toolResult,
	} {
		if err := sess.Append(t.Context(), NewMessage(sess.ID(), msg)); err != nil {
			t.Fatalf("append message: %v", err)
		}
	}

	events := sess.Events()
	snapshot := ProjectionSnapshot{
		Strategy:      string(ProjectionTriggerManual),
		CutoffEventID: events[2].ID.String(),
		Entries: []HistoryEntry{
			{
				EventID: events[0].ID.String(),
				Message: llm.Message{Role: llm.RoleUser, Content: "before"},
				Tool:    &ToolHistory{ID: "stale", Name: "bash"},
			},
			{
				EventID: events[1].ID.String(),
				Message: assistant,
				Tool:    &ToolHistory{ID: "stale", Name: "bash"},
			},
			{
				EventID: events[2].ID.String(),
				Message: toolResult,
				Tool:    &ToolHistory{ID: "wrong", Name: "bash"},
			},
		},
	}
	if err := sess.Append(t.Context(), NewProjectionSnapshot(sess.ID(), snapshot)); err != nil {
		t.Fatalf("append projection snapshot: %v", err)
	}

	entries, err := sess.EffectiveEntries()
	if err != nil {
		t.Fatalf("EffectiveEntries: %v", err)
	}
	if len(entries) != 3 {
		t.Fatalf("entries len = %d, want 3: %#v", len(entries), entries)
	}
	if entries[0].Tool != nil || entries[1].Tool != nil {
		t.Fatalf("non-tool entries kept tool metadata: %#v", entries)
	}
	if entries[2].Tool != nil {
		t.Fatalf("mismatched tool metadata survived: %#v", entries[2].Tool)
	}
}

func TestRebuilderNormalizesSnapshotToolMetadata(t *testing.T) {
	sess := New("snapshot-tool-metadata-normalized")
	call := llm.Call{ID: "call-1", Type: "function"}
	call.Function.Name = "read"

	assistant := llm.Message{Role: llm.RoleAssistant, Calls: []llm.Call{call}}
	toolResult := llm.Message{
		Role:    llm.RoleTool,
		ToolID:  "call-1",
		Name:    "read",
		Content: "result",
	}
	for _, msg := range []llm.Message{
		assistant,
		toolResult,
	} {
		if err := sess.Append(t.Context(), NewMessage(sess.ID(), msg)); err != nil {
			t.Fatalf("append message: %v", err)
		}
	}

	events := sess.Events()
	snapshot := ProjectionSnapshot{
		Strategy:      string(ProjectionTriggerManual),
		CutoffEventID: events[1].ID.String(),
		Entries: []HistoryEntry{
			{EventID: events[0].ID.String(), Message: assistant},
			{
				EventID: events[1].ID.String(),
				Message: toolResult,
				Tool:    &ToolHistory{Arguments: `{"file_path":"AGENTS.md"}`},
			},
		},
	}
	if err := sess.Append(t.Context(), NewProjectionSnapshot(sess.ID(), snapshot)); err != nil {
		t.Fatalf("append projection snapshot: %v", err)
	}

	entries, err := sess.EffectiveEntries()
	if err != nil {
		t.Fatalf("EffectiveEntries: %v", err)
	}
	tool := entries[1].Tool
	if tool == nil || tool.ID != "call-1" || tool.Name != "read" ||
		tool.Arguments != `{"file_path":"AGENTS.md"}` {
		t.Fatalf("unexpected normalized tool metadata: %#v", tool)
	}
}

func TestRebuilderRebuildEntriesInjectsWorkingSetAfterSummary(t *testing.T) {
	sess := New("compacted")
	oldUser := llm.Message{Role: llm.RoleUser, Content: "old"}
	cutoff := llm.Message{Role: llm.RoleAssistant, Content: "cutoff"}
	recent := llm.Message{Role: llm.RoleUser, Content: "recent"}

	for _, msg := range []llm.Message{oldUser, cutoff, recent} {
		if err := sess.Append(t.Context(), NewMessage(sess.ID(), msg)); err != nil {
			t.Fatalf("append message: %v", err)
		}
	}

	events := sess.Events()
	snapshot := CompactionSnapshot{
		Strategy:      "summarize",
		CutoffEventID: events[1].ID.String(),
		Entries: []HistoryEntry{
			{
				EventID: "summary-event",
				Message: llm.Message{
					Role:    llm.RoleSystem,
					Content: "<conversation_summary>\nsummary\n</conversation_summary>",
				},
			},
		},
		ReadFiles:     []string{"a.txt", "c.txt", "a.txt"},
		ModifiedFiles: []string{"b.txt", "c.txt"},
	}
	if err := sess.Append(t.Context(), NewCompactionEvent(sess.ID(), snapshot)); err != nil {
		t.Fatalf("append compaction: %v", err)
	}

	entries, err := NewRebuilder().RebuildEntries(sess)
	if err != nil {
		t.Fatalf("RebuildEntries: %v", err)
	}
	if len(entries) != 3 {
		t.Fatalf("expected 3 entries, got %d", len(entries))
	}
	if entries[0].Message.Role != llm.RoleUser ||
		!strings.Contains(entries[0].Message.Content, "<conversation_summary>") {
		t.Fatalf("expected summary first, got %#v", entries[0])
	}
	if entries[1].Message.Role != llm.RoleUser ||
		!strings.Contains(entries[1].Message.Content, "<working_set>") {
		t.Fatalf("expected working set second, got %#v", entries[1])
	}
	if !strings.Contains(entries[1].Message.Content, "Modified files:\n- b.txt\n- c.txt\n") {
		t.Fatalf("expected modified files block, got %q", entries[1].Message.Content)
	}
	if !strings.Contains(entries[1].Message.Content, "Read-only files:\n- a.txt\n") {
		t.Fatalf("expected read-only file block, got %q", entries[1].Message.Content)
	}
	if entries[2].Message.Content != "recent" {
		t.Fatalf("expected recent message last, got %q", entries[2].Message.Content)
	}
}

func TestEffectiveMessagesUsesRebuilderWorkingSetInjection(t *testing.T) {
	sess := New("effective")
	for _, msg := range []llm.Message{
		{Role: llm.RoleUser, Content: "old"},
		{Role: llm.RoleAssistant, Content: "cutoff"},
		{Role: llm.RoleUser, Content: "recent"},
	} {
		if err := sess.Append(t.Context(), NewMessage(sess.ID(), msg)); err != nil {
			t.Fatalf("append message: %v", err)
		}
	}

	events := sess.Events()
	if err := sess.Append(t.Context(), NewCompactionEvent(sess.ID(), CompactionSnapshot{
		Strategy:      "summarize",
		CutoffEventID: events[1].ID.String(),
		Entries: []HistoryEntry{
			{
				Message: llm.Message{
					Role:    llm.RoleSystem,
					Content: "<conversation_summary>\nsummary\n</conversation_summary>",
				},
			},
		},
		ModifiedFiles: []string{"main.go"},
	})); err != nil {
		t.Fatalf("append compaction: %v", err)
	}

	messages, err := sess.EffectiveMessages()
	if err != nil {
		t.Fatalf("EffectiveMessages: %v", err)
	}
	if len(messages) != 3 {
		t.Fatalf("expected 3 messages, got %d", len(messages))
	}
	if !strings.Contains(messages[1].Content, "<working_set>") {
		t.Fatalf("expected working set block in effective messages, got %q", messages[1].Content)
	}
}

func TestRebuilderRebuildEntriesUsesLatestProjectionSnapshot(t *testing.T) {
	sess := New("projected")
	oldUser := llm.Message{Role: llm.RoleUser, Content: "old"}
	cutoff := llm.Message{Role: llm.RoleAssistant, Content: "cutoff"}
	recent := llm.Message{Role: llm.RoleUser, Content: "recent"}

	for _, msg := range []llm.Message{oldUser, cutoff, recent} {
		if err := sess.Append(t.Context(), NewMessage(sess.ID(), msg)); err != nil {
			t.Fatalf("append message: %v", err)
		}
	}

	events := sess.Events()
	snapshot := ProjectionSnapshot{
		Strategy:      string(ProjectionTriggerTime),
		CutoffEventID: events[2].ID.String(),
		Entries: []HistoryEntry{
			{
				Message: llm.Message{
					Role:    llm.RoleSystem,
					Content: "<conversation_summary>\nsummary\n</conversation_summary>",
				},
			},
			{
				EventID: events[2].ID.String(),
				Message: llm.Message{Role: llm.RoleUser, Content: "recent"},
			},
		},
		ReadFiles:     []string{"a.txt"},
		ModifiedFiles: []string{"b.txt"},
	}
	if err := sess.Append(t.Context(), NewProjectionSnapshot(sess.ID(), snapshot)); err != nil {
		t.Fatalf("append projection snapshot: %v", err)
	}

	after := llm.Message{Role: llm.RoleAssistant, Content: "after"}
	if err := sess.Append(t.Context(), NewMessage(sess.ID(), after)); err != nil {
		t.Fatalf("append after: %v", err)
	}

	entries, err := NewRebuilder().RebuildEntries(sess)
	if err != nil {
		t.Fatalf("RebuildEntries: %v", err)
	}
	if len(entries) != 4 {
		t.Fatalf("expected 4 entries, got %d", len(entries))
	}
	if entries[0].Message.Role != llm.RoleUser ||
		!strings.Contains(entries[0].Message.Content, "<conversation_summary>") {
		t.Fatalf("expected summary first, got %#v", entries[0])
	}
	if entries[1].Message.Role != llm.RoleUser ||
		!strings.Contains(entries[1].Message.Content, "<working_set>") {
		t.Fatalf("expected working set second, got %#v", entries[1])
	}
	if !strings.Contains(entries[1].Message.Content, "Modified files:\n- b.txt\n") {
		t.Fatalf("expected modified file block, got %q", entries[1].Message.Content)
	}
	if entries[2].Message.Content != "recent" {
		t.Fatalf("expected recent entry third, got %q", entries[2].Message.Content)
	}
	if entries[3].Message.Content != "after" {
		t.Fatalf("expected post-snapshot entry last, got %q", entries[3].Message.Content)
	}
}

func TestRebuilderAcceptsSnapshotCutoffOnHiddenEvent(t *testing.T) {
	sess := New("hidden-cutoff")
	if err := sess.Append(t.Context(), NewMessage(sess.ID(), llm.Message{
		Role:    llm.RoleUser,
		Content: "before",
	})); err != nil {
		t.Fatalf("append before: %v", err)
	}
	hidden := NewEvent(sess.ID(), TurnStarted, map[string]string{"turn": "1"})
	if err := sess.Append(t.Context(), hidden); err != nil {
		t.Fatalf("append hidden event: %v", err)
	}
	snapshot := ProjectionSnapshot{
		Strategy:      string(ProjectionTriggerManual),
		CutoffEventID: hidden.ID.String(),
		Entries: []HistoryEntry{{
			EventID: sess.Events()[0].ID.String(),
			Message: llm.Message{Role: llm.RoleUser, Content: "before"},
		}},
	}
	if err := sess.Append(t.Context(), NewProjectionSnapshot(sess.ID(), snapshot)); err != nil {
		t.Fatalf("append projection snapshot: %v", err)
	}
	if err := sess.Append(t.Context(), NewMessage(sess.ID(), llm.Message{
		Role:    llm.RoleAssistant,
		Content: "after",
	})); err != nil {
		t.Fatalf("append after: %v", err)
	}

	entries, err := NewRebuilder().RebuildEntries(sess)
	if err != nil {
		t.Fatalf("RebuildEntries: %v", err)
	}
	if len(entries) != 2 {
		t.Fatalf("expected snapshot entry plus post-cutoff message, got %#v", entries)
	}
	if entries[0].Message.Content != "before" || entries[1].Message.Content != "after" {
		t.Fatalf("unexpected rebuilt entries: %#v", entries)
	}
}
