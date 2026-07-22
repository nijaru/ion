package agent

import (
	"fmt"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func BenchmarkControllerSubscribeSnapshot256(b *testing.B) {
	store, err := session.NewSQLiteStore(":memory:", "subscription-benchmark")
	if err != nil {
		b.Fatal(err)
	}
	defer store.Close()

	ctx := b.Context()
	parentID := ""
	for i := 0; i < 256; i++ {
		id := fmt.Sprintf("entry-%03d", i)
		entry := &session.MessageEntry{
			EntryBase: session.EntryBase{
				ID:        id,
				ParentID:  parentID,
				Timestamp: time.UnixMilli(int64(i)),
			},
			Message: session.NewUserText(fmt.Sprintf("message %d", i), time.UnixMilli(int64(i))),
		}
		if _, err := store.AppendLeafEntry(ctx, entry); err != nil {
			b.Fatal(err)
		}
		parentID = id
	}

	controller := NewController(ControllerConfig{
		Session: session.NewSession(store, 64),
		Store:   store,
		Model:   llm.Model{ID: "benchmark-model"},
	})
	defer controller.Close()

	b.ReportAllocs()
	b.ResetTimer()
	for b.Loop() {
		subscription, err := controller.Subscribe(ctx, EventCursor{})
		if err != nil {
			b.Fatal(err)
		}
		subscription.Close()
	}
}
