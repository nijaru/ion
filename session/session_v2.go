package session

import (
	"context"
	"fmt"
	"sync"
	"time"

	"github.com/nijaru/ion/llm"
)

// SessionV2 is the high-level session API that wraps SessionStorage.
// This matches Pi's Session class for parity.
type SessionV2 struct {
	mu      sync.RWMutex
	storage SessionStorage
}

// NewSessionV2 creates a new session with the given storage.
func NewSessionV2(storage SessionStorage) *SessionV2 {
	return &SessionV2{
		storage: storage,
	}
}

// GetMetadata returns the session's metadata.
func (s *SessionV2) GetMetadata(ctx context.Context) (*Metadata, error) {
	return s.storage.GetMetadata(ctx)
}

// GetStorage returns the underlying storage.
func (s *SessionV2) GetStorage() SessionStorage {
	return s.storage
}

// GetLeafID returns the current leaf entry ID.
func (s *SessionV2) GetLeafID(ctx context.Context) (string, error) {
	return s.storage.GetLeafID(ctx)
}

// GetEntry returns an entry by ID.
func (s *SessionV2) GetEntry(ctx context.Context, id string) (*TreeEntry, error) {
	return s.storage.GetEntry(ctx, id)
}

// GetEntries returns all entries in the session.
func (s *SessionV2) GetEntries(ctx context.Context) ([]TreeEntry, error) {
	return s.storage.GetEntries(ctx)
}

// GetBranch returns the branch from the given entry to the root.
func (s *SessionV2) GetBranch(ctx context.Context, fromID string) ([]TreeEntry, error) {
	if fromID == "" {
		leafID, err := s.storage.GetLeafID(ctx)
		if err != nil {
			return nil, err
		}
		fromID = leafID
	}
	return s.storage.GetPathToRoot(ctx, fromID)
}

// GetLabel returns the label for an entry.
func (s *SessionV2) GetLabel(ctx context.Context, id string) (string, error) {
	return s.storage.GetLabel(ctx, id)
}

// GetSessionName returns the session's name (from session_info entries).
func (s *SessionV2) GetSessionName(ctx context.Context) (string, error) {
	entries, err := s.storage.FindEntries(ctx, "session_info")
	if err != nil {
		return "", err
	}
	if len(entries) == 0 {
		return "", nil
	}
	// Return the last session_info entry's label
	if entries[len(entries)-1].Label != nil {
		return entries[len(entries)-1].Label.Label, nil
	}
	return "", nil
}

// AppendTypedEntry appends a typed entry to the session.
func (s *SessionV2) AppendTypedEntry(ctx context.Context, entry TreeEntry) (string, error) {
	if err := s.storage.AppendEntry(ctx, entry); err != nil {
		return "", err
	}
	return entry.ID, nil
}

// AppendMessage appends a message entry to the session.
func (s *SessionV2) AppendMessage(ctx context.Context, message llm.Message) (string, error) {
	entryID, err := s.storage.CreateEntryID(ctx)
	if err != nil {
		return "", err
	}

	leafID, err := s.storage.GetLeafID(ctx)
	if err != nil {
		return "", err
	}

	entry := TreeEntry{
		ID:        entryID,
		Type:      "message",
		ParentID:  &leafID,
		Timestamp: time.Now(),
		Message:   &message,
	}

	return s.AppendTypedEntry(ctx, entry)
}

// AppendModelChange appends a model change entry to the session.
func (s *SessionV2) AppendModelChange(ctx context.Context, provider, model string) (string, error) {
	entryID, err := s.storage.CreateEntryID(ctx)
	if err != nil {
		return "", err
	}

	leafID, err := s.storage.GetLeafID(ctx)
	if err != nil {
		return "", err
	}

	entry := TreeEntry{
		ID:        entryID,
		Type:      "model_change",
		ParentID:  &leafID,
		Timestamp: time.Now(),
		ModelChange: &ModelChangeData{
			Provider: provider,
			ModelID:  model,
		},
	}

	return s.AppendTypedEntry(ctx, entry)
}

// AppendThinkingLevelChange appends a thinking level change entry to the session.
func (s *SessionV2) AppendThinkingLevelChange(ctx context.Context, level string) (string, error) {
	entryID, err := s.storage.CreateEntryID(ctx)
	if err != nil {
		return "", err
	}

	leafID, err := s.storage.GetLeafID(ctx)
	if err != nil {
		return "", err
	}

	entry := TreeEntry{
		ID:        entryID,
		Type:      "thinking_level_change",
		ParentID:  &leafID,
		Timestamp: time.Now(),
		ThinkingLevel: &ThinkingLevelData{
			Level: level,
		},
	}

	return s.AppendTypedEntry(ctx, entry)
}

// AppendActiveToolsChange appends an active tools change entry to the session.
func (s *SessionV2) AppendActiveToolsChange(ctx context.Context, toolNames []string) (string, error) {
	entryID, err := s.storage.CreateEntryID(ctx)
	if err != nil {
		return "", err
	}

	leafID, err := s.storage.GetLeafID(ctx)
	if err != nil {
		return "", err
	}

	entry := TreeEntry{
		ID:        entryID,
		Type:      "active_tools_change",
		ParentID:  &leafID,
		Timestamp: time.Now(),
		ToolsChange: &ActiveToolsData{
			ToolNames: toolNames,
		},
	}

	return s.AppendTypedEntry(ctx, entry)
}

// AppendSessionInfo appends a session info entry to the session.
func (s *SessionV2) AppendSessionInfo(ctx context.Context, name string) (string, error) {
	entryID, err := s.storage.CreateEntryID(ctx)
	if err != nil {
		return "", err
	}

	leafID, err := s.storage.GetLeafID(ctx)
	if err != nil {
		return "", err
	}

	entry := TreeEntry{
		ID:        entryID,
		Type:      "session_info",
		ParentID:  &leafID,
		Timestamp: time.Now(),
		SessionInfo: &SessionInfoData{
			Name: name,
		},
	}

	return s.AppendTypedEntry(ctx, entry)
}

// AppendCompaction appends a compaction entry to the session.
func (s *SessionV2) AppendCompaction(ctx context.Context, summary string) (string, error) {
	entryID, err := s.storage.CreateEntryID(ctx)
	if err != nil {
		return "", err
	}

	leafID, err := s.storage.GetLeafID(ctx)
	if err != nil {
		return "", err
	}

	entry := TreeEntry{
		ID:        entryID,
		Type:      "compaction",
		ParentID:  &leafID,
		Timestamp: time.Now(),
		Compaction: &CompactionData{
			Summary: summary,
		},
	}

	return s.AppendTypedEntry(ctx, entry)
}

// SetLeaf sets the leaf entry ID (for branching).
func (s *SessionV2) SetLeaf(ctx context.Context, leafID string) error {
	return s.storage.SetLeafID(ctx, leafID)
}

// BuildContext builds the session context for the agent.
func (s *SessionV2) BuildContext(ctx context.Context) (SessionContext, error) {
	branch, err := s.GetBranch(ctx, "")
	if err != nil {
		return SessionContext{}, fmt.Errorf("get branch: %w", err)
	}
	return buildSessionContextFromEntries(branch), nil
}
