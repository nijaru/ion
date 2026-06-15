package session

import (
	"encoding/json"
	"fmt"
	"os"
	"sync"
	"time"

	"github.com/nijaru/ion/llm"
)

// EntryType discriminates the kind of session tree entry.
type EntryType string

const (
	EntryMessage             EntryType = "message"
	EntryCompaction          EntryType = "compaction"
	EntryBranchSummary       EntryType = "branch_summary"
	EntryThinkingLevelChange EntryType = "thinking_level_change"
	EntryModelChange         EntryType = "model_change"
	EntryActiveToolsChange   EntryType = "active_tools_change"
	EntryLabel               EntryType = "label"
	EntrySessionInfo         EntryType = "session_info"
	EntryLeaf                EntryType = "leaf"
	EntryCustom              EntryType = "custom"
)

// TreeEntry is a single node in the session tree.
// Each entry has a unique ID, an optional parent, a type, and a timestamp.
// Type-specific data is stored in the union fields.
type TreeEntry struct {
	ID        string    `json:"id"`
	ParentID  *string   `json:"parent_id,omitempty"`
	Type      EntryType `json:"type"`
	Timestamp time.Time `json:"timestamp"`

	// Union: exactly one of these is set based on Type.
	Message       *llm.Message        `json:"message,omitempty"`
	Compaction    *CompactionData       `json:"compaction,omitempty"`
	BranchSummary *TreeBranchSummaryData `json:"branch_summary,omitempty"`
	ThinkingLevel *ThinkingLevelData  `json:"thinking_level,omitempty"`
	ModelChange   *ModelChangeData    `json:"model_change,omitempty"`
	ToolsChange   *ActiveToolsData    `json:"tools_change,omitempty"`
	Label         *LabelData          `json:"label,omitempty"`
	SessionInfo   *SessionInfoData    `json:"session_info,omitempty"`
	Custom        *CustomData         `json:"custom,omitempty"`
}

// CompactionData holds summarized context.
type CompactionData struct {
	Summary          string `json:"summary"`
	FirstKeptEntryID string `json:"first_kept_entry_id"`
	TokensBefore     int    `json:"tokens_before"`
}

// TreeBranchSummaryData holds a summary of a branch in the tree.
type TreeBranchSummaryData struct {
	FromID  string `json:"from_id"`
	Summary string `json:"summary"`
}

// ThinkingLevelData records a thinking level change.
type ThinkingLevelData struct {
	Level string `json:"level"`
}

// ModelChangeData records a model change.
type ModelChangeData struct {
	Provider string `json:"provider"`
	ModelID  string `json:"model_id"`
}

// ActiveToolsData records a change in active tools.
type ActiveToolsData struct {
	ToolNames []string `json:"tool_names"`
}

// LabelData attaches a label to another entry.
type LabelData struct {
	TargetID string `json:"target_id"`
	Label    string `json:"label"`
}

// SessionInfoData holds session metadata.
type SessionInfoData struct {
	Name string `json:"name,omitempty"`
}

// CustomData holds extensible custom entry data.
type CustomData struct {
	CustomType string `json:"custom_type"`
	Data       any    `json:"data,omitempty"`
}

// NewMessageEntry creates a message entry.
func NewMessageEntry(id string, parentID *string, msg llm.Message) *TreeEntry {
	return &TreeEntry{
		ID:        id,
		ParentID:  parentID,
		Type:      EntryMessage,
		Timestamp: time.Now(),
		Message:   &msg,
	}
}

// NewCompactionEntry creates a compaction entry.
func NewCompactionEntry(id string, parentID *string, summary string, firstKeptID string, tokensBefore int) *TreeEntry {
	return &TreeEntry{
		ID:       id,
		ParentID: parentID,
		Type:     EntryCompaction,
		Timestamp: time.Now(),
		Compaction: &CompactionData{
			Summary:          summary,
			FirstKeptEntryID: firstKeptID,
			TokensBefore:     tokensBefore,
		},
	}
}

// NewBranchSummaryEntry creates a branch summary entry.
func NewBranchSummaryEntry(id string, parentID *string, fromID string, summary string) *TreeEntry {
	return &TreeEntry{
		ID:       id,
		ParentID: parentID,
		Type:     EntryBranchSummary,
		Timestamp: time.Now(),
		BranchSummary: &TreeBranchSummaryData{
			FromID:  fromID,
			Summary: summary,
		},
	}
}

// NewModelChangeEntry creates a model change entry.
func NewModelChangeEntry(id string, parentID *string, provider string, modelID string) *TreeEntry {
	return &TreeEntry{
		ID:       id,
		ParentID: parentID,
		Type:     EntryModelChange,
		Timestamp: time.Now(),
		ModelChange: &ModelChangeData{
			Provider: provider,
			ModelID:  modelID,
		},
	}
}

// NewCustomEntry creates a custom entry.
func NewCustomEntry(id string, parentID *string, customType string, data any) *TreeEntry {
	return &TreeEntry{
		ID:       id,
		ParentID: parentID,
		Type:     EntryCustom,
		Timestamp: time.Now(),
		Custom: &CustomData{
			CustomType: customType,
			Data:       data,
		},
	}
}

// Validate checks that the entry has exactly one type-specific field set.
func (e *TreeEntry) Validate() error {
	count := 0
	if e.Message != nil {
		count++
	}
	if e.Compaction != nil {
		count++
	}
	if e.BranchSummary != nil {
		count++
	}
	if e.ThinkingLevel != nil {
		count++
	}
	if e.ModelChange != nil {
		count++
	}
	if e.ToolsChange != nil {
		count++
	}
	if e.Label != nil {
		count++
	}
	if e.SessionInfo != nil {
		count++
	}
	if e.Custom != nil {
		count++
	}
	if count != 1 {
		return fmt.Errorf("entry %s: expected exactly 1 type-specific field, got %d", e.ID, count)
	}
	return nil
}

// TreeStore is an in-memory tree of session entries.
// Thread-safe for concurrent access.
type TreeStore struct {
	entries  map[string]*TreeEntry
	children map[string][]string // parentId -> child ids
	leafID   string              // active leaf
	nextID   int64               // monotonically increasing ID counter
	mu       sync.RWMutex
}

// NewTreeStore creates an empty tree store.
func NewTreeStore() *TreeStore {
	return &TreeStore{
		entries:  make(map[string]*TreeEntry),
		children: make(map[string][]string),
		nextID:   1,
	}
}

// NextID returns the next unique entry ID and increments the counter.
// Must be called with mu held or on a single-goroutine path.
func (t *TreeStore) NextID() string {
	id := t.nextID
	t.nextID++
	return fmt.Sprintf("%d", id)
}

// Add inserts an entry into the tree.
// Returns an error if the entry ID already exists or parent doesn't exist.
func (t *TreeStore) Add(entry *TreeEntry) error {
	if err := entry.Validate(); err != nil {
		return err
	}

	t.mu.Lock()
	defer t.mu.Unlock()

	if _, exists := t.entries[entry.ID]; exists {
		return fmt.Errorf("entry %s already exists", entry.ID)
	}
	if entry.ParentID != nil {
		if _, exists := t.entries[*entry.ParentID]; !exists {
			return fmt.Errorf("parent %s does not exist", *entry.ParentID)
		}
	}

	t.entries[entry.ID] = entry
	if entry.ParentID != nil {
		t.children[*entry.ParentID] = append(t.children[*entry.ParentID], entry.ID)
	}
	return nil
}

// Get returns an entry by ID.
func (t *TreeStore) Get(id string) (*TreeEntry, bool) {
	t.mu.RLock()
	defer t.mu.RUnlock()
	entry, ok := t.entries[id]
	return entry, ok
}

// Children returns the child entry IDs of the given parent.
func (t *TreeStore) Children(id string) []string {
	t.mu.RLock()
	defer t.mu.RUnlock()
	return append([]string{}, t.children[id]...)
}

// Parent returns the parent entry, or nil if the entry is root.
func (t *TreeStore) Parent(id string) *TreeEntry {
	t.mu.RLock()
	defer t.mu.RUnlock()
	entry, ok := t.entries[id]
	if !ok || entry.ParentID == nil {
		return nil
	}
	return t.entries[*entry.ParentID]
}

// Leaf returns the active leaf entry.
func (t *TreeStore) Leaf() *TreeEntry {
	t.mu.RLock()
	defer t.mu.RUnlock()
	if t.leafID == "" {
		return nil
	}
	return t.entries[t.leafID]
}

// SetLeaf sets the active leaf to the given entry ID.
func (t *TreeStore) SetLeaf(id string) error {
	t.mu.Lock()
	defer t.mu.Unlock()
	if _, exists := t.entries[id]; !exists {
		return fmt.Errorf("entry %s does not exist", id)
	}
	t.leafID = id
	return nil
}

// LeafID returns the active leaf entry ID.
func (t *TreeStore) LeafID() string {
	t.mu.RLock()
	defer t.mu.RUnlock()
	return t.leafID
}

// Len returns the number of entries in the tree.
func (t *TreeStore) Len() int {
	t.mu.RLock()
	defer t.mu.RUnlock()
	return len(t.entries)
}

// Update replaces the message content of an existing entry.
// Returns an error if the entry doesn't exist or isn't a message entry.
func (t *TreeStore) Update(id string, msg llm.Message) error {
	t.mu.Lock()
	defer t.mu.Unlock()
	entry, ok := t.entries[id]
	if !ok {
		return fmt.Errorf("entry %s does not exist", id)
	}
	if entry.Type != EntryMessage {
		return fmt.Errorf("entry %s is not a message entry", id)
	}
	entry.Message = &msg
	return nil
}

// Ancestors returns the path from the given entry to the root (inclusive).
func (t *TreeStore) Ancestors(id string) []*TreeEntry {
	t.mu.RLock()
	defer t.mu.RUnlock()

	var result []*TreeEntry
	current := id
	for current != "" {
		entry, ok := t.entries[current]
		if !ok {
			break
		}
		result = append(result, entry)
		if entry.ParentID == nil {
			break
		}
		current = *entry.ParentID
	}
	return result
}

// Path returns the entries on the path from ancestor to descendant.
func (t *TreeStore) Path(fromID, toID string) ([]*TreeEntry, error) {
	t.mu.RLock()
	defer t.mu.RUnlock()

	// Build ancestor set from 'to' to root
	ancestorSet := make(map[string]bool)
	var toPath []*TreeEntry
	current := toID
	for current != "" {
		ancestorSet[current] = true
		entry, ok := t.entries[current]
		if !ok {
			break
		}
		toPath = append(toPath, entry)
		if entry.ParentID == nil {
			break
		}
		current = *entry.ParentID
	}

	if !ancestorSet[fromID] {
		return nil, fmt.Errorf("entry %s is not an ancestor of %s", fromID, toID)
	}

	// Find the sub-path from 'from' to 'to'
	// toPath is [leaf, ..., root], we want [root, ..., leaf]
	var result []*TreeEntry
	found := false
	for i := len(toPath) - 1; i >= 0; i-- {
		if toPath[i].ID == fromID {
			found = true
		}
		if found {
			result = append(result, toPath[i])
		}
	}
	return result, nil
}

// Messages returns all message entries on the path from root to the active leaf.
func (t *TreeStore) Messages() []llm.Message {
	t.mu.RLock()
	defer t.mu.RUnlock()

	if t.leafID == "" {
		return nil
	}

	var messages []llm.Message
	current := t.leafID
	for current != "" {
		entry, ok := t.entries[current]
		if !ok {
			break
		}
		if entry.Type == EntryMessage && entry.Message != nil {
			messages = append(messages, *entry.Message)
		}
		if entry.ParentID == nil {
			break
		}
		current = *entry.ParentID
	}

	// Reverse to get root-first order
	for i, j := 0, len(messages)-1; i < j; i, j = i+1, j-1 {
		messages[i], messages[j] = messages[j], messages[i]
	}
	return messages
}

// CommonAncestor returns the ID of the nearest common ancestor of two entries.
func (t *TreeStore) CommonAncestor(id1, id2 string) string {
	t.mu.RLock()
	defer t.mu.RUnlock()

	// Build ancestor set for id1
	ancestors := make(map[string]bool)
	current := id1
	for current != "" {
		ancestors[current] = true
		entry, ok := t.entries[current]
		if !ok || entry.ParentID == nil {
			break
		}
		current = *entry.ParentID
	}

	// Walk id2's ancestors to find first common
	current = id2
	for current != "" {
		if ancestors[current] {
			return current
		}
		entry, ok := t.entries[current]
		if !ok || entry.ParentID == nil {
			break
		}
		current = *entry.ParentID
	}
	return ""
}

// Entries returns all entries in the tree (for serialization).
func (t *TreeStore) Entries() []*TreeEntry {
	t.mu.RLock()
	defer t.mu.RUnlock()

	result := make([]*TreeEntry, 0, len(t.entries))
	for _, entry := range t.entries {
		result = append(result, entry)
	}
	return result
}

// Save persists the tree store to a JSON file.
func (t *TreeStore) Save(path string) error {
	t.mu.RLock()
	defer t.mu.RUnlock()

	data := treeStoreData{
		Entries:  t.entries,
		Children: t.children,
		LeafID:   t.leafID,
		NextID:   t.nextID,
	}

	bytes, err := json.Marshal(data)
	if err != nil {
		return fmt.Errorf("marshal tree: %w", err)
	}

	return os.WriteFile(path, bytes, 0644)
}

// LoadTreeStore loads a tree store from a JSON file.
func LoadTreeStore(path string) (*TreeStore, error) {
	bytes, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("read tree: %w", err)
	}

	var data treeStoreData
	if err := json.Unmarshal(bytes, &data); err != nil {
		return nil, fmt.Errorf("unmarshal tree: %w", err)
	}

	return &TreeStore{
		entries:  data.Entries,
		children: data.Children,
		leafID:   data.LeafID,
		nextID:   data.NextID,
	}, nil
}

type treeStoreData struct {
	Entries  map[string]*TreeEntry `json:"entries"`
	Children map[string][]string   `json:"children"`
	LeafID   string                `json:"leaf_id"`
	NextID   int64                 `json:"next_id"`
}

// Remove deletes an entry and all its descendants from the tree.
func (t *TreeStore) Remove(id string) error {
	t.mu.Lock()
	defer t.mu.Unlock()

	entry, exists := t.entries[id]
	if !exists {
		return fmt.Errorf("entry %s does not exist", id)
	}

	// Remove from parent's children list
	if entry.ParentID != nil {
		siblings := t.children[*entry.ParentID]
		for i, childID := range siblings {
			if childID == id {
				t.children[*entry.ParentID] = append(siblings[:i], siblings[i+1:]...)
				break
			}
		}
	}

	// Recursively remove descendants
	t.removeRecursive(id)
	return nil
}

func (t *TreeStore) removeRecursive(id string) {
	// Remove children first
	for _, childID := range t.children[id] {
		t.removeRecursive(childID)
	}
	delete(t.children, id)
	delete(t.entries, id)

	// Update leaf if needed
	if t.leafID == id {
		t.leafID = ""
	}
}
