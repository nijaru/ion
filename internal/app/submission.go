package app

import (
	"context"
	"errors"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func (m Model) cancelRunningTurn(reason string) (Model, tea.Cmd) {
	m.clearActiveTurnState(true)
	m.InFlight.DrainUntilTurnStarted = true
	m.InFlight.DrainStartedAt = time.Now()
	m.Progress.Compacting = false
	m.Progress.Mode = stateCancelled
	m.Progress.Status = ""
	m.Progress.StatusUpdatedAt = time.Time{}
	entry := session.Entry{Role: session.System, Content: reason}
	return m, sequenceCmds(
		m.printEntries(entry),
		m.persistEntryCmd("persist cancellation", storage.System{
			Type:    "system",
			Content: entry.Content,
			TS:      now(),
		}),
		cancelTurnCmd(m.Model.Session),
	)
}

func cancelTurnCmd(sess session.AgentSession) tea.Cmd {
	return func() tea.Msg {
		if sess == nil {
			return turnCancelResultMsg{err: errors.New("session unavailable")}
		}
		if err := sess.CancelTurn(context.Background()); err != nil {
			return turnCancelResultMsg{err: err}
		}
		return turnCancelResultMsg{}
	}
}

func (m Model) handleTurnCancelResult(msg turnCancelResultMsg) (Model, tea.Cmd) {
	if msg.err != nil {
		return m, persistErrorCmd("cancel turn", msg.err)
	}
	return m, nil
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
	draft := text
	text = m.expandMarkers(text)

	if !strings.HasPrefix(text, "/") {
		if status := m.configurationStatus(); status != "" {
			return m, cmdError(status)
		}
		if reason := m.configuredSessionBudgetStopReason(); reason != "" {
			return m, cmdError(reason)
		}
	}

	if strings.HasPrefix(text, "/") {
		historyText, historyChanged := m.appendInputHistory(text)
		var historyCmd tea.Cmd
		if historyChanged {
			historyCmd = m.persistInputHistory(context.Background(), historyText)
		}
		m.resetComposerDraft()
		m, cmd := m.handleCommand(text)
		return m, sequenceCmds(cmd, historyCmd)
	}

	m.Progress.Mode = stateIonizing
	m.Progress.Status = ""
	m.Progress.LastError = ""
	m.InFlight.Thinking = true
	m.resetComposerDraft()
	return m, submitTurnCmd(m.Model.Session, text, draft)
}

func submitTurnCmd(sess session.AgentSession, text, draft string) tea.Cmd {
	return func() tea.Msg {
		if err := sess.SubmitTurn(context.Background(), text); err != nil {
			return turnSubmitResultMsg{text: text, draft: draft, err: err}
		}
		return turnSubmitResultMsg{text: text, draft: draft}
	}
}

func (m Model) handleTurnSubmitResult(msg turnSubmitResultMsg) (Model, tea.Cmd) {
	if msg.err == nil {
		historyText, historyChanged := m.appendInputHistory(msg.text)
		var historyCmd tea.Cmd
		if historyChanged {
			historyCmd = m.persistInputHistory(context.Background(), historyText)
		}
		if err := m.persistEntry(m.routingDecision("use_model", "active_preset", "")); err != nil {
			return m, sequenceCmds(
				persistErrorCmd("persist routing decision", err),
				historyCmd,
			)
		}
		if msg.rearm {
			return m, sequenceCmds(historyCmd, m.awaitSessionEvent())
		}
		return m, historyCmd
	}
	m.clearActiveTurnState(true)
	m.Progress.Compacting = false
	m.Progress.Mode = stateReady
	m.Progress.Status = ""
	m.Progress.StatusUpdatedAt = time.Time{}
	m.Progress.LastError = ""
	m.Progress.TurnStartedAt = time.Time{}
	if strings.TrimSpace(m.Input.Composer.Value()) == "" {
		m.setComposerDraft(msg.draft)
	}
	return m, cmdError(sessionErrorDisplay(msg.err))
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
		return next, rearmSubmitResultCmd(cmd)
	}
	return next, sequenceCmds(cmd, next.awaitSessionEvent())
}

func rearmSubmitResultCmd(submitCmd tea.Cmd) tea.Cmd {
	return func() tea.Msg {
		msg := submitCmd()
		if result, ok := msg.(turnSubmitResultMsg); ok {
			result.rearm = true
			return result
		}
		return msg
	}
}
