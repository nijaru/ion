package app

import (
	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func (m Model) handleSubagentMessage(msg session.AgentMessage) (Model, tea.Cmd) {
	committed, ok := m.turnReducer().commitSubagentMessage(
		msg.AgentID,
		msg.Message,
		msg.Timestamp,
	)
	if !ok {
		return m, m.awaitSessionEvent()
	}
	return m, tea.Sequence(m.terminalCommit().Entries(committed), m.awaitSessionEvent())
}

func (m Model) handleChildRequested(msg session.ChildRequested) (Model, tea.Cmd) {
	p := m.turnReducer().requestChild(msg.AgentName, msg.Query)

	entry, _ := storage.EntrySubagent(p.Name, "Started: "+p.Intent, false, msg.Timestamp)
	return m, sequenceCmds(
		m.terminalCommit().Entries(entry),
		m.persistEntryCmd("persist subagent start", storage.Subagent{
			Type:    "subagent",
			Name:    msg.AgentName,
			Content: entry.Content,
			IsError: false,
			TS:      now(),
		}),
		m.awaitSessionEvent(),
	)
}

func (m Model) handleChildStarted(msg session.ChildStarted) (Model, tea.Cmd) {
	m.turnReducer().startChild(msg.AgentName)
	return m, m.awaitSessionEvent()
}

func (m Model) handleChildDelta(msg session.ChildDelta) (Model, tea.Cmd) {
	m.turnReducer().appendChildDelta(msg.AgentName, msg.Delta)
	return m, m.awaitSessionEvent()
}

func (m Model) handleChildCompleted(msg session.ChildCompleted) (Model, tea.Cmd) {
	committed, ok := m.turnReducer().completeChild(msg.AgentName, msg.Result, msg.Timestamp)
	if !ok {
		return m, m.awaitSessionEvent()
	}

	return m, sequenceCmds(
		m.terminalCommit().Entries(committed),
		m.persistEntryCmd("persist subagent completion", storage.Subagent{
			Type:    "subagent",
			Name:    msg.AgentName,
			Content: committed.Content,
			IsError: false,
			TS:      now(),
		}),
		m.awaitSessionEvent(),
	)
}

func (m Model) handleChildBlocked(msg session.ChildBlocked) (Model, tea.Cmd) {
	m.turnReducer().blockChild(msg.AgentName, msg.Reason)
	return m, m.awaitSessionEvent()
}

func (m Model) handleChildFailed(msg session.ChildFailed) (Model, tea.Cmd) {
	committed, ok := m.turnReducer().failChild(msg.AgentName, msg.Error, msg.Timestamp)
	if !ok {
		return m, m.awaitSessionEvent()
	}

	return m, sequenceCmds(
		m.terminalCommit().Entries(committed),
		m.persistEntryCmd("persist subagent failure", storage.Subagent{
			Type:    "subagent",
			Name:    msg.AgentName,
			Content: committed.Content,
			IsError: true,
			TS:      now(),
		}),
		m.awaitSessionEvent(),
	)
}

func (m Model) handleChildCanceled(msg session.ChildCanceled) (Model, tea.Cmd) {
	committed, ok := m.turnReducer().cancelChild(msg.AgentName, msg.Reason, msg.Timestamp)
	if !ok {
		return m, m.awaitSessionEvent()
	}

	return m, sequenceCmds(
		m.terminalCommit().Entries(committed),
		m.persistEntryCmd("persist subagent cancellation", storage.Subagent{
			Type:    "subagent",
			Name:    msg.AgentName,
			Content: committed.Content,
			IsError: false,
			TS:      now(),
		}),
		m.awaitSessionEvent(),
	)
}

func childCanceledContent(reason string) string {
	if reason == "" {
		return "Canceled"
	}
	return "Canceled: " + reason
}
