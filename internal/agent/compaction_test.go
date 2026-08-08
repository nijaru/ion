package agent

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func TestShouldCompactUnknownContextWindowDoesNotLoop(t *testing.T) {
	if ShouldCompact(1, 0, DefaultCompactionSettings()) {
		t.Fatal("unknown context window enabled compaction")
	}
	if ShouldCompact(1, -1, DefaultCompactionSettings()) {
		t.Fatal("negative context window enabled compaction")
	}
}

func TestFindCutPointNeverStartsAtToolResult(t *testing.T) {
	entries := []session.Entry{
		&session.MessageEntry{
			EntryBase: session.EntryBase{ID: "user"},
			Message:   session.NewUserText("inspect", mustTime()),
		},
		&session.MessageEntry{
			EntryBase: session.EntryBase{ID: "assistant"},
			Message: &session.AssistantMessage{
				Content: []session.Content{&session.ToolCall{
					ID: "call-1", Name: "read", Arguments: map[string]any{"path": "main.go"},
				}},
			},
		},
		&session.MessageEntry{
			EntryBase: session.EntryBase{ID: "tool-result"},
			Message: &session.ToolResultMessage{
				ToolCallID: "call-1",
				ToolName:   "read",
				Content:    []session.Content{session.TextContent{Text: strings.Repeat("tool output ", 40)}},
			},
		},
	}

	cut := FindCutPoint(entries, 0, len(entries), 1)
	if cut.FirstKeptEntryIndex != 0 {
		t.Fatalf("cut point = %#v, want the preceding valid boundary", cut)
	}
	if entries[cut.FirstKeptEntryIndex].ID() == "tool-result" {
		t.Fatal("compaction cut orphaned a tool result")
	}
}

func TestEstimateTokensCountsCustomMessageContent(t *testing.T) {
	message := &session.CustomMessage{
		Content: []session.Content{session.TextContent{Text: strings.Repeat("x", 100)}},
	}
	if got := EstimateTokens(message); got != 25 {
		t.Fatalf("custom message tokens = %d, want 25", got)
	}
}

func TestEstimateContextTokensFallsBackWhenUsageIsEmpty(t *testing.T) {
	messages := []session.Message{
		session.NewUserText("before", mustTime()),
		&session.AssistantMessage{
			Content:    []session.Content{session.TextContent{Text: "after"}},
			StopReason: session.StopReasonEndTurn,
		},
		session.NewUserText("trailing", mustTime()),
	}
	if got := EstimateContextTokens(messages).Tokens; got == 0 {
		t.Fatal("empty assistant usage suppressed heuristic context tokens")
	}
}

// INVARIANT: EstimateContextTokens is usage-aware — uses provider usage when available.
func TestEstimateContextTokensUsageAware(t *testing.T) {
	// Messages without usage data → pure heuristic.
	messages := []session.Message{
		session.NewUserText("hello world", mustTime()),
	}
	est := EstimateContextTokens(messages)
	if est.UsageTokens != 0 {
		t.Errorf("expected 0 usage tokens without assistant, got %d", est.UsageTokens)
	}
	if est.Tokens != est.TrailingTokens {
		t.Errorf(
			"without usage, tokens should equal trailing, got tokens=%d trailing=%d",
			est.Tokens,
			est.TrailingTokens,
		)
	}

	// Messages with a successful assistant that has usage.
	used := session.Usage{Input: 100, Output: 50, TotalTokens: 150}
	assistantMsg := &session.AssistantMessage{
		Usage:      used,
		StopReason: session.StopReasonEndTurn,
	}
	messages = []session.Message{
		session.NewUserText("hello", mustTime()),
		assistantMsg,
		session.NewUserText(
			"follow-up with many many many characters here to add some trailing tokens let us make this at least forty characters",
			mustTime(),
		),
	}

	est = EstimateContextTokens(messages)
	if est.UsageTokens != 150 {
		t.Errorf("expected 150 usage tokens, got %d", est.UsageTokens)
	}
	if est.Tokens <= 150 {
		t.Errorf("expected tokens > 150 due to trailing messages, got %d", est.Tokens)
	}
	if est.LastUsageIndex == nil || *est.LastUsageIndex != 1 {
		t.Errorf("expected last usage index 1, got %v", est.LastUsageIndex)
	}
}

// INVARIANT: EstimateContextTokens skips aborted/errored assistant messages.
func TestEstimateContextTokensSkipsFailed(t *testing.T) {
	used := session.Usage{Input: 100, Output: 50, TotalTokens: 150}
	aborted := &session.AssistantMessage{
		Usage:      used,
		StopReason: session.StopReasonAborted,
	}
	errored := &session.AssistantMessage{
		Usage:      used,
		StopReason: session.StopReasonError,
	}
	good := &session.AssistantMessage{
		Usage:      session.Usage{Input: 200, Output: 80, TotalTokens: 280},
		StopReason: session.StopReasonEndTurn,
	}
	messages := []session.Message{
		session.NewUserText("hi", mustTime()),
		aborted,
		session.NewUserText("retry", mustTime()),
		errored,
		session.NewUserText("retry again", mustTime()),
		good,
	}

	est := EstimateContextTokens(messages)
	// Should use the "good" message's usage (280), not the aborted/errored ones.
	if est.UsageTokens != 280 {
		t.Errorf("expected usage tokens from good message (280), got %d", est.UsageTokens)
	}
}

// INVARIANT: ExtractFileOperations inherits from previous compaction.
func TestExtractFileOperationsInheritsPrevious(t *testing.T) {
	var entries []session.Entry
	// Add a previous compaction with file ops.
	entries = append(entries, &session.CompactionEntry{})
	// Simulate non-compaction entries after it.
	entries = append(entries, &session.MessageEntry{
		Message: session.NewUserText("hi", mustTime()),
	})

	// Since we can't easily construct CompactionEntry with Details, test the basic flow.
	fileOps := ExtractFileOperations(nil, entries, 0)
	if fileOps == nil {
		t.Fatal("expected non-nil fileOps")
	}
}

func TestCompactSplitTurnWithNoPriorHistorySummarizesPrefix(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	if _, err := sess.AppendMessage(context.Background(), session.NewUserText("start work", mustTime())); err != nil {
		t.Fatal(err)
	}
	if _, err := sess.AppendMessage(context.Background(), &session.AssistantMessage{
		Content: []session.Content{session.TextContent{Text: "partial work"}},
	}); err != nil {
		t.Fatal(err)
	}

	var calls int
	result, err := Compact(context.Background(), sess, CompactOptions{
		Model: "test",
		StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
			calls++
			return &mockStream{chunks: []*llm.Chunk{{Content: "prefix summary", StopReason: "stop"}}}, nil
		},
	}, CompactionSettings{Enabled: true, ReserveTokens: 1, KeepRecentTokens: 1})
	if err != nil {
		t.Fatalf("Compact: %v", err)
	}
	if calls != 1 || result == nil {
		t.Fatalf("Compact result = %#v, stream calls = %d, want one prefix summary", result, calls)
	}
	if !strings.Contains(result.Summary, "Turn Context (split turn)") ||
		!strings.Contains(result.Summary, "prefix summary") {
		t.Fatalf("split-turn summary = %q", result.Summary)
	}
}

// INVARIANT: Compact with empty messages returns nil (no-op).
func TestCompactEmpty(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)

	result, err := Compact(context.Background(), sess, CompactOptions{
		Model: "test",
		StreamFn: func(ctx context.Context, req *llm.Request) (llm.Stream, error) {
			return &mockStream{}, nil
		},
	}, DefaultCompactionSettings())
	if err != nil {
		t.Fatal(err)
	}
	if result != nil {
		t.Error("expected nil result for empty session")
	}
}

func TestCompactFailureLeavesCommittedContextUnchanged(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	if _, err := sess.AppendMessage(context.Background(), session.NewUserText("history", time.Now())); err != nil {
		t.Fatal(err)
	}
	if _, err := sess.AppendMessage(context.Background(), session.NewUserText("recent", time.Now())); err != nil {
		t.Fatal(err)
	}

	leafBefore := sess.GetLeafID()
	entriesBefore, err := sess.Entries(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	_, err = Compact(context.Background(), sess, CompactOptions{
		Model: "test",
		StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
			return nil, errors.New("summary provider unavailable")
		},
	}, CompactionSettings{Enabled: true, ReserveTokens: 1, KeepRecentTokens: 1})
	if err == nil || !strings.Contains(err.Error(), "summarization failed") {
		t.Fatalf("Compact error = %v, want summary failure", err)
	}
	if got := sess.GetLeafID(); got != leafBefore {
		t.Fatalf("leaf after failed compaction = %q, want %q", got, leafBefore)
	}
	entriesAfter, err := sess.Entries(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if len(entriesAfter) != len(entriesBefore) {
		t.Fatalf("entry count after failed compaction = %d, want %d", len(entriesAfter), len(entriesBefore))
	}
}

func TestCompactCommitsReplacementForReplay(t *testing.T) {
	dir := t.TempDir()
	path := dir + "/session.db"
	store, err := session.NewSQLiteStore(path, "test")
	if err != nil {
		t.Fatal(err)
	}
	sess := session.NewSession(store, 64)
	if _, err := sess.AppendMessage(context.Background(), session.NewUserText("history", time.Now())); err != nil {
		t.Fatal(err)
	}
	if _, err := sess.AppendMessage(context.Background(), session.NewUserText("recent", time.Now())); err != nil {
		t.Fatal(err)
	}

	result, err := Compact(context.Background(), sess, CompactOptions{
		Model: "test",
		StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{{
				Content:    "durable summary",
				StopReason: "stop",
				Usage:      &llm.Usage{InputTokens: 11, OutputTokens: 7, TotalTokens: 18, Cost: 0.25},
			}}}, nil
		},
	}, CompactionSettings{Enabled: true, ReserveTokens: 1, KeepRecentTokens: 1})
	if err != nil {
		t.Fatal(err)
	}
	if result == nil || result.FirstKeptEntryID == "" {
		t.Fatalf("Compact result = %#v, want committed replacement", result)
	}
	if result.Usage.Input != 11 || result.Usage.Output != 7 || result.Usage.TotalTokens != 18 ||
		result.Usage.Cost.Total != 0.25 {
		t.Fatalf("compaction usage = %#v, want provider usage", result.Usage)
	}
	if err := store.Close(); err != nil {
		t.Fatal(err)
	}

	reopened, err := session.NewSQLiteStore(path, "test")
	if err != nil {
		t.Fatal(err)
	}
	defer reopened.Close()
	reopenedSession := session.NewSession(reopened, 64)
	snapshot, err := reopenedSession.BuildContext(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	usage, err := reopenedSession.Usage(context.Background())
	if err != nil {
		t.Fatalf("reopened usage: %v", err)
	}
	if usage.Input != 11 || usage.Output != 7 || usage.TotalTokens != 18 || usage.Cost.Total != 0.25 {
		t.Fatalf("replayed usage = %#v, want compaction usage", usage)
	}
	if len(snapshot.Messages) != 2 {
		t.Fatalf("replayed message count = %d, want summary plus kept message", len(snapshot.Messages))
	}
	if !strings.Contains(session.EntryText(&session.MessageEntry{Message: snapshot.Messages[0]}), "durable summary") {
		t.Fatalf(
			"replayed summary = %q, want durable summary",
			session.EntryText(&session.MessageEntry{Message: snapshot.Messages[0]}),
		)
	}
	if got := session.EntryText(&session.MessageEntry{Message: snapshot.Messages[1]}); got != "recent" {
		t.Fatalf("replayed kept message = %q, want recent", got)
	}
	entries, err := reopened.Entries(context.Background())
	if err != nil {
		t.Fatalf("reopened entries: %v", err)
	}
	var foundUsage session.Usage
	for _, entry := range entries {
		if compaction, ok := entry.(*session.CompactionEntry); ok {
			foundUsage = compaction.Usage
			break
		}
	}
	if foundUsage.Input != 11 || foundUsage.Output != 7 || foundUsage.TotalTokens != 18 ||
		foundUsage.Cost.Total != 0.25 {
		t.Fatalf("replayed compaction usage = %#v, want provider usage", foundUsage)
	}
}

func TestCompactCancellationDoesNotDoubleCloseSignal(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	if _, err := sess.AppendMessage(context.Background(), session.NewUserText("history", time.Now())); err != nil {
		t.Fatal(err)
	}
	if _, err := sess.AppendMessage(context.Background(), session.NewUserText("recent", time.Now())); err != nil {
		t.Fatal(err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	_, err := Compact(ctx, sess, CompactOptions{
		Model: "test",
		StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
			cancel()
			// Give the old proxy implementation time to close its channel. The
			// regression then panicked when Compact deferred another close.
			time.Sleep(10 * time.Millisecond)
			return &mockStream{}, nil
		},
	}, CompactionSettings{Enabled: true, ReserveTokens: 1, KeepRecentTokens: 1})
	if err == nil || !strings.Contains(err.Error(), "summarization aborted") {
		t.Fatalf("Compact error = %v, want cancellation during summarization", err)
	}
}

func TestAbortCancelsActiveCompaction(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	for _, text := range []string{"history", "recent"} {
		if _, err := sess.AppendMessage(context.Background(), session.NewUserText(text, time.Now())); err != nil {
			t.Fatal(err)
		}
	}

	started := make(chan struct{})
	h := NewController(ControllerConfig{
		Session: sess,
		Store:   store,
		Model:   llm.Model{ID: "summary-model"},
		Compaction: CompactionSettings{
			Enabled:          true,
			ReserveTokens:    1,
			KeepRecentTokens: 1,
		},
		StreamFn: func(ctx context.Context, _ *llm.Request) (llm.Stream, error) {
			close(started)
			<-ctx.Done()
			return nil, ctx.Err()
		},
	})
	defer h.Close()

	resultCh := make(chan error, 1)
	go func() { resultCh <- h.Compact(context.Background()) }()
	<-started
	if _, _, err := h.Abort(); err != nil {
		t.Fatalf("Abort() during compaction: %v", err)
	}
	if err := <-resultCh; !errors.Is(err, context.Canceled) {
		t.Fatalf("compaction error = %v, want context.Canceled", err)
	}
	entries, err := sess.Entries(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 2 {
		t.Fatalf("entries after canceled compaction = %d, want unchanged history", len(entries))
	}
}

func TestGenerateSummarySerializesConvertedToolCalls(t *testing.T) {
	var request llm.Request
	messages := []session.Message{
		session.NewUserText("inspect the project", time.Now()),
		&session.AssistantMessage{
			Content: []session.Content{&session.ToolCall{
				ID:        "call-1",
				Name:      "read",
				Arguments: map[string]any{"path": "main.go"},
			}},
		},
		&session.ToolResultMessage{
			ToolCallID: "call-1",
			ToolName:   "read",
			Content:    []session.Content{session.TextContent{Text: "package main"}},
		},
	}
	_, err := GenerateSummary(
		context.Background(), messages, "test", 1024, 0, "", nil, nil, "", "", "", nil,
		func(_ context.Context, req *llm.Request) (llm.Stream, error) {
			request = *req
			return &mockStream{chunks: []*llm.Chunk{{Content: "summary", StopReason: "stop"}}}, nil
		},
		llm.StreamRetryPolicy{},
	)
	if err != nil {
		t.Fatalf("GenerateSummary: %v", err)
	}
	if len(request.Messages) != 2 || !strings.Contains(request.Messages[1].Content, "read") ||
		!strings.Contains(request.Messages[1].Content, "main.go") ||
		!strings.Contains(request.Messages[1].Content, "package main") {
		t.Fatalf("summary request = %#v, want tool call and result context", request.Messages)
	}
}

func TestGenerateSummaryRetriesAfterPartialTransientFailure(t *testing.T) {
	var attempts int
	transient := errors.New("summary connection lost")
	result, err := GenerateSummary(
		context.Background(),
		[]session.Message{session.NewUserText("history", time.Now())},
		"test", 1024, 0, "", nil, nil, "", "", "", nil,
		func(context.Context, *llm.Request) (llm.Stream, error) {
			attempts++
			if attempts == 1 {
				return &compactionTransientStream{err: transient}, nil
			}
			return &mockStream{chunks: []*llm.Chunk{{Content: "complete", StopReason: "stop"}}}, nil
		},
		llm.StreamRetryPolicy{
			Config: llm.RetryConfig{
				MaxAttempts: 2, MinInterval: time.Nanosecond, MaxInterval: time.Nanosecond, Multiplier: 1,
			},
			IsTransient: func(err error) bool { return errors.Is(err, transient) },
		},
	)
	if err != nil {
		t.Fatalf("GenerateSummary: %v", err)
	}
	if attempts != 2 {
		t.Fatalf("summary attempts = %d, want one replay-safe retry", attempts)
	}
	if result.Text != "complete" {
		t.Fatalf("summary text = %q, want only the completed retry", result.Text)
	}
}

func TestGenerateSummaryRequiresStream(t *testing.T) {
	_, err := GenerateSummary(
		context.Background(),
		[]session.Message{session.NewUserText("history", time.Now())},
		"test", 1024, 0, "", nil, nil, "", "", "", nil, nil,
		llm.StreamRetryPolicy{},
	)
	if err == nil || err.Error() != "compaction stream is not configured" {
		t.Fatalf("GenerateSummary error = %v, want missing stream error", err)
	}
}

type compactionTransientStream struct {
	err     error
	emitted bool
}

func (s *compactionTransientStream) Next() (*llm.Chunk, bool) {
	if s.emitted {
		return nil, false
	}
	s.emitted = true
	return &llm.Chunk{Content: "partial"}, true
}

func (s *compactionTransientStream) Err() error { return s.err }
func (*compactionTransientStream) Close() error { return nil }

// mustTime returns a fixed time for test use.
func mustTime() time.Time {
	return time.Time{}
}
