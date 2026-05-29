package session

import (
	"context"
	"testing"
	"time"

	"github.com/nijaru/ion/internal/llm"
)

func TestExportRun(t *testing.T) {
	sess := New("test-session")

	// Add some events
	now := time.Now()

	// User message
	e1 := NewEvent(sess.ID(), MessageAdded, llm.Message{
		Role:    llm.RoleUser,
		Content: "Hello",
	})
	e1.Timestamp = now
	e1.Cost = 0.01
	_ = sess.Append(context.Background(), e1)

	// Assistant response with tool call
	e2 := NewEvent(sess.ID(), MessageAdded, llm.Message{
		Role:    llm.RoleAssistant,
		Content: "Let me check",
		Calls: []llm.Call{
			{ID: "call_1", Type: "function", Function: struct {
				Name      string "json:\"name\""
				Arguments string "json:\"arguments\""
			}{Name: "search", Arguments: "{}"}},
		},
	})
	e2.Timestamp = now.Add(time.Second)
	e2.Cost = 0.05
	sess.Append(context.Background(), e2)

	// Tool result
	e3 := NewEvent(sess.ID(), MessageAdded, llm.Message{
		Role:    llm.RoleTool,
		Content: "Result data",
		ToolID:  "call_1",
		Name:    "search",
	})
	e3.Timestamp = now.Add(2 * time.Second)
	sess.Append(context.Background(), e3)

	// Second assistant response
	e4 := NewEvent(sess.ID(), MessageAdded, llm.Message{
		Role:    llm.RoleAssistant,
		Content: "The result is data",
	})
	e4.Timestamp = now.Add(3 * time.Second)
	e4.Cost = 0.02
	sess.Append(context.Background(), e4)

	traj, err := ExportRun(sess)
	if err != nil {
		t.Fatal(err)
	}

	if traj.SessionID != "test-session" {
		t.Errorf("expected session test-session, got %s", traj.SessionID)
	}
	if traj.TotalCost != 0.08 {
		t.Errorf("expected cost 0.08, got %f", traj.TotalCost)
	}

	if len(traj.Turns) != 2 {
		t.Fatalf("expected 2 turns, got %d", len(traj.Turns))
	}

	turn1 := traj.Turns[0]
	if len(turn1.Input) != 1 || turn1.Input[0].Role != llm.RoleUser {
		t.Errorf("expected turn 1 input to be user message")
	}
	if turn1.Output.Role != llm.RoleAssistant || len(turn1.ToolCalls) != 1 {
		t.Errorf("expected turn 1 output to be assistant with 1 tool call")
	}
	if len(turn1.ToolResults) != 1 || turn1.ToolResults[0].Content != "Result data" {
		t.Errorf("expected turn 1 to have 1 tool result")
	}

	turn2 := traj.Turns[1]
	if len(turn2.Input) != 1 {
		t.Fatalf(
			"expected turn 2 input to include the prior tool result, got %d entries",
			len(turn2.Input),
		)
	}
	if turn2.Input[0].Role != llm.RoleTool || turn2.Input[0].Content != "Result data" {
		t.Errorf("expected turn 2 input to carry the tool result, got %+v", turn2.Input[0])
	}
	if turn2.Output.Role != llm.RoleAssistant || turn2.Output.Content != "The result is data" {
		t.Errorf("expected turn 2 output to be final assistant message")
	}
}

func TestExportRunIncludesModelVisibleContextInTurnInput(t *testing.T) {
	sess := New("context-run")
	if err := sess.AppendContext(t.Context(), ContextEntry{
		Kind:    ContextKindBootstrap,
		Content: "workspace context",
	}); err != nil {
		t.Fatalf("AppendContext: %v", err)
	}
	if err := sess.Append(t.Context(), NewMessage(sess.ID(), llm.Message{
		Role:    llm.RoleUser,
		Content: "Hello",
	})); err != nil {
		t.Fatalf("append user: %v", err)
	}
	if err := sess.Append(t.Context(), NewMessage(sess.ID(), llm.Message{
		Role:    llm.RoleAssistant,
		Content: "Hi",
	})); err != nil {
		t.Fatalf("append assistant: %v", err)
	}

	traj, err := ExportRun(sess)
	if err != nil {
		t.Fatal(err)
	}
	if len(traj.Turns) != 1 {
		t.Fatalf("expected 1 turn, got %d", len(traj.Turns))
	}
	input := traj.Turns[0].Input
	if len(input) != 2 {
		t.Fatalf("expected context plus user input, got %#v", input)
	}
	if input[0].Role != llm.RoleUser || input[0].Content != "workspace context" {
		t.Fatalf("expected context first, got %#v", input[0])
	}
	if input[1].Role != llm.RoleUser || input[1].Content != "Hello" {
		t.Fatalf("expected user input second, got %#v", input[1])
	}
	entries := traj.Turns[0].InputEntries
	if len(entries) != 2 {
		t.Fatalf("expected context plus user input entries, got %#v", entries)
	}
	if entries[0].EventType != ContextAdded ||
		entries[0].ContextKind != ContextKindBootstrap {
		t.Fatalf("expected typed context entry first, got %#v", entries[0])
	}
	if entries[1].EventType != MessageAdded {
		t.Fatalf("expected transcript entry second, got %#v", entries[1])
	}
}

func TestExportRunDemotesPrivilegedTranscriptInput(t *testing.T) {
	sess := New("export-system")
	// System prompt is now stored separately, not as a message event
	sess.SetSystemPrompt("durable notice")
	if err := sess.Append(t.Context(), NewMessage(sess.ID(), llm.Message{
		Role:    llm.RoleUser,
		Content: "developer notice",
	})); err != nil {
		t.Fatalf("append user: %v", err)
	}
	if err := sess.Append(t.Context(), NewMessage(sess.ID(), llm.Message{
		Role:    llm.RoleAssistant,
		Content: "ok",
	})); err != nil {
		t.Fatalf("append assistant: %v", err)
	}

	traj, err := ExportRun(sess)
	if err != nil {
		t.Fatal(err)
	}
	if len(traj.Turns) != 1 {
		t.Fatalf("expected 1 turn, got %d", len(traj.Turns))
	}
	// System prompt is stored separately, not in the conversation input
	if len(traj.Turns[0].Input) != 1 {
		t.Fatalf(
			"expected 1 input message (system prompt stored separately), got %d",
			len(traj.Turns[0].Input),
		)
	}
	if traj.Turns[0].Input[0].Role != llm.RoleUser {
		t.Fatalf(
			"expected exported user input, got %#v",
			traj.Turns[0].Input[0],
		)
	}
	for _, entry := range traj.Turns[0].InputEntries {
		if entry.EventType != MessageAdded {
			t.Fatalf("expected transcript marker, got %#v", entry)
		}
	}
}

func TestExportRunTreeIncludesChildRuns(t *testing.T) {
	parent := New("parent")
	child := New("child-session")

	if err := child.Append(t.Context(), NewMessage(child.ID(), llm.Message{
		Role:    llm.RoleUser,
		Content: "Child task",
	})); err != nil {
		t.Fatalf("append child user: %v", err)
	}
	if err := child.Append(t.Context(), NewMessage(child.ID(), llm.Message{
		Role:    llm.RoleAssistant,
		Content: "Child result",
	})); err != nil {
		t.Fatalf("append child assistant: %v", err)
	}

	if err := parent.Append(t.Context(), NewChildRequestedEvent(parent.ID(), ChildRequestedData{
		ChildID:        "child-1",
		ChildSessionID: child.ID(),
		AgentID:        "worker",
		Mode:           ChildModeHandoff,
		Task:           "Do the child task",
	})); err != nil {
		t.Fatalf("append child requested: %v", err)
	}
	if err := parent.Append(t.Context(), NewArtifactRecordedEvent(parent.ID(), ArtifactRecordedData{
		ChildID: "child-1",
		Artifact: ArtifactRef{
			ID:   "file-ref-1",
			Kind: ArtifactKindWorkspaceFileRef,
			URI:  "workspace://notes.txt",
		},
	})); err != nil {
		t.Fatalf("append file ref artifact: %v", err)
	}
	if err := parent.Append(t.Context(), NewArtifactRecordedEvent(parent.ID(), ArtifactRecordedData{
		ChildID: "child-1",
		Artifact: ArtifactRef{
			ID:   "artifact-1",
			Kind: "patch",
			URI:  "/tmp/patch.diff",
		},
	})); err != nil {
		t.Fatalf("append artifact recorded: %v", err)
	}
	if err := parent.Append(t.Context(), NewChildCompletedEvent(parent.ID(), ChildCompletedData{
		ChildID:        "child-1",
		ChildSessionID: child.ID(),
		Summary:        "Child finished successfully",
	})); err != nil {
		t.Fatalf("append child completed: %v", err)
	}

	tree, err := ExportRunTree(parent, func(sessionID string) (*Session, error) {
		if sessionID == child.ID() {
			return child, nil
		}
		return nil, nil
	})
	if err != nil {
		t.Fatalf("export run tree: %v", err)
	}
	if len(tree.ChildRuns) != 1 {
		t.Fatalf("expected 1 child run, got %d", len(tree.ChildRuns))
	}

	childRun := tree.ChildRuns[0]
	if childRun.Status != ChildStatusCompleted {
		t.Fatalf("child status = %q, want completed", childRun.Status)
	}
	if childRun.Summary != "Child finished successfully" {
		t.Fatalf("child summary = %q", childRun.Summary)
	}
	if len(childRun.Artifacts) != 1 || childRun.Artifacts[0].Kind != "patch" {
		t.Fatalf("unexpected child artifacts: %#v", childRun.Artifacts)
	}
	if childRun.Run == nil || childRun.Run.SessionID != child.ID() {
		t.Fatalf("expected nested child run, got %#v", childRun.Run)
	}
	if len(childRun.Run.Turns) != 1 {
		t.Fatalf("expected 1 child turn, got %d", len(childRun.Run.Turns))
	}
}
