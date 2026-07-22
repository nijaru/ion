package session

import (
	"fmt"
	"testing"
	"time"
)

func benchmarkSQLiteBranch256(b *testing.B) (*SQLiteStore, Session) {
	store, err := NewSQLiteStore(":memory:", "branch-benchmark")
	if err != nil {
		b.Fatal(err)
	}

	ctx := b.Context()
	sess := NewSession(store, 64)
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
	return store, sess
}

func BenchmarkSQLiteBranch256(b *testing.B) {
	store, _ := benchmarkSQLiteBranch256(b)
	defer store.Close()

	b.ReportAllocs()
	b.ResetTimer()
	ctx := b.Context()
	for b.Loop() {
		if _, err := store.Branch(ctx); err != nil {
			b.Fatal(err)
		}
	}
}

func BenchmarkSQLiteBuildContext256(b *testing.B) {
	store, sess := benchmarkSQLiteBranch256(b)
	defer store.Close()

	b.ReportAllocs()
	b.ResetTimer()
	ctx := b.Context()
	for b.Loop() {
		if _, err := sess.BuildContext(ctx); err != nil {
			b.Fatal(err)
		}
	}
}
