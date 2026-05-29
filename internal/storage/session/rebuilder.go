package session

import (
	"fmt"
	"slices"

	"github.com/nijaru/ion/internal/llm"
)

const defaultRebuilderFilesLimit = 5

// Rebuilder standardizes how model-visible history is reconstructed after a
// durable compaction or projection snapshot. It keeps the append-only event
// log untouched and materializes a canonical prompt view from snapshot state
// plus later events.
type Rebuilder struct {
	FilesLimit int
}

// NewRebuilder creates a Rebuilder with default limits.
func NewRebuilder() *Rebuilder {
	return &Rebuilder{FilesLimit: defaultRebuilderFilesLimit}
}

// RebuildEntries returns the canonical model-visible history after compaction.
func (r *Rebuilder) RebuildEntries(sess *Session) ([]HistoryEntry, error) {
	sess.mu.Lock()
	defer sess.mu.Unlock()
	return r.rebuildEntriesLocked(sess)
}

// RebuildMessages returns the rebuilt model-visible history as plain messages.
func (r *Rebuilder) RebuildMessages(sess *Session) ([]llm.Message, error) {
	entries, err := r.RebuildEntries(sess)
	if err != nil {
		return nil, err
	}
	messages := make([]llm.Message, 0, len(entries))
	for _, entry := range entries {
		messages = append(messages, entry.Message)
	}
	return messages, nil
}

func (r *Rebuilder) rebuildEntriesLocked(sess *Session) ([]HistoryEntry, error) {
	activeEvents, err := sess.activeEventsLocked()
	if err != nil {
		return nil, err
	}
	snapshot, ok, err := latestDurableSnapshotFromEvents(activeEvents)
	if err != nil {
		return nil, err
	}
	if !ok {
		entries, err := rawEntriesFromEvents(activeEvents)
		if err != nil {
			return nil, err
		}
		entries, err = recoverCompletedToolResults(entries, activeEvents)
		if err != nil {
			return nil, err
		}
		return withToolHistory(
			arrangePromptEntries(normalizeEffectiveEntries(entries)),
			activeEvents,
		)
	}

	entries := slices.Clone(snapshot.entries())
	if fileEntry, ok := r.fileContextEntry(snapshot); ok {
		entries = insertAfterDurableContextEntries(entries, fileEntry)
	}

	cutoffSeen := false
	for i := range activeEvents {
		e := &activeEvents[i]
		if !cutoffSeen {
			if e.ID.String() == snapshot.CutoffEventID {
				cutoffSeen = true
			}
			continue
		}
		if e.Type != MessageAdded && e.Type != ContextAdded && e.Type != BranchSummary {
			continue
		}

		entry, err := sess.historyEntryFromEvent(e)
		if err != nil {
			return nil, fmt.Errorf("effective history: decode message %s: %w", e.ID, err)
		}
		entries = append(entries, entry)
	}

	if !cutoffSeen {
		return nil, fmt.Errorf(
			"effective history: compaction cutoff %q not found",
			snapshot.CutoffEventID,
		)
	}
	entries, err = recoverCompletedToolResults(entries, activeEvents)
	if err != nil {
		return nil, err
	}
	return withToolHistory(arrangePromptEntries(normalizeEffectiveEntries(entries)), activeEvents)
}
