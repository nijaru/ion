package session

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"time"
)

// sessionImpl implements Session over a Store.
type sessionImpl struct {
	store  Store
	events chan Event
}

// NewSession wraps a Store with the Session façade.
func NewSession(store Store, bufferSize int) Session {
	return &sessionImpl{store: store, events: make(chan Event, bufferSize)}
}

func (s *sessionImpl) ID() string {
	if leaf := s.store.GetLeafID(); leaf != "" {
		return leaf
	}
	return s.store.GetMetadata().ID
}
func (s *sessionImpl) Meta() Metadata { return s.store.GetMetadata() }
func (s *sessionImpl) Close() error   { return s.store.Close() }
func (s *sessionImpl) Branch(ctx context.Context) ([]Entry, error) {
	return s.store.Branch(ctx)
}
func (s *sessionImpl) Entries(ctx context.Context) ([]Entry, error) {
	return s.store.Entries(ctx)
}

// BuildContext reconstructs []Message by walking the branch.
//
// Compaction-aware: if the branch contains a CompactionEntry, messages before
// its FirstKeptID are replaced by a single system message containing the
// compaction summary. If the branch contains a BranchSummaryEntry, a system
// message is prepended with that summary.
func (s *sessionImpl) BuildContext(ctx context.Context) (ContextSnapshot, error) {
	entries, err := s.store.Branch(ctx)
	if err != nil {
		return ContextSnapshot{}, err
	}

	// Find the most recent compaction, branch summary, and state changes.
	var lastCompaction *CompactionEntry
	var branchSummary *BranchSummaryEntry
	var activeModel string
	var activeThinking ThinkingLevel
	var activeTools []string
	for i := len(entries) - 1; i >= 0; i-- {
		switch e := entries[i].(type) {
		case *CompactionEntry:
			if lastCompaction == nil {
				lastCompaction = e
			}
		case *BranchSummaryEntry:
			if branchSummary == nil {
				branchSummary = e
			}
		case *ModelChangeEntry:
			if activeModel == "" {
				activeModel = e.ModelID
			}
		case *ThinkingChangeEntry:
			if activeThinking == "" {
				activeThinking = e.Level
			}
		case *ToolsChangeEntry:
			if activeTools == nil {
				activeTools = e.ActiveTools
			}
		}
		if lastCompaction != nil && branchSummary != nil && activeModel != "" && activeThinking != "" && activeTools != nil {
			break
		}
	}

	// Determine the first index to include after the compaction cut.
	startIdx := 0
	if lastCompaction != nil {
		for i, e := range entries {
			if e.ID() == lastCompaction.FirstKeptID {
				startIdx = i
				break
			}
		}
	}

	var msgs []Message

	// Prepend branch summary if present.
	if branchSummary != nil && branchSummary.Summary != "" {
		msgs = append(msgs, NewUserText(branchSummary.Summary, time.Now()))
	}

	// Prepend compaction summary if present.
	if lastCompaction != nil && lastCompaction.Summary != "" {
		msgs = append(msgs, NewUserText(lastCompaction.Summary, time.Now()))
	}

	// Extract messages from the kept portion of the branch.
	for _, e := range entries[startIdx:] {
		if me, ok := e.(*MessageEntry); ok {
			msgs = append(msgs, me.Message)
		}
	}

	return ContextSnapshot{
		Messages:    msgs,
		ActiveModel: activeModel,
		Thinking:    activeThinking,
		ActiveTools: activeTools,
	}, nil
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

// --- typed append helpers ---

func (s *sessionImpl) AppendMessage(ctx context.Context, msg Message) (string, error) {
	id := newID()
	entry := &MessageEntry{
		EntryBase: EntryBase{ID: id, ParentID: s.store.GetLeafID(), Timestamp: time.Now()},
		Message:   msg,
	}
	_, err := s.store.Append(ctx, entry)
	if err != nil {
		return "", err
	}
	if err := s.store.SetLeafID(id); err != nil {
		return "", err
	}
	return id, nil
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

func (s *sessionImpl) AppendSessionInfo(ctx context.Context, name string) (string, error) {
	return s.appendLeaf(ctx, &SessionInfoEntry{
		EntryBase: s.newBase(ctx),
		Name:      name,
	})
}

func (s *sessionImpl) AppendModelChange(ctx context.Context, provider string, modelID string) (string, error) {
	return s.appendLeaf(ctx, &ModelChangeEntry{
		EntryBase: s.newBase(ctx),
		Provider:  provider,
		ModelID:   modelID,
	})
}

func (s *sessionImpl) AppendThinkingLevelChange(ctx context.Context, level ThinkingLevel) (string, error) {
	return s.appendLeaf(ctx, &ThinkingChangeEntry{
		EntryBase: s.newBase(ctx),
		Level:     level,
	})
}

func (s *sessionImpl) AppendActiveToolsChange(ctx context.Context, tools []string) (string, error) {
	return s.appendLeaf(ctx, &ToolsChangeEntry{
		EntryBase:   s.newBase(ctx),
		ActiveTools: tools,
	})
}

func (s *sessionImpl) AppendCustom(ctx context.Context, custom *CustomEntry) (string, error) {
	custom.EntryBase = s.newBase(ctx)
	return s.appendLeaf(ctx, custom)
}

// appendLeaf persists an entry and advances the leaf pointer.
func (s *sessionImpl) appendLeaf(ctx context.Context, entry Entry) (string, error) {
	id := entry.ID()
	_, err := s.store.Append(ctx, entry)
	if err != nil {
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

func (s *sessionImpl) Append(ctx context.Context, entry Entry) (string, error) {
	_, err := s.store.Append(ctx, entry)
	if err != nil {
		return "", err
	}
	return entry.(interface{ ID() string }).ID(), nil
}

func (s *sessionImpl) SubmitTurn(ctx context.Context, text string) error {
	_, err := s.AppendMessage(ctx, NewUserText(text, time.Now()))
	return err
}

func (s *sessionImpl) EventSender() chan Event { return s.events }
