package storage

import (
	"context"
	"testing"
	"time"
)

func TestCantoStoreAppendUpdatesRecentSession(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	cwd := "/tmp/ion-storage-test"

	first, err := store.OpenSession(ctx, cwd, "model-a", "main")
	if err != nil {
		t.Fatalf("open first session: %v", err)
	}

	time.Sleep(1100 * time.Millisecond)

	second, err := store.OpenSession(ctx, cwd, "model-b", "main")
	if err != nil {
		t.Fatalf("open second session: %v", err)
	}

	recent, err := store.GetRecentSession(ctx, cwd)
	if err != nil {
		t.Fatalf("recent session before append: %v", err)
	}
	if recent.ID != second.ID() {
		t.Fatalf("recent session before append = %q, want %q", recent.ID, second.ID())
	}

	time.Sleep(1100 * time.Millisecond)

	if err := first.Append(ctx, Status{Status: "working"}); err != nil {
		t.Fatalf("append status: %v", err)
	}

	recent, err = store.GetRecentSession(ctx, cwd)
	if err != nil {
		t.Fatalf("recent session after append: %v", err)
	}
	if recent.ID != first.ID() {
		t.Fatalf("recent session after append = %q, want %q", recent.ID, first.ID())
	}
}

func TestCantoStoreAppendReturnsPersistenceErrors(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := store.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	if err := store.db.Close(); err != nil {
		t.Fatalf("close db: %v", err)
	}

	if err := sess.Append(ctx, Status{Status: "still working"}); err == nil {
		t.Fatal("expected append to return an error when session metadata update fails")
	}
}

func TestCantoStorePersistsSystemNotes(t *testing.T) {
	root := t.TempDir()
	storeAny, err := NewCantoStore(root)
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}
	store := storeAny.(*cantoStore)

	ctx := context.Background()
	sess, err := store.OpenSession(ctx, "/tmp/ion-storage-test", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	if err := sess.Append(ctx, System{Content: "Switched model to gpt-4.1"}); err != nil {
		t.Fatalf("append system note: %v", err)
	}

	resumed, err := store.ResumeSession(ctx, sess.ID())
	if err != nil {
		t.Fatalf("resume session: %v", err)
	}

	entries, err := resumed.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	found := false
	for _, entry := range entries {
		if entry.Role == "system" {
			if entry.Content != "Switched model to gpt-4.1" {
				t.Fatalf("system entry content = %q, want %q", entry.Content, "Switched model to gpt-4.1")
			}
			found = true
		}
	}
	if !found {
		t.Fatal("expected persisted system note in session entries")
	}
}
