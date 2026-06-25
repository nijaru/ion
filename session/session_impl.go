package session

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"time"
)

// sessionImpl implements Session over a Store.
type sessionImpl struct {
	store Store
}

// NewSession wraps a Store with the Session façade.
func NewSession(store Store) Session {
	return &sessionImpl{store: store}
}

func (s *sessionImpl) ID() string   { return s.store.GetMetadata().ID }
func (s *sessionImpl) Meta() Metadata { return s.store.GetMetadata() }
func (s *sessionImpl) Close() error { return s.store.Close() }
func (s *sessionImpl) GetLeafID() string { return s.store.GetLeafID() }
func (s *sessionImpl) SetLeafID(id string) error { return s.store.SetLeafID(id) }
func (s *sessionImpl) GetEntry(ctx context.Context, id string) (Entry, error) {
	return s.store.GetEntry(ctx, id)
}
func (s *sessionImpl) Branch(ctx context.Context) ([]Entry, error) {
	return s.store.Branch(ctx)
}
func (s *sessionImpl) Entries(ctx context.Context) ([]Entry, error) {
	return s.store.Entries(ctx)
}

// BuildContext reconstructs []Message by walking the branch and extracting MessageEntries.
func (s *sessionImpl) BuildContext(ctx context.Context) (ContextSnapshot, error) {
	entries, err := s.store.Branch(ctx)
	if err != nil {
		return ContextSnapshot{}, err
	}
	var msgs []Message
	for _, e := range entries {
		if me, ok := e.(*MessageEntry); ok {
			msgs = append(msgs, me.Message)
		}
	}
	return ContextSnapshot{Messages: msgs}, nil
}

// Usage accumulates token counts from all AssistantMessages in the branch.
func (s *sessionImpl) Usage(ctx context.Context) (Usage, error) {
	entries, err := s.store.Branch(ctx)
	if err != nil {
		return Usage{}, err
	}
	var total Usage
	for _, e := range entries {
		if me, ok := e.(*MessageEntry); ok {
			if am, ok := me.Message.(*AssistantMessage); ok {
				total.Input += am.Usage.Input
				total.Output += am.Usage.Output
				total.CacheRead += am.Usage.CacheRead
				total.CacheWrite += am.Usage.CacheWrite
				total.TotalTokens += am.Usage.TotalTokens
				total.Cost.Input += am.Usage.Cost.Input
				total.Cost.Output += am.Usage.Cost.Output
				total.Cost.CacheRead += am.Usage.Cost.CacheRead
				total.Cost.CacheWrite += am.Usage.Cost.CacheWrite
				total.Cost.Total += am.Usage.Cost.Total
			}
		}
	}
	return total, nil
}

// MoveTo navigates to a new leaf, optionally appending a branch summary.
func (s *sessionImpl) MoveTo(ctx context.Context, leafID string, summary *BranchSummaryData) (string, error) {
	if err := s.store.SetLeafID(leafID); err != nil {
		return "", err
	}
	if summary == nil {
		return "", nil
	}
	return s.AppendBranchSummary(ctx, *summary)
}

// --- typed append helpers ---

func (s *sessionImpl) AppendMessage(ctx context.Context, msg Message) (string, error) {
	id := newID()
	entry := &MessageEntry{
		EntryBase: EntryBase{ID: id, ParentID: s.store.GetLeafID(), Timestamp: time.Now()},
		Message:   msg,
	}
	if err := s.store.Append(ctx, entry); err != nil {
		return "", err
	}
	if err := s.store.SetLeafID(id); err != nil {
		return "", err
	}
	return id, nil
}

func (s *sessionImpl) AppendModelChange(ctx context.Context, provider, modelID string) (string, error) {
	return s.appendLeaf(ctx, &ModelChangeEntry{
		EntryBase: s.newBase(ctx),
		Provider:  provider,
		ModelID:   modelID,
	})
}

func (s *sessionImpl) AppendThinkingChange(ctx context.Context, level ThinkingLevel) (string, error) {
	return s.appendLeaf(ctx, &ThinkingChangeEntry{
		EntryBase: s.newBase(ctx),
		Level:     level,
	})
}

func (s *sessionImpl) AppendToolsChange(ctx context.Context, tools []string) (string, error) {
	return s.appendLeaf(ctx, &ToolsChangeEntry{
		EntryBase:   s.newBase(ctx),
		ActiveTools: tools,
	})
}

func (s *sessionImpl) AppendCompaction(ctx context.Context, data CompactionData) (string, error) {
	return s.appendLeaf(ctx, &CompactionEntry{
		EntryBase:    s.newBase(ctx),
		Summary:      data.Summary,
		FirstKeptID:  data.FirstKeptID,
		TokensBefore: data.TokensBefore,
		Details:      data.Details,
	})
}

func (s *sessionImpl) AppendBranchSummary(ctx context.Context, data BranchSummaryData) (string, error) {
	return s.appendLeaf(ctx, &BranchSummaryEntry{
		EntryBase: s.newBase(ctx),
		Summary:   data.Summary,
		Details:   data.Details,
	})
}

func (s *sessionImpl) AppendLabel(ctx context.Context, targetID, label string) (string, error) {
	return s.appendLeaf(ctx, &LabelEntry{
		EntryBase: s.newBase(ctx),
		TargetID:  targetID,
		Label:     label,
	})
}

func (s *sessionImpl) AppendSessionInfo(ctx context.Context, name string) (string, error) {
	return s.appendLeaf(ctx, &SessionInfoEntry{
		EntryBase: s.newBase(ctx),
		Name:      name,
	})
}

func (s *sessionImpl) AppendCustom(ctx context.Context, custom *CustomEntry) (string, error) {
	custom.EntryBase = s.newBase(ctx)
	return s.appendLeaf(ctx, custom)
}

// appendLeaf persists an entry and advances the leaf pointer.
func (s *sessionImpl) appendLeaf(ctx context.Context, entry Entry) (string, error) {
	id := entry.ID()
	if err := s.store.Append(ctx, entry); err != nil {
		return "", err
	}
	if err := s.store.SetLeafID(id); err != nil {
		return "", err
	}
	return id, nil
}

func (s *sessionImpl) newBase(ctx context.Context) EntryBase {
	return EntryBase{ID: newID(), ParentID: s.store.GetLeafID(), Timestamp: time.Now()}
}

func newID() string {
	b := make([]byte, 16)
	rand.Read(b)
	return hex.EncodeToString(b)
}
