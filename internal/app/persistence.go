package app

import (
	"context"
	"fmt"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/session"
)

func persistErrorCmd(action string, err error) tea.Cmd {
	return func() tea.Msg {
		return localErrorMsg{err: fmt.Errorf("%s: %w", action, err)}
	}
}

func (m Model) persistEntry(action string, entry any) error {
	if m.Model.Storage == nil {
		return nil
	}
	if err := m.Model.Storage.Append(context.Background(), entry); err != nil {
		return fmt.Errorf("%s: %w", action, err)
	}
	return nil
}

func entryUnix(timestamp time.Time) int64 {
	if timestamp.IsZero() {
		return now()
	}
	return timestamp.UTC().Unix()
}

func setEntryTimestamp(entry *session.Entry, timestamp time.Time) {
	if entry != nil && !timestamp.IsZero() {
		entry.Timestamp = timestamp.UTC()
	}
}
