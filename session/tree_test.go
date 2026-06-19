package session

import (
	"fmt"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
)

func TestTreeEntry_Validate(t *testing.T) {
	tests := []struct {
		name    string
		entry   *TreeEntry
		wantErr bool
	}{
		{
			name:    "message entry valid",
			entry:   NewMessageEntry("1", nil, llm.Message{Role: llm.RoleUser, Content: "hello"}),
			wantErr: false,
		},
		{
			name:    "compaction entry valid",
			entry:   NewCompactionEntry("1", nil, "summary", "first-kept", 100),
			wantErr: false,
		},
		{
			name: "no type-specific field",
			entry: &TreeEntry{
				ID:        "1",
				Type:      EntryMessage,
				Timestamp: now(),
			},
			wantErr: true,
		},
		{
			name: "multiple type-specific fields",
			entry: &TreeEntry{
				ID:        "1",
				Type:      EntryMessage,
				Timestamp: now(),
				Message:   &llm.Message{Role: llm.RoleUser, Content: "hello"},
				Custom:    &CustomData{CustomType: "test"},
			},
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.entry.Validate()
			if (err != nil) != tt.wantErr {
				t.Errorf("Validate() error = %v, wantErr %v", err, tt.wantErr)
			}
		})
	}
}

func TestTreeStore_Add(t *testing.T) {
	store := NewTreeStore()

	// Add root entry
	root := NewMessageEntry("root", nil, llm.Message{Role: llm.RoleUser, Content: "hello"})
	if err := store.Add(root); err != nil {
		t.Fatalf("Add root: %v", err)
	}

	// Add child entry
	child := NewMessageEntry("child", ptr("root"), llm.Message{Role: llm.RoleAssistant, Content: "hi"})
	if err := store.Add(child); err != nil {
		t.Fatalf("Add child: %v", err)
	}

	// Verify parent-child relationship
	children := store.Children("root")
	if len(children) != 1 || children[0] != "child" {
		t.Fatalf("Children(root) = %v, want [child]", children)
	}

	// Verify duplicate detection
	if err := store.Add(root); err == nil {
		t.Fatal("expected error for duplicate entry")
	}

	// Verify missing parent detection
	bad := NewMessageEntry("bad", ptr("nonexistent"), llm.Message{Role: llm.RoleUser, Content: "x"})
	if err := store.Add(bad); err == nil {
		t.Fatal("expected error for missing parent")
	}
}

func TestTreeStore_Leaf(t *testing.T) {
	store := NewTreeStore()

	// No leaf initially
	if leaf := store.Leaf(); leaf != nil {
		t.Fatalf("expected nil leaf, got %v", leaf)
	}

	// Add entries
	store.Add(NewMessageEntry("1", nil, llm.Message{Role: llm.RoleUser, Content: "a"}))
	store.Add(NewMessageEntry("2", ptr("1"), llm.Message{Role: llm.RoleAssistant, Content: "b"}))

	// Set leaf
	if err := store.SetLeaf("2"); err != nil {
		t.Fatalf("SetLeaf: %v", err)
	}

	leaf := store.Leaf()
	if leaf == nil || leaf.ID != "2" {
		t.Fatalf("Leaf() = %v, want entry 2", leaf)
	}

	// Set leaf to nonexistent entry
	if err := store.SetLeaf("x"); err == nil {
		t.Fatal("expected error for nonexistent leaf")
	}
}

func TestTreeStore_Ancestors(t *testing.T) {
	store := NewTreeStore()

	store.Add(NewMessageEntry("root", nil, llm.Message{Role: llm.RoleUser, Content: "a"}))
	store.Add(NewMessageEntry("mid", ptr("root"), llm.Message{Role: llm.RoleAssistant, Content: "b"}))
	store.Add(NewMessageEntry("leaf", ptr("mid"), llm.Message{Role: llm.RoleUser, Content: "c"}))

	ancestors := store.Ancestors("leaf")
	if len(ancestors) != 3 {
		t.Fatalf("Ancestors(leaf) = %d entries, want 3", len(ancestors))
	}
	if ancestors[0].ID != "leaf" || ancestors[2].ID != "root" {
		t.Fatalf("Ancestors order wrong: %v", ancestors)
	}
}

func TestTreeStore_Path(t *testing.T) {
	store := NewTreeStore()

	store.Add(NewMessageEntry("root", nil, llm.Message{Role: llm.RoleUser, Content: "a"}))
	store.Add(NewMessageEntry("mid", ptr("root"), llm.Message{Role: llm.RoleAssistant, Content: "b"}))
	store.Add(NewMessageEntry("leaf", ptr("mid"), llm.Message{Role: llm.RoleUser, Content: "c"}))

	path, err := store.Path("root", "leaf")
	if err != nil {
		t.Fatalf("Path: %v", err)
	}
	if len(path) != 3 {
		t.Fatalf("Path = %d entries, want 3", len(path))
	}
	if path[0].ID != "root" || path[2].ID != "leaf" {
		t.Fatalf("Path order wrong: %v", path)
	}

	// Test non-ancestor path
	store.Add(NewMessageEntry("other", nil, llm.Message{Role: llm.RoleUser, Content: "x"}))
	_, err = store.Path("other", "leaf")
	if err == nil {
		t.Fatal("expected error for non-ancestor path")
	}
}

func TestTreeStore_Messages(t *testing.T) {
	store := NewTreeStore()

	store.Add(NewMessageEntry("1", nil, llm.Message{Role: llm.RoleUser, Content: "hello"}))
	store.Add(NewMessageEntry("2", ptr("1"), llm.Message{Role: llm.RoleAssistant, Content: "hi"}))
	store.Add(NewMessageEntry("3", ptr("2"), llm.Message{Role: llm.RoleUser, Content: "bye"}))

	store.SetLeaf("3")

	messages, err := store.Messages()
	if err != nil {
		t.Fatalf("Messages() error: %v", err)
	}
	if len(messages) != 3 {
		t.Fatalf("Messages() = %d messages, want 3", len(messages))
	}
	if messages[0].Content != "hello" || messages[2].Content != "bye" {
		t.Fatalf("Messages() order wrong: %v", messages)
	}
}

func TestTreeStore_MessagesWithCompaction(t *testing.T) {
	store := NewTreeStore()

	// Build: msg1 -> msg2 -> compact -> msg3
	store.Add(NewMessageEntry("1", nil, llm.Message{Role: llm.RoleUser, Content: "old"}))
	store.Add(NewMessageEntry("2", ptr("1"), llm.Message{Role: llm.RoleAssistant, Content: "old-reply"}))
	store.Add(NewCompactionEntry("compact", ptr("2"), "summarized old context", "2", 1000))
	store.Add(NewMessageEntry("3", ptr("compact"), llm.Message{Role: llm.RoleUser, Content: "new"}))

	store.SetLeaf("3")

	messages, err := store.Messages()
	if err != nil {
		t.Fatalf("Messages() error: %v", err)
	}
	// Should include compaction summary as a system message + msg3
	// The compaction entry acts as a barrier — Messages() should include it as a synthetic system message
	if len(messages) < 1 {
		t.Fatalf("Messages() too few: %d", len(messages))
	}

	// Verify the path is correct
	path := store.Ancestors("3")
	if len(path) != 4 { // 3 -> compact -> 2 -> 1
		t.Fatalf("Ancestors(3) = %d, want 4", len(path))
	}
}

func TestTreeStore_Remove(t *testing.T) {
	store := NewTreeStore()

	store.Add(NewMessageEntry("root", nil, llm.Message{Role: llm.RoleUser, Content: "a"}))
	store.Add(NewMessageEntry("child1", ptr("root"), llm.Message{Role: llm.RoleAssistant, Content: "b"}))
	store.Add(NewMessageEntry("child2", ptr("root"), llm.Message{Role: llm.RoleAssistant, Content: "c"}))
	store.Add(NewMessageEntry("grandchild", ptr("child1"), llm.Message{Role: llm.RoleUser, Content: "d"}))

	store.SetLeaf("grandchild")

	// Remove child1 and its descendants
	if err := store.Remove("child1"); err != nil {
		t.Fatalf("Remove: %v", err)
	}

	if _, ok := store.Get("child1"); ok {
		t.Fatal("child1 should be removed")
	}
	if _, ok := store.Get("grandchild"); ok {
		t.Fatal("grandchild should be removed")
	}

	// Root and child2 should still exist
	if _, ok := store.Get("root"); !ok {
		t.Fatal("root should still exist")
	}
	if _, ok := store.Get("child2"); !ok {
		t.Fatal("child2 should still exist")
	}

	// Leaf should be cleared
	if store.LeafID() != "" {
		t.Fatalf("leaf should be cleared, got %s", store.LeafID())
	}

	// Remove nonexistent entry
	if err := store.Remove("x"); err == nil {
		t.Fatal("expected error for nonexistent entry")
	}
}

func TestTreeStore_Branching(t *testing.T) {
	store := NewTreeStore()

	// Create a linear conversation
	store.Add(NewMessageEntry("1", nil, llm.Message{Role: llm.RoleUser, Content: "question"}))
	store.Add(NewMessageEntry("2", ptr("1"), llm.Message{Role: llm.RoleAssistant, Content: "answer-A"}))

	// Fork: create an alternative answer
	store.Add(NewMessageEntry("2-alt", ptr("1"), llm.Message{Role: llm.RoleAssistant, Content: "answer-B"}))

	// Add follow-up to alternative
	store.Add(NewMessageEntry("3", ptr("2-alt"), llm.Message{Role: llm.RoleUser, Content: "follow-up"}))

	// Set leaf to the alternative path
	store.SetLeaf("3")

	messages, err := store.Messages()
	if err != nil {
		t.Fatalf("Messages() error: %v", err)
	}
	if len(messages) != 3 {
		t.Fatalf("Messages() = %d, want 3", len(messages))
	}
	if messages[1].Content != "answer-B" {
		t.Fatalf("Messages()[1] = %q, want answer-B", messages[1].Content)
	}

	// Switch back to original path
	store.SetLeaf("2")
	messages, err = store.Messages()
	if err != nil {
		t.Fatalf("Messages() error: %v", err)
	}
	if len(messages) != 2 {
		t.Fatalf("Messages() = %d, want 2", len(messages))
	}
	if messages[1].Content != "answer-A" {
		t.Fatalf("Messages()[1] = %q, want answer-A", messages[1].Content)
	}
}

func TestTreeStore_Concurrent(t *testing.T) {
	store := NewTreeStore()
	store.Add(NewMessageEntry("root", nil, llm.Message{Role: llm.RoleUser, Content: "root"}))

	var wg sync.WaitGroup
	for i := 0; i < 100; i++ {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			id := fmt.Sprintf("entry-%d", i)
			store.Add(NewMessageEntry(id, ptr("root"), llm.Message{Role: llm.RoleUser, Content: id}))
			store.Get(id)
			store.Children("root")
			store.Len()
		}(i)
	}
	wg.Wait()

	if store.Len() != 101 { // root + 100
		t.Fatalf("Len() = %d, want 101", store.Len())
	}
}

func ptr(s string) *string {
	return &s
}

func now() time.Time {
	return time.Now()
}

func TestTreeStorePathIntegrity_MissingParent(t *testing.T) {
	store := NewTreeStore()

	// Add entries
	store.Add(NewMessageEntry("1", nil, llm.Message{Role: llm.RoleUser, Content: "hello"}))
	store.Add(NewMessageEntry("2", ptr("1"), llm.Message{Role: llm.RoleAssistant, Content: "hi"}))
	store.SetLeaf("2")

	// Verify path works
	path, err := store.PathToRoot()
	if err != nil {
		t.Fatalf("PathToRoot() error: %v", err)
	}
	if len(path) != 2 {
		t.Fatalf("PathToRoot() = %d entries, want 2", len(path))
	}

	// Corrupt the store by removing entry "1"
	store.mu.Lock()
	delete(store.entries, "1")
	store.mu.Unlock()

	// PathToRoot should return an error, not truncate
	_, err = store.PathToRoot()
	if err == nil {
		t.Fatal("Expected error for missing parent, got nil")
	}
	if !strings.Contains(err.Error(), "path integrity") {
		t.Fatalf("Expected path integrity error, got: %v", err)
	}
}
