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
	m.clearActiveTurnState(true)
	m.InFlight.DrainUntilTurnStarted = true
	m.InFlight.DrainStartedAt = time.Now()
	m.Progress.Compacting = false
	m.Progress.Mode = stateCancelled
	m.Progress.Status = ""
	m.Progress.StatusUpdatedAt = time.Time{}
	entry := session.Entry{Role: session.System, Content: reason}
	if err := m.persistEntry(storage.System{
		Type:    "system",
		Content: entry.Content,
		TS:      now(),
	}); err != nil {
		return m, tea.Sequence(m.printEntries(entry), persistErrorCmd("persist cancellation", err))
	}
	return m, m.printEntries(entry)
}

func (m *Model) clearActiveTurnState(clearQueued bool) {
	m.InFlight.Thinking = false
	m.InFlight.Pending = nil
	m.InFlight.PendingTools = nil
	m.InFlight.Subagents = make(map[string]*SubagentProgress)
	if clearQueued {
		m.InFlight.QueuedTurns = nil
	}
	m.InFlight.StreamBuf = ""
	m.InFlight.ReasonBuf = ""
	m.InFlight.AgentCommitted = false
	m.InFlight.DrainUntilTurnStarted = false
	m.InFlight.DrainStartedAt = time.Time{}
	m.Progress.LastToolUseID = ""
	m.Progress.ContextTokens = 0
}

func (m Model) submitText(text string) (Model, tea.Cmd) {
	// Expand any paste marker placeholders to their original content.
	text = m.expandMarkers(text)
	m.clearPasteMarkers()

	if !strings.HasPrefix(text, "/") {
		if status := m.configurationStatus(); status != "" {
			return m, cmdError(status)
		}
		if reason := m.configuredSessionBudgetStopReason(); reason != "" {
			return m, cmdError(reason)
		}
	}

	historyText, historyChanged := m.appendInputHistory(text)
	var historyCmd tea.Cmd
	if historyChanged {
		historyCmd = m.persistInputHistory(context.Background(), historyText)
	}

	m.resetComposerDraft()

	if strings.HasPrefix(text, "/") {
		m, cmd := m.handleCommand(text)
		return m, sequenceCmds(cmd, historyCmd)
	}

	userEntry := session.Entry{
		Role:      session.User,
		Timestamp: time.Now().UTC(),
		Content:   text,
	}

	m.Progress.Mode = stateIonizing
	m.Progress.Status = ""
	m.Progress.LastError = ""
	m.InFlight.Thinking = true
	if err := m.Model.Session.SubmitTurn(context.Background(), text); err != nil {
		m, errCmd := m.handleSessionError(err, false)
		return m, sequenceCmds(m.printEntries(userEntry), errCmd, historyCmd)
	}
	if err := m.persistEntry(m.routingDecision("use_model", "active_preset", "")); err != nil {
		return m, sequenceCmds(
			m.printEntries(userEntry),
			persistErrorCmd("persist routing decision", err),
			historyCmd,
		)
	}
	return m, sequenceCmds(m.printEntries(userEntry), historyCmd)
}

func (m Model) handleDeferredEnter() (Model, tea.Cmd) {
	if !m.Input.DeferredEnter {
		return m, nil
	}
	if m.printHoldActive() {
		return m, m.scheduleDeferredEnter()
	}
	m.Input.DeferredEnter = false
	m.Input.PrintHoldDelay = 0
	return m.submitComposer()
}

func (m Model) handleQueuedTurn(msg queuedTurnMsg) (Model, tea.Cmd) {
	next, cmd := m.submitText(msg.text)
	if !msg.rearmSessionEvents {
		return next, cmd
	}
	if next.InFlight.Thinking {
		if cmd == nil {
			return next, next.awaitSessionEvent()
		}
		return next, tea.Sequence(cmd, next.awaitSessionEvent())
	}
	return next, sequenceCmds(cmd, next.awaitSessionEvent())
}
