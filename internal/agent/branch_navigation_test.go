package agent

import (
	"context"
	"errors"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func TestBranchSummaryMessagesBoundRecentContextAndSkipToolResults(t *testing.T) {
	timestamp := time.Now()
	entries := []session.Entry{
		&session.MessageEntry{
			EntryBase: session.EntryBase{ID: "user-old", Timestamp: timestamp},
			Message:   session.NewUserText("old branch context that should be trimmed", timestamp),
		},
		&session.MessageEntry{
			EntryBase: session.EntryBase{ID: "tool-result", Timestamp: timestamp},
			Message: &session.ToolResultMessage{
				ToolCallID: "call-1",
				ToolName:   "read",
				Content:    []session.Content{session.TextContent{Text: "File: secret.go"}},
			},
		},
		&session.MessageEntry{
			EntryBase: session.EntryBase{ID: "user-new", Timestamp: timestamp},
			Message:   session.NewUserText("new", timestamp),
		},
	}

	messages := branchSummaryMessages(entries, 1)
	if len(messages) != 1 || session.MessageText(messages[0]) != "new" {
		t.Fatalf("bounded branch messages = %#v, want only newest user message", messages)
	}
	for _, message := range branchSummaryMessages(entries, -1) {
		if _, ok := message.(*session.ToolResultMessage); ok {
			t.Fatal("branch summary prompt included a tool result")
		}
	}

	fileOps := branchSummaryFileOperations(entries)
	if len(fileOps.ReadFiles) != 1 || fileOps.ReadFiles[0] != "secret.go" {
		t.Fatalf("branch file operations = %#v, want read secret.go", fileOps.ReadFiles)
	}
}

func TestNavigateTreeSummarizesAbandonedBranchAndPersistsFromID(t *testing.T) {
	ctx := context.Background()
	path := filepath.Join(t.TempDir(), "session.db")
	store, err := session.NewSQLiteStore(path, "branch-navigation")
	if err != nil {
		t.Fatal(err)
	}
	sess := session.NewSession(store, 64)
	a, err := sess.AppendMessage(ctx, session.NewUserText("A", time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	if _, err := sess.AppendMessage(ctx, session.NewUserText("B", time.Now())); err != nil {
		t.Fatal(err)
	}
	oldLeafID, err := sess.AppendMessage(ctx, session.NewUserText("C", time.Now()))
	if err != nil {
		t.Fatal(err)
	}

	var request llm.Request
	h := NewController(ControllerConfig{
		Session: sess,
		Store:   store,
		Model:   llm.Model{ID: "summary-model", MaxTokens: 2048},
		StreamFn: func(_ context.Context, req *llm.Request) (llm.Stream, error) {
			request = *req
			return &mockStream{chunks: []*llm.Chunk{{Content: "branch work", StopReason: "stop"}}}, nil
		},
		Compaction: CompactionSettings{ReserveTokens: 1024},
	})
	defer h.Close()

	result, err := h.NavigateTree(ctx, a, NavigateOptions{
		Summarize:          true,
		CustomInstructions: "focus on unfinished work",
	})
	if err != nil {
		t.Fatal(err)
	}
	if result.SummaryEntryID == "" {
		t.Fatal("summary entry ID is empty")
	}
	if result.LeafID != sess.GetLeafID() || result.LeafID != result.SummaryEntryID {
		t.Fatalf("navigation leaf = %q, want summary leaf %q", result.LeafID, result.SummaryEntryID)
	}
	if request.Model != "summary-model" || len(request.Messages) != 2 {
		t.Fatalf(
			"summary request = model %q with %d messages, want summary-model with 2",
			request.Model,
			len(request.Messages),
		)
	}
	if !strings.Contains(request.Messages[1].Content, "focus on unfinished work") {
		t.Fatalf("summary request omitted custom instructions: %q", request.Messages[1].Content)
	}

	branch, err := store.Branch(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(branch) != 2 {
		t.Fatalf("branch length = %d, want target plus summary", len(branch))
	}
	summary, ok := branch[1].(*session.BranchSummaryEntry)
	if !ok {
		t.Fatalf("branch entry 1 = %T, want BranchSummaryEntry", branch[1])
	}
	if summary.FromID != oldLeafID || !strings.Contains(summary.Summary, "branch work") {
		t.Fatalf(
			"summary = from %q text %q, want from %q and generated text",
			summary.FromID,
			summary.Summary,
			oldLeafID,
		)
	}

	snapshot, err := sess.BuildContext(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(snapshot.Messages) != 2 || session.MessageText(snapshot.Messages[0]) != "A" ||
		!strings.Contains(session.MessageText(snapshot.Messages[1]), "branch work") {
		t.Fatalf("context after navigation = %#v, want A followed by branch summary", snapshot.Messages)
	}

	if err := store.Close(); err != nil {
		t.Fatal(err)
	}
	reopened, err := session.NewSQLiteStore(path, "branch-navigation")
	if err != nil {
		t.Fatal(err)
	}
	defer reopened.Close()
	reopenedSummary, ok := mustBranchEntry(t, reopened, result.SummaryEntryID).(*session.BranchSummaryEntry)
	if !ok || reopenedSummary.FromID != oldLeafID {
		t.Fatalf("reopened summary = %#v, want persisted FromID %q", reopenedSummary, oldLeafID)
	}
}

func TestNavigateTreeReportsSelectedBranchModel(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	if _, err := sess.AppendMessage(ctx, session.NewUserText("target", time.Now())); err != nil {
		t.Fatal(err)
	}
	targetModelLeaf, err := sess.AppendModelChange(ctx, "openrouter", "target-model")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := sess.AppendMessage(ctx, session.NewUserText("abandoned", time.Now())); err != nil {
		t.Fatal(err)
	}

	h := NewController(ControllerConfig{
		Session: sess,
		Store:   store,
		Model:   llm.Model{Provider: "openrouter", ID: "current-model"},
	})
	defer h.Close()

	result, err := h.NavigateTree(ctx, targetModelLeaf, NavigateOptions{})
	if err != nil {
		t.Fatal(err)
	}
	if result.ActiveProvider != "openrouter" || result.ActiveModel != "target-model" {
		t.Fatalf(
			"selected branch model = %s/%s, want openrouter/target-model",
			result.ActiveProvider,
			result.ActiveModel,
		)
	}
}

func TestNavigateTreeCancellationLeavesSessionUnchanged(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	targetID, err := sess.AppendMessage(ctx, session.NewUserText("target", time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	if _, err := sess.AppendMessage(ctx, session.NewUserText("abandoned", time.Now())); err != nil {
		t.Fatal(err)
	}
	oldLeafID := sess.GetLeafID()
	started := make(chan struct{})
	h := NewController(ControllerConfig{
		Session: sess,
		Store:   store,
		Model:   llm.Model{ID: "summary-model"},
		StreamFn: func(ctx context.Context, _ *llm.Request) (llm.Stream, error) {
			close(started)
			<-ctx.Done()
			return nil, ctx.Err()
		},
	})
	defer h.Close()

	resultCh := make(chan error, 1)
	go func() {
		_, err := h.NavigateTree(ctx, targetID, NavigateOptions{Summarize: true})
		resultCh <- err
	}()
	<-started
	if _, _, err := h.Abort(); err != nil {
		t.Fatal(err)
	}
	if err := <-resultCh; !errors.Is(err, context.Canceled) {
		t.Fatalf("navigation error = %v, want context.Canceled", err)
	}
	if got := sess.GetLeafID(); got != oldLeafID {
		t.Fatalf("leaf after cancellation = %q, want %q", got, oldLeafID)
	}
}

func mustBranchEntry(t *testing.T, store session.Store, id string) session.Entry {
	t.Helper()
	entry, err := store.GetEntry(context.Background(), id)
	if err != nil {
		t.Fatal(err)
	}
	return entry
}
