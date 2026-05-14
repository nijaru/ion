package app

import (
	"context"
	"fmt"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/session"
)

func persistErrorCmd(action string, err error) tea.Cmd {
	if err == nil {
		return nil
	}
	return func() tea.Msg {
		return localErrorMsg{err: fmt.Errorf("%s: %w", action, err)}
	}
}

func (m Model) persistErrorAndAwait(action string, err error) tea.Cmd {
	return sequenceCmds(persistErrorCmd(action, err), m.awaitSessionEvent())
}

func sequenceCmds(cmds ...tea.Cmd) tea.Cmd {
	filtered := cmds[:0]
	for _, cmd := range cmds {
		if cmd != nil {
			filtered = append(filtered, cmd)
		}
	}
	switch len(filtered) {
	case 0:
		return nil
	case 1:
		return filtered[0]
	default:
		return tea.Sequence(filtered...)
	}
}

func (m Model) persistEntry(entry any) error {
	if m.Model.Storage == nil {
		return nil
	}
	if err := m.Model.Storage.Append(context.Background(), entry); err != nil {
		return err
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
