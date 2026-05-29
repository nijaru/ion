package prompt

import (
	"testing"

	"github.com/nijaru/ion/internal/llm"
	"github.com/nijaru/ion/internal/storage/session"
)

func BenchmarkBuilderBuildPreview(b *testing.B) {
	sess := benchmarkSession(b)
	builder := NewBuilder(
		Instructions("You are Canto benchmark agent."),
		History(),
	)

	for b.Loop() {
		req := &llm.Request{}
		if err := builder.BuildPreview(b.Context(), nil, "", sess, req); err != nil {
			b.Fatalf("build preview: %v", err)
		}
	}
}

func BenchmarkBuilderBuildCommit(b *testing.B) {
	sess := benchmarkSession(b)
	builder := NewBuilder(
		Instructions("You are Canto benchmark agent."),
		History(),
	)

	for b.Loop() {
		req := &llm.Request{}
		if err := builder.BuildCommit(b.Context(), nil, "", sess, req); err != nil {
			b.Fatalf("build commit: %v", err)
		}
	}
}

func benchmarkSession(b *testing.B) *session.Session {
	b.Helper()

	sess := session.New("context-bench")
	for i := range 48 {
		_ = sess.Append(b.Context(), session.NewMessage(sess.ID(), llm.Message{
			Role:    llm.RoleUser,
			Content: "user message",
		}))
		_ = sess.Append(b.Context(), session.NewMessage(sess.ID(), llm.Message{
			Role:    llm.RoleAssistant,
			Content: "assistant message",
		}))

		if i == 23 {
			entries, err := sess.EffectiveEntries()
			if err != nil {
				b.Fatalf("effective entries: %v", err)
			}
			snapshot := session.CompactionSnapshot{
				Strategy:      "benchmark",
				CutoffEventID: entries[len(entries)-1].EventID,
				Entries:       entries,
			}
			if err := sess.Append(b.Context(), session.NewCompactionEvent(sess.ID(), snapshot)); err != nil {
				b.Fatalf("append compaction event: %v", err)
			}
		}
	}
	return sess
}
