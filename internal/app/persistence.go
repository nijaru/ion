package app

import (
	"fmt"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/session"
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

func (m Model) persistEntryCmd(action string, entry session.StoreEvent) tea.Cmd {
	return m.persistenceController().appendEntry(action, entry)
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

func batchCmds(cmds ...tea.Cmd) tea.Cmd {
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
		return tea.Batch(filtered...)
	}
}

func entryUnix(timestamp time.Time) int64 {
	if timestamp.IsZero() {
		return now()
	}
	return timestamp.UTC().Unix()
}

func setEntryTimestamp(entry *session.Entry, timestamp time.Time) {
	session.SetTimestamp(entry, timestamp)
}
