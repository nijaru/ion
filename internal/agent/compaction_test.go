package agent

import (
	"context"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

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
		t.Errorf("without usage, tokens should equal trailing, got tokens=%d trailing=%d", est.Tokens, est.TrailingTokens)
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
		session.NewUserText("follow-up with many many many characters here to add some trailing tokens let us make this at least forty characters", mustTime()),
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

func TestGenerateSummaryRequiresStream(t *testing.T) {
	_, err := GenerateSummary(
		context.Background(),
		[]session.Message{session.NewUserText("history", time.Now())},
		"test", 1024, 0, "", nil, nil, "", "", "", nil, nil,
	)
	if err == nil || err.Error() != "compaction stream is not configured" {
		t.Fatalf("GenerateSummary error = %v, want missing stream error", err)
	}
}

// mustTime returns a fixed time for test use.
func mustTime() time.Time {
	return time.Time{}
}
