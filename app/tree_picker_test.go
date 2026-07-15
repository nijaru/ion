package app

import (
	"context"
	"testing"
	"time"

	"github.com/nijaru/ion/session"
)

func TestLoadSessionTreeProjectsPersistedLineageAndChildren(t *testing.T) {
	store, err := session.NewSQLiteStore(":memory:", "tree-test")
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close()
	sess := session.NewSession(store, 16)
	ctx := context.Background()

	rootID, err := sess.AppendMessage(ctx, session.NewUserText("root", time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	childID, err := sess.AppendMessage(ctx, session.NewUserText("child", time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	if err := store.SetLeafID(rootID); err != nil {
		t.Fatal(err)
	}

	tree, err := loadSessionTree(ctx, store, rootID)
	if err != nil {
		t.Fatal(err)
	}
	if tree.Current == nil || tree.Current.ID() != rootID {
		t.Fatalf("current = %#v, want root %q", tree.Current, rootID)
	}
	if len(tree.Lineage) != 0 {
		t.Fatalf("lineage = %d entries, want empty root lineage", len(tree.Lineage))
	}
	if len(tree.Children) != 1 || tree.Children[0].ID() != childID {
		t.Fatalf("children = %#v, want child %q", tree.Children, childID)
	}
}
