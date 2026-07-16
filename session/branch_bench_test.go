package session

import (
	"context"
	"fmt"
	"testing"
	"time"
)

func BenchmarkSQLiteBranch256(b *testing.B) {
	store, err := NewSQLiteStore(":memory:", "branch-benchmark")
	if err != nil {
		b.Fatal(err)
	}
	defer store.Close()

	ctx := context.Background()
	parentID := ""
	for i := 0; i < 256; i++ {
		id := fmt.Sprintf("entry-%03d", i)
		entry := &MessageEntry{
			EntryBase: EntryBase{
				ID:        id,
				ParentID:  parentID,
				Timestamp: time.UnixMilli(int64(i)),
			},
			Message: NewUserText(fmt.Sprintf("message %d", i), time.UnixMilli(int64(i))),
		}
		if _, err := store.AppendLeafEntry(ctx, entry); err != nil {
			b.Fatal(err)
		}
		parentID = id
	}

	b.ReportAllocs()
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		if _, err := store.Branch(ctx); err != nil {
			b.Fatal(err)
		}
	}
}
