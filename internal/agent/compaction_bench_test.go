package agent

import (
	"context"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func BenchmarkCompact256(b *testing.B) {
	ctx := context.Background()
	streamFn := func(context.Context, *llm.Request) (llm.Stream, error) {
		return &mockStream{chunks: []*llm.Chunk{{Content: "summary", StopReason: "stop"}}}, nil
	}
	settings := CompactionSettings{Enabled: true, ReserveTokens: 128, KeepRecentTokens: 512}
	history := strings.Repeat("history content ", 24)

	b.ReportAllocs()
	for b.Loop() {
		store, err := session.NewSQLiteStore(":memory:", "compaction-benchmark")
		if err != nil {
			b.Fatal(err)
		}
		sess := session.NewSession(store, 512)
		for i := 0; i < 256; i++ {
			if _, err := sess.AppendMessage(ctx, session.NewUserText(history, time.Time{})); err != nil {
				_ = store.Close()
				b.Fatal(err)
			}
			if _, err := sess.AppendMessage(ctx, &session.AssistantMessage{
				Content:    []session.Content{session.TextContent{Text: history}},
				StopReason: session.StopReasonEndTurn,
			}); err != nil {
				_ = store.Close()
				b.Fatal(err)
			}
		}
		if _, err := Compact(ctx, sess, CompactOptions{
			Model:    "benchmark",
			StreamFn: streamFn,
		}, settings); err != nil {
			_ = store.Close()
			b.Fatal(err)
		}
		if err := store.Close(); err != nil {
			b.Fatal(err)
		}
	}
}
