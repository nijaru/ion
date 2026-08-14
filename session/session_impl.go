package session

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"iter"
	"time"
)

// sessionImpl implements Session over a Store.
type sessionImpl struct {
	store Store
}

// NewSession wraps a Store with the Session façade.
func NewSession(store Store, bufferSize int) Session {
	return &sessionImpl{store: store}
}

func (s *sessionImpl) ID() string {
	return s.store.Meta().ID
}
func (s *sessionImpl) Meta() Metadata { return s.store.Meta() }
func (s *sessionImpl) Branch(ctx context.Context) ([]Entry, error) {
	return s.store.Branch(ctx)
}

func (s *sessionImpl) BranchSeq(ctx context.Context) iter.Seq2[Entry, error] {
	return s.store.BranchSeq(ctx)
}

func (s *sessionImpl) BranchAt(ctx context.Context, leafID string) ([]Entry, error) {
	return s.store.BranchAt(ctx, leafID)
}

func (s *sessionImpl) BranchAtSeq(ctx context.Context, leafID string) iter.Seq2[Entry, error] {
	return s.store.BranchAtSeq(ctx, leafID)
}

func (s *sessionImpl) GetEntry(ctx context.Context, id string) (Entry, error) {
	return s.store.GetEntry(ctx, id)
}
func (s *sessionImpl) GetLeafID() string { return s.store.GetLeafID() }
func (s *sessionImpl) MoveTo(ctx context.Context, entryID string, summary *BranchSummaryData) (string, error) {
	return s.store.MoveTo(ctx, entryID, summary)
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
	return ProjectContext(entries)
}

// ContextMessagesForEntry returns the provider-visible messages represented by
// one durable entry. Metadata entries have no context projection. Session owns
// this mapping so compaction and replay cannot silently diverge.
func ContextMessagesForEntry(entry Entry) []Message {
	switch e := entry.(type) {
	case *MessageEntry:
		if e.Message != nil {
			return []Message{e.Message}
		}
	case *BranchSummaryEntry:
		if e.Summary != "" {
			return []Message{NewUserText(
				BranchSummaryPrefix+e.Summary+BranchSummarySuffix, e.EntryBase.Timestamp,
			)}
		}
	case *CustomMessageEntry:
		return []Message{&CustomMessage{
			CustomType: e.CustomType,
			Content:    e.Content,
			Display:    e.Display,
			Details:    e.Details,
			Timestamp:  e.EntryBase.Timestamp,
		}}
	}
	return nil
}

// ProjectContext projects a selected branch into the immutable context passed
// to the turn engine. The runtime also uses it for a live, uncommitted turn
// branch; keeping the projection pure prevents storage state from leaking into
// the engine contract.
func ProjectContext(entries []Entry) (ContextSnapshot, error) {
	// Find the most recent compaction and state changes.
	var lastCompaction *CompactionEntry
	var activeProvider string
	var activeModel string
	var activeThinking ThinkingLevel
	var activeTools []string
	var activeToolsSet bool
	for i := len(entries) - 1; i >= 0; i-- {
		switch e := entries[i].(type) {
		case *CompactionEntry:
			if lastCompaction == nil {
				lastCompaction = e
			}
		case *ModelChangeEntry:
			if activeModel == "" {
				activeProvider = e.Provider
				activeModel = e.ModelID
			}
		case *ThinkingChangeEntry:
			if activeThinking == "" {
				activeThinking = e.Level
			}
		case *ToolsChangeEntry:
			if !activeToolsSet {
				activeTools = append([]string(nil), e.ActiveTools...)
				activeToolsSet = true
			}
		case *MessageEntry:
			// Assistant messages carry the authoritative provider/model, so the
			// active model is recovered even without an explicit ModelChangeEntry.
			if am, ok := e.Message.(*AssistantMessage); ok && activeModel == "" && am.Model != "" {
				activeProvider = am.Provider
				activeModel = am.Model
			}
		}
		if lastCompaction != nil && activeModel != "" && activeThinking != "" && activeToolsSet {
			break
		}
	}

	// Determine the first index to include after the compaction cut.
	startIdx := 0
	if lastCompaction != nil {
		found := false
		for i, e := range entries {
			if e.ID() == lastCompaction.FirstKeptID {
				startIdx = i
				found = true
				break
			}
		}
		if !found {
			return ContextSnapshot{}, fmt.Errorf(
				"compaction %q references missing first-kept entry %q",
				lastCompaction.ID(),
				lastCompaction.FirstKeptID,
			)
		}
	}

	var msgs []Message

	// Prepend the compaction summary when one is present.
	if lastCompaction != nil && lastCompaction.Summary != "" {
		msgs = append(msgs, NewUserText(
			CompactionSummaryPrefix+lastCompaction.Summary+CompactionSummarySuffix,
			lastCompaction.Timestamp,
		))
	}

	// Extract messages from the kept portion of the branch.
	for _, entry := range entries[startIdx:] {
		msgs = append(msgs, ContextMessagesForEntry(entry)...)
	}

	return ContextSnapshot{
		Messages:       msgs,
		ActiveProvider: activeProvider,
		ActiveModel:    activeModel,
		Thinking:       activeThinking,
		ActiveTools:    activeTools,
		ActiveToolsSet: activeToolsSet,
	}, nil
}

// AddUsage returns the component-wise sum of two usage records.
func AddUsage(left, right Usage) Usage {
	left.Input += right.Input
	left.Output += right.Output
	left.CacheRead += right.CacheRead
	left.CacheWrite += right.CacheWrite
	left.TotalTokens += right.TotalTokens
	left.Cost.Input += right.Cost.Input
	left.Cost.Output += right.Cost.Output
	left.Cost.CacheRead += right.Cost.CacheRead
	left.Cost.CacheWrite += right.Cost.CacheWrite
	left.Cost.Total += right.Cost.Total
	return left
}

// UsageFromEntries accumulates token counts from assistant and summarization entries
// in a selected branch. The runtime uses it for active-turn projections that
// may include entries staged in a durable turn.
func UsageFromEntries(entries []Entry) Usage {
	var total Usage
	for _, e := range entries {
		switch entry := e.(type) {
		case *MessageEntry:
			if am, ok := entry.Message.(*AssistantMessage); ok {
				total = AddUsage(total, am.Usage)
			}
		case *CompactionEntry:
			total = AddUsage(total, entry.Usage)
		case *BranchSummaryEntry:
			total = AddUsage(total, entry.Usage)
		}
	}
	return total
}

// Usage accumulates token counts from all AssistantMessages in the branch.
func (s *sessionImpl) Usage(ctx context.Context) (Usage, error) {
	entries, err := s.store.Branch(ctx)
	if err != nil {
		return Usage{}, err
	}
	return UsageFromEntries(entries), nil
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
		Usage:        data.Usage,
		Details:      data.Details,
	})
}

func (s *sessionImpl) AppendBranchSummary(ctx context.Context, data BranchSummaryData) (string, error) {
	return s.appendLeaf(ctx, &BranchSummaryEntry{
		EntryBase: s.newBase(ctx),
		FromID:    data.FromID,
		Summary:   data.Summary,
		Usage:     data.Usage,
		Details:   data.Details,
	})
}

func (s *sessionImpl) SessionName(ctx context.Context) (string, error) {
	entries, err := s.store.Branch(ctx)
	if err != nil {
		return "", fmt.Errorf("load session name: %w", err)
	}
	for i := len(entries) - 1; i >= 0; i-- {
		if e, ok := entries[i].(*SessionInfoEntry); ok && e.Name != "" {
			return e.Name, nil
		}
	}
	return "", nil
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

// NewEntryID returns an opaque, collision-resistant ID for a durable entry or
// turn. IDs are generated by the session domain so callers never invent an
// alternate persistence identity scheme.
func NewEntryID() string { return newID() }

// NewSessionID returns an opaque identity for a logical conversation catalog
// record. It deliberately uses the same domain-owned generator as entries but
// remains a distinct API so callers do not reuse mutable leaf checkpoints as
// session identities.
func NewSessionID() string { return newID() }

func (s *sessionImpl) Append(ctx context.Context, entry Entry) (string, error) {
	_, err := s.store.Append(ctx, entry)
	if err != nil {
		return "", err
	}
	return entry.(interface{ ID() string }).ID(), nil
}
