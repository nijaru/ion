package app

import (
	"context"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func (m Model) cancelRunningTurn(reason string) (Model, tea.Cmd) {
	if err := m.Model.Session.CancelTurn(context.Background()); err != nil {
		return m, persistErrorCmd("cancel turn", err)
	}
	m.InFlight.Thinking = false
	m.Progress.Mode = stateCancelled
	m.InFlight.Pending = nil
	m.InFlight.PendingTools = nil
	m.InFlight.QueuedTurns = nil
	m.InFlight.StreamBuf = ""
	m.InFlight.ReasonBuf = ""
	m.InFlight.AgentCommitted = false
	entry := session.Entry{Role: session.System, Content: reason}
	if err := m.persistEntry("persist cancellation", storage.System{
		Type:    "system",
		Content: entry.Content,
		TS:      now(),
	}); err != nil {
		return m, persistErrorCmd("persist cancellation", err)
	}
	return m, m.printEntries(entry)
}

func (m Model) submitText(text string) (Model, tea.Cmd) {
	// Expand any paste marker placeholders to their original content.
	text = m.expandMarkers(text)
	m.PasteMarkers = make(map[string]pasteMarker)

	if !strings.HasPrefix(text, "/") {
		if reason := m.configuredSessionBudgetStopReason(); reason != "" {
			return m, cmdError(reason)
		}
	}

	m.Input.History = append(m.Input.History, text)
	m.Input.HistoryIdx = -1
	m.Input.HistoryDraft = ""

	userEntry := session.Entry{
		Role:      session.User,
		Timestamp: time.Now().UTC(),
		Content:   text,
	}
	m.Input.Composer.Reset()
	m.relayoutComposer()

	if strings.HasPrefix(text, "/") {
		m, cmd := m.handleCommand(text)
		return m, tea.Sequence(m.printEntries(userEntry), cmd)
	}

	m.Progress.Mode = stateIonizing
	m.Progress.Status = ""
	m.Progress.LastError = ""
	m.InFlight.Thinking = true
	if err := m.Model.Session.SubmitTurn(context.Background(), text); err != nil {
		m, errCmd := m.handleSessionError(err, false)
		return m, tea.Sequence(m.printEntries(userEntry), errCmd)
	}
	if err := m.persistEntry("persist routing decision", m.routingDecision("use_model", "active_preset", "")); err != nil {
		return m, persistErrorCmd("persist routing decision", err)
	}
	return m, m.printEntries(userEntry)
}
