package session

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"fmt"
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
	return s.store.Meta().ID
}
func (s *sessionImpl) Meta() Metadata { return s.store.Meta() }
func (s *sessionImpl) Close() error {
	// Store lifecycle is owned by the caller. Closing here would double-close
	// when closeRuntimeHandles also calls store.Close(), causing "database is
	// closed" errors in --resume --print mode.
	return nil
}
func (s *sessionImpl) Branch(ctx context.Context) ([]Entry, error) {
	return s.store.Branch(ctx)
}
func (s *sessionImpl) GetEntry(ctx context.Context, id string) (Entry, error) {
	return s.store.GetEntry(ctx, id)
}
func (s *sessionImpl) GetLeafID() string { return s.store.GetLeafID() }
func (s *sessionImpl) MoveTo(ctx context.Context, entryID string, summary *BranchSummaryData) (string, error) {
	// Validate the entry exists.
	if _, err := s.store.GetEntry(ctx, entryID); err != nil {
		return "", fmt.Errorf("moveTo: entry %q not found: %w", entryID, err)
	}
	// Record leaf movement as a LeafEntry (Pi: flushPendingSessionWrites records "leaf" type).
	if _, err := s.AppendLeaf(ctx, entryID); err != nil {
		return "", fmt.Errorf("moveTo: append leaf entry: %w", err)
	}
	if err := s.store.SetLeafID(entryID); err != nil {
		return "", fmt.Errorf("moveTo: %w", err)
	}
	if summary != nil {
		return s.AppendBranchSummary(ctx, *summary)
	}
	return "", nil
}
func (s *sessionImpl) Entries(ctx context.Context) ([]Entry, error) {
	return s.store.Entries(ctx)
}

// BuildContext reconstructs []Message by walking the branch.
//
// Compaction-aware: if the branch contains a CompactionEntry, messages before
// its FirstKeptID are replaced by a single system message containing the
// compaction summary. Branch summaries are projected at their tree position.
func (s *sessionImpl) BuildContext(ctx context.Context) (ContextSnapshot, error) {
	entries, err := s.store.Branch(ctx)
	if err != nil {
		return ContextSnapshot{}, err
	}

	// Find the most recent compaction and state changes.
	var lastCompaction *CompactionEntry
	var activeModel string
	var activeThinking ThinkingLevel
	var activeTools []string
	for i := len(entries) - 1; i >= 0; i-- {
		switch e := entries[i].(type) {
		case *CompactionEntry:
			if lastCompaction == nil {
				lastCompaction = e
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
		case *MessageEntry:
			// Pi: assistant messages carry the authoritative provider/model,
			// so the active model is recovered even without an explicit ModelChangeEntry.
			// Reference: Pi session.js buildSessionContext line 15-16.
			if am, ok := e.Message.(*AssistantMessage); ok && activeModel == "" && am.Model != "" {
				activeModel = am.Model
			}
		}
		if lastCompaction != nil && activeModel != "" && activeThinking != "" && activeTools != nil {
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

	// Prepend compaction summary if present (Pi: COMPACTION_SUMMARY_PREFIX/SUFFIX wrapping).
	if lastCompaction != nil && lastCompaction.Summary != "" {
		msgs = append(msgs, NewUserText(
			CompactionSummaryPrefix+lastCompaction.Summary+CompactionSummarySuffix, time.Now()))
	}

	// Extract messages from the kept portion of the branch.
	for _, e := range entries[startIdx:] {
		switch e := e.(type) {
		case *MessageEntry:
			msgs = append(msgs, e.Message)
		case *BranchSummaryEntry:
			if e.Summary != "" {
				msgs = append(msgs, NewUserText(
					BranchSummaryPrefix+e.Summary+BranchSummarySuffix, e.EntryBase.Timestamp))
			}
		case *CustomMessageEntry:
			// Pi: custom_message entries project as CustomMessage in context.
			msgs = append(msgs, &CustomMessage{
				CustomType: e.CustomType,
				Content:    e.Content,
				Display:    e.Display,
				Details:    e.Details,
				Timestamp:  e.EntryBase.Timestamp,
			})
		case *LeafEntry:
			// LeafEntry is metadata; skip in context projection.
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
	return s.store.AppendLeafEntry(ctx, entry)
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
		FromID:    data.FromID,
		Summary:   data.Summary,
		Details:   data.Details,
	})
}

func (s *sessionImpl) SessionName(ctx context.Context) string {
	entries, err := s.store.Branch(ctx)
	if err != nil {
		return ""
	}
	for i := len(entries) - 1; i >= 0; i-- {
		if e, ok := entries[i].(*SessionInfoEntry); ok && e.Name != "" {
			return e.Name
		}
	}
	return ""
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

// AppendLeaf records a leaf position change as a LeafEntry.
func (s *sessionImpl) AppendLeaf(ctx context.Context, targetID string) (string, error) {
	return s.appendLeaf(ctx, &LeafEntry{
		EntryBase: s.newBase(ctx),
		TargetID:  targetID,
	})
}

// AppendCustomMessage persists a CustomMessageEntry in the tree.
func (s *sessionImpl) AppendCustomMessage(ctx context.Context, entry *CustomMessageEntry) (string, error) {
	entry.EntryBase = s.newBase(ctx)
	return s.appendLeaf(ctx, entry)
}

// AppendLabel attaches a label to a target entry (must exist).
// Returns the LabelEntry ID.
func (s *sessionImpl) AppendLabel(ctx context.Context, targetID string, label string) (string, error) {
	if _, err := s.store.GetEntry(ctx, targetID); err != nil {
		return "", fmt.Errorf("appendLabel: target entry %q not found: %w", targetID, err)
	}
	return s.appendLeaf(ctx, &LabelEntry{
		EntryBase: s.newBase(ctx),
		TargetID:  targetID,
		Label:     label,
	})
}

// GetLabel returns the most recent label on the branch for the given target entry.
func (s *sessionImpl) GetLabel(ctx context.Context, targetID string) (string, error) {
	entries, err := s.store.Branch(ctx)
	if err != nil {
		return "", err
	}
	// Walk in reverse; first match is most recent.
	for i := len(entries) - 1; i >= 0; i-- {
		if le, ok := entries[i].(*LabelEntry); ok && le.TargetID == targetID {
			return le.Label, nil
		}
	}
	return "", nil
}

// appendLeaf persists an entry and advances the leaf pointer.
func (s *sessionImpl) appendLeaf(ctx context.Context, entry Entry) (string, error) {
	return s.store.AppendLeafEntry(ctx, entry)
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
