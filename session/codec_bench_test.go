package session

import (
	"bytes"
	"testing"

	"github.com/nijaru/ion/llm"
)

func BenchmarkDecodeEventJSON(b *testing.B) {
	event := NewMessage("bench-session", llm.Message{
		Role:    llm.RoleAssistant,
		Content: "hello world",
	})
	event.Metadata = map[string]any{
		"kind": "message",
		"seq":  float64(1),
	}

	var buf bytes.Buffer
	if err := writeEventJSON(&buf, event); err != nil {
		b.Fatalf("write event: %v", err)
	}
	data := bytes.Clone(buf.Bytes())

	for b.Loop() {
		if _, err := decodeEventJSON(data); err != nil {
			b.Fatalf("decode event: %v", err)
		}
	}
}

func BenchmarkEffectiveEntriesFromEvents(b *testing.B) {
	sess := New("bench-session")
	for i := range 256 {
		role := llm.RoleUser
		if i%2 == 1 {
			role = llm.RoleAssistant
		}
		_ = sess.Append(b.Context(), NewMessage(sess.ID(), llm.Message{
			Role:    role,
			Content: "message payload",
		}))
	}

	b.ResetTimer()
	for b.Loop() {
		if _, err := sess.EffectiveEntries(); err != nil {
			b.Fatalf("effective entries: %v", err)
		}
	}
}

func BenchmarkExportRun(b *testing.B) {
	sess := New("bench-export")
	for i := range 128 {
		_ = sess.Append(b.Context(), NewMessage(sess.ID(), llm.Message{
			Role:    llm.RoleUser,
			Content: "user message",
		}))
		_ = sess.Append(b.Context(), NewMessage(sess.ID(), llm.Message{
			Role:    llm.RoleAssistant,
			Content: "assistant message",
		}))
		if i%4 == 0 {
			_ = sess.Append(b.Context(), NewMessage(sess.ID(), llm.Message{
				Role:    llm.RoleTool,
				Content: "tool output",
				ToolID:  "tool-call",
			}))
		}
	}

	for b.Loop() {
		if _, err := ExportRun(sess); err != nil {
			b.Fatalf("export run: %v", err)
		}
	}
}
