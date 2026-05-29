package session

import (
	"errors"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
)

func TestJSONLStoreForkPersistsTreeQueries(t *testing.T) {
	store, err := NewJSONLStore(t.TempDir())
	if err != nil {
		t.Fatalf("new jsonl store: %v", err)
	}

	parentID := "parent-jsonl"
	childID := "child-jsonl"

	for _, msg := range []llm.Message{
		{Role: llm.RoleUser, Content: "hello"},
		{Role: llm.RoleAssistant, Content: "hi"},
	} {
		if err := store.Save(t.Context(), NewEvent(parentID, MessageAdded, msg)); err != nil {
			t.Fatalf("save parent event: %v", err)
		}
	}

	if _, err := store.ForkWithOptions(t.Context(), parentID, childID, ForkOptions{
		BranchLabel: "review",
		ForkReason:  "compare approaches",
	}); err != nil {
		t.Fatalf("fork with options: %v", err)
	}

	parent, err := store.Parent(t.Context(), childID)
	if err != nil {
		t.Fatalf("parent query failed: %v", err)
	}
	if parent == nil || parent.SessionID != parentID {
		t.Fatalf("parent ancestry = %#v, want session %q", parent, parentID)
	}

	children, err := store.Children(t.Context(), parentID)
	if err != nil {
		t.Fatalf("children query failed: %v", err)
	}
	if len(children) != 1 {
		t.Fatalf("expected 1 child, got %d", len(children))
	}
	if children[0].SessionID != childID {
		t.Fatalf("child session_id = %q, want %q", children[0].SessionID, childID)
	}
	if children[0].BranchLabel != "review" || children[0].ForkReason != "compare approaches" {
		t.Fatalf("child ancestry metadata = %#v", children[0])
	}
	if children[0].Depth != 1 {
		t.Fatalf("child depth = %d, want 1", children[0].Depth)
	}
	loaded, err := store.Load(t.Context(), childID)
	if err != nil {
		t.Fatalf("load child: %v", err)
	}
	assertChildForkPointMatchesLastOrigin(t, loaded, children[0])

	lineage, err := store.Lineage(t.Context(), childID)
	if err != nil {
		t.Fatalf("lineage query failed: %v", err)
	}
	if len(lineage) != 2 || lineage[0].SessionID != parentID || lineage[1].SessionID != childID {
		t.Fatalf("lineage = %#v, want [%q, %q]", lineage, parentID, childID)
	}
}

func TestJSONLStoreRejectsEmptyAssistantMessage(t *testing.T) {
	store, err := NewJSONLStore(t.TempDir())
	if err != nil {
		t.Fatalf("new jsonl store: %v", err)
	}

	err = store.Save(t.Context(), NewMessage("jsonl-empty-assistant", llm.Message{
		Role: llm.RoleAssistant,
	}))
	if !errors.Is(err, errEmptyAssistantMessage) {
		t.Fatalf("Save error = %v, want %v", err, errEmptyAssistantMessage)
	}
}

func TestJSONLStoreRootSessionHasNilParent(t *testing.T) {
	store, err := NewJSONLStore(t.TempDir())
	if err != nil {
		t.Fatalf("new jsonl store: %v", err)
	}

	sessionID := "root-jsonl"
	if err := store.Save(t.Context(), NewEvent(sessionID, MessageAdded, llm.Message{
		Role:    llm.RoleUser,
		Content: "hello",
	})); err != nil {
		t.Fatalf("save root event: %v", err)
	}

	parent, err := store.Parent(t.Context(), sessionID)
	if err != nil {
		t.Fatalf("parent query failed: %v", err)
	}
	if parent != nil {
		t.Fatalf("expected nil root parent, got %#v", parent)
	}

	lineage, err := store.Lineage(t.Context(), sessionID)
	if err != nil {
		t.Fatalf("lineage query failed: %v", err)
	}
	if len(lineage) != 1 || lineage[0].SessionID != sessionID || lineage[0].Depth != 0 {
		t.Fatalf("lineage = %#v, want only root session", lineage)
	}
}

func TestJSONLStoreLoadMissingSessionKeepsWriter(t *testing.T) {
	store, err := NewJSONLStore(t.TempDir())
	if err != nil {
		t.Fatalf("new jsonl store: %v", err)
	}

	sess, err := store.Load(t.Context(), "missing-jsonl")
	if err != nil {
		t.Fatalf("load missing session: %v", err)
	}

	if err := sess.Append(t.Context(), NewEvent("missing-jsonl", MessageAdded, llm.Message{
		Role:    llm.RoleUser,
		Content: "hello",
	})); err != nil {
		t.Fatalf("append missing session: %v", err)
	}

	path := filepath.Join(store.root.Name(), "missing-jsonl.jsonl")
	if _, err := os.Stat(path); err != nil {
		t.Fatalf("stat missing session file: %v", err)
	}

	loaded, err := store.Load(t.Context(), "missing-jsonl")
	if err != nil {
		t.Fatalf("reload missing session: %v", err)
	}
	if got := len(loaded.Messages()); got != 1 {
		t.Fatalf("loaded messages = %d, want 1", got)
	}
}

func TestSessionBranchUsesJSONLLiveParentState(t *testing.T) {
	store, err := NewJSONLStore(t.TempDir())
	if err != nil {
		t.Fatalf("new jsonl store: %v", err)
	}

	parent := New("live-parent").WithWriter(store)
	for _, msg := range []llm.Message{
		{Role: llm.RoleUser, Content: "hello"},
		{Role: llm.RoleAssistant, Content: "hi"},
	} {
		if err := parent.Append(t.Context(), NewMessage(parent.ID(), msg)); err != nil {
			t.Fatalf("append parent message: %v", err)
		}
	}

	child, err := parent.Branch(t.Context(), "live-child", ForkOptions{
		BranchLabel: "fanout",
		ForkReason:  "test",
	})
	if err != nil {
		t.Fatalf("branch session: %v", err)
	}

	reloaded, err := store.Load(t.Context(), child.ID())
	if err != nil {
		t.Fatalf("load child: %v", err)
	}
	if got := len(reloaded.Messages()); got != 2 {
		t.Fatalf("reloaded child messages = %d, want 2", got)
	}

	parentAncestry, err := store.Parent(t.Context(), child.ID())
	if err != nil {
		t.Fatalf("load parent ancestry: %v", err)
	}
	if parentAncestry == nil || parentAncestry.SessionID != parent.ID() {
		t.Fatalf("child parent ancestry = %#v, want %q", parentAncestry, parent.ID())
	}
	lineage, err := store.Lineage(t.Context(), child.ID())
	if err != nil {
		t.Fatalf("lineage query failed: %v", err)
	}
	assertChildForkPointMatchesLastOrigin(t, reloaded, lineage[len(lineage)-1])
}

func TestJSONLStoreSaveAncestryPreservesImportedLineage(t *testing.T) {
	store, err := NewJSONLStore(t.TempDir())
	if err != nil {
		t.Fatalf("new jsonl store: %v", err)
	}

	rootCreatedAt := time.Date(2026, 5, 2, 12, 0, 0, 0, time.UTC)
	childCreatedAt := rootCreatedAt.Add(time.Minute)
	if err := store.SaveAncestry(t.Context(), SessionAncestry{
		SessionID: "import-root",
		Depth:     0,
		CreatedAt: rootCreatedAt,
	}); err != nil {
		t.Fatalf("save root ancestry: %v", err)
	}
	if err := store.SaveAncestry(t.Context(), SessionAncestry{
		SessionID:       "import-child",
		ParentSessionID: "import-root",
		BranchLabel:     "mac branch",
		ForkReason:      "cross-host import",
		Depth:           1,
		CreatedAt:       childCreatedAt,
	}); err != nil {
		t.Fatalf("save child ancestry: %v", err)
	}
	if err := store.Save(t.Context(), NewEvent("import-child", MessageAdded, llm.Message{
		Role:    llm.RoleUser,
		Content: "hello from mac",
	})); err != nil {
		t.Fatalf("save imported event: %v", err)
	}

	parent, err := store.Parent(t.Context(), "import-child")
	if err != nil {
		t.Fatalf("parent query: %v", err)
	}
	if parent == nil || parent.SessionID != "import-root" {
		t.Fatalf("parent = %#v, want import-root", parent)
	}
	lineage, err := store.Lineage(t.Context(), "import-child")
	if err != nil {
		t.Fatalf("lineage query: %v", err)
	}
	if len(lineage) != 2 ||
		lineage[0].SessionID != "import-root" ||
		lineage[1].SessionID != "import-child" ||
		lineage[1].BranchLabel != "mac branch" {
		t.Fatalf("lineage = %#v, want imported parent and child", lineage)
	}
}

func TestJSONLStoreConcurrentSaveAndAncestryImport(t *testing.T) {
	store, err := NewJSONLStore(t.TempDir())
	if err != nil {
		t.Fatalf("new jsonl store: %v", err)
	}

	var wg sync.WaitGroup
	errs := make(chan error, 64)
	for i := range 32 {
		wg.Go(func() {
			sessionID := "concurrent-save"
			errs <- store.Save(t.Context(), NewEvent(sessionID, MessageAdded, llm.Message{
				Role:    llm.RoleUser,
				Content: "hello",
			}))
		})
		wg.Go(func() {
			errs <- store.SaveAncestry(t.Context(), SessionAncestry{
				SessionID: "imported-concurrent-" + string(rune('a'+i)),
				Depth:     0,
				CreatedAt: time.Now().UTC(),
			})
		})
	}
	wg.Wait()
	close(errs)
	for err := range errs {
		if err != nil {
			t.Fatalf("concurrent jsonl store operation: %v", err)
		}
	}
}
