package session

import (
	"context"
	"fmt"
	"sync"
	"time"

	"github.com/google/uuid"
)

// MemorySessionStorage implements SessionStorage using in-memory storage.
type MemorySessionStorage struct {
	mu       sync.RWMutex
	metadata Metadata
	entries  []TreeEntry
	byID     map[string]*TreeEntry
	leafID   string
}

// NewMemorySessionStorage creates a new in-memory session storage.
func NewMemorySessionStorage(opts *MemoryStorageOpts) *MemorySessionStorage {
	s := &MemorySessionStorage{
		byID: make(map[string]*TreeEntry),
	}

	if opts != nil && opts.Metadata != nil {
		s.metadata = *opts.Metadata
	} else {
		s.metadata = Metadata{
			ID:        uuid.New().String(),
			CreatedAt: time.Now(),
		}
	}

	if opts != nil && opts.Entries != nil {
		for _, entry := range opts.Entries {
			entry := entry
			s.entries = append(s.entries, entry)
			s.byID[entry.ID] = &entry
			s.leafID = leafIDAfterTreeEntry(entry)
		}
	}

	return s
}

// MemoryStorageOpts contains options for creating a MemorySessionStorage.
type MemoryStorageOpts struct {
	Metadata *Metadata
	Entries  []TreeEntry
}

func (s *MemorySessionStorage) GetMetadata(ctx context.Context) (*Metadata, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return &s.metadata, nil
}

func (s *MemorySessionStorage) GetLeafID(ctx context.Context) (string, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.leafID, nil
}

func (s *MemorySessionStorage) SetLeafID(ctx context.Context, leafID string) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	if leafID != "" {
		if _, ok := s.byID[leafID]; !ok {
			return fmt.Errorf("entry not found: %s", leafID)
		}
	}

	// Create a leaf entry
	leafIDStr := leafID
	entry := TreeEntry{
		ID:        uuid.New().String(),
		Type:      "leaf",
		ParentID:  &s.leafID,
		Timestamp: time.Now(),
		BranchSummary: &TreeBranchSummaryData{
			FromID: leafIDStr,
		},
	}
	s.entries = append(s.entries, entry)
	s.byID[entry.ID] = &entry
	s.leafID = leafID

	return nil
}

func (s *MemorySessionStorage) CreateEntryID(ctx context.Context) (string, error) {
	return uuid.New().String(), nil
}

func (s *MemorySessionStorage) AppendEntry(ctx context.Context, entry TreeEntry) error {
	s.mu.Lock()
	defer s.mu.Unlock()

	s.entries = append(s.entries, entry)
	s.byID[entry.ID] = &entry
	s.leafID = leafIDAfterTreeEntry(entry)

	return nil
}

func (s *MemorySessionStorage) GetEntry(ctx context.Context, id string) (*TreeEntry, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()

	entry, ok := s.byID[id]
	if !ok {
		return nil, fmt.Errorf("entry not found: %s", id)
	}
	return entry, nil
}

func (s *MemorySessionStorage) FindEntries(ctx context.Context, entryType EntryType) ([]TreeEntry, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()

	var result []TreeEntry
	for _, entry := range s.entries {
		if entry.Type == entryType {
			result = append(result, entry)
		}
	}
	return result, nil
}

func (s *MemorySessionStorage) GetLabel(ctx context.Context, id string) (string, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()

	entry, ok := s.byID[id]
	if !ok {
		return "", nil
	}
	if entry.Label != nil {
		return entry.Label.Label, nil
	}
	return "", nil
}

func (s *MemorySessionStorage) GetPathToRoot(ctx context.Context, leafID string) ([]TreeEntry, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()

	if leafID == "" {
		return nil, nil
	}

	var path []TreeEntry
	current, ok := s.byID[leafID]
	if !ok {
		return nil, fmt.Errorf("entry not found: %s", leafID)
	}

	for current != nil {
		path = append([]TreeEntry{*current}, path...)
		if current.ParentID == nil || *current.ParentID == "" {
			break
		}
		parent, ok := s.byID[*current.ParentID]
		if !ok {
			break
		}
		current = parent
	}

	return path, nil
}

func (s *MemorySessionStorage) GetEntries(ctx context.Context) ([]TreeEntry, error) {
	s.mu.RLock()
	defer s.mu.RUnlock()

	result := make([]TreeEntry, len(s.entries))
	copy(result, s.entries)
	return result, nil
}

// leafIDAfterTreeEntry returns the leaf ID after an entry is appended.
func leafIDAfterTreeEntry(entry TreeEntry) string {
	if entry.Type == "leaf" && entry.BranchSummary != nil {
		return entry.BranchSummary.FromID
	}
	return entry.ID
}
