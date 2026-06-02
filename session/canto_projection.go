package session

import (
	"context"
	"time"
)

func (s *cantoSession) Entries(ctx context.Context) ([]Entry, error) {
	sess, err := s.store.canto.Load(ctx, s.id)
	if err != nil {
		return nil, err
	}
	return displayEntriesFromSession(s.meta.CWD, sess)
}

func displayEntriesFromSession(
	workdir string,
	sess *Session,
) ([]Entry, error) {
	history, err := sess.EffectiveEntries()
	if err != nil {
		return nil, err
	}

	projector := NewProjector(workdir)
	entries := make([]Entry, 0, len(history))
	effectiveByEventID := make(map[string]HistoryEntry, len(history))
	for _, entry := range history {
		if entry.EventID == "" {
			if display, ok := projector.HistoryEntry(entry); ok {
				entries = append(entries, display)
			}
			continue
		}
		effectiveByEventID[entry.EventID] = entry
	}

	events := sess.Events()
	cutoffID, hasCutoff := latestDisplayCutoff(events)
	afterCutoff := !hasCutoff
	seenEffective := make(map[string]bool, len(effectiveByEventID))
	for _, ev := range events {
		eventID := ev.ID.String()
		if entry, ok := effectiveByEventID[eventID]; ok {
			if display, ok := projector.HistoryEntry(entry); ok {
				display = WithTimestamp(display, ev.Timestamp)
				entries = append(entries, display)
			}
			seenEffective[entry.EventID] = true
		} else if afterCutoff {
			if display, ok := displayEventEntry(ev); ok {
				display = WithTimestamp(display, ev.Timestamp)
				entries = append(entries, display)
			}
		}
		if hasCutoff && eventID == cutoffID {
			afterCutoff = true
		}
	}
	for _, entry := range history {
		if entry.EventID == "" || seenEffective[entry.EventID] {
			continue
		}
		if display, ok := projector.HistoryEntry(entry); ok {
			entries = append(entries, display)
		}
	}
	return Normalize(entries), nil
}

func latestDisplayCutoff(events []Event) (string, bool) {
	for i := len(events) - 1; i >= 0; i-- {
		if snapshot, ok, err := events[i].ProjectionSnapshot(); err == nil &&
			ok &&
			usableDisplaySnapshot(snapshot) {
			return snapshot.CutoffEventID, true
		}
		if snapshot, ok, err := events[i].CompactionSnapshot(); err == nil &&
			ok &&
			usableDisplaySnapshot(snapshot) {
			return snapshot.CutoffEventID, true
		}
	}
	return "", false
}

func usableDisplaySnapshot(snapshot CompactionSnapshot) bool {
	return snapshot.CutoffEventID != "" &&
		(len(snapshot.Entries) > 0 || len(snapshot.Messages) > 0)
}

func displayEventEntry(ev Event) (Entry, bool) {
	switch ev.Type {
	case ionSystemEvent:
		var data StoreSystem
		if err := ev.UnmarshalData(&data); err != nil {
			return Entry{}, false
		}
		return EntrySystem(data.Content, time.Time{})
	case ionSubagentEvent:
		var data StoreSubagent
		if err := ev.UnmarshalData(&data); err != nil {
			return Entry{}, false
		}
		return EntrySubagent(data.Name, data.Content, data.IsError, time.Time{})
	default:
		return Entry{}, false
	}
}
