package app

import (
	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/session"
)

func (m Model) handleSubagentMessage(msg session.AgentMessageEvent) (Model, tea.Cmd) {
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

func (m Model) handleChildRequested(msg session.ChildRequestedEvent) (Model, tea.Cmd) {
	p := m.turnReducer().requestChild(msg.AgentName, msg.Query)

	entry, _ := session.EntrySubagent(p.Name, "Started: "+p.Intent, false, msg.Timestamp)
	return m, batchCmds(
		m.terminalCommit().Entries(entry),
		m.persistEntryCmd("persist subagent start", session.StoreSubagent{
			Type:    "subagent",
			Name:    msg.AgentName,
			Content: entry.Content,
			IsError: false,
			TS:      now(),
		}),
		m.awaitSessionEvent(),
	)
}

func (m Model) handleChildStarted(msg session.ChildStartedEvent) (Model, tea.Cmd) {
	m.turnReducer().startChild(msg.AgentName)
	return m, m.awaitSessionEvent()
}

func (m Model) handleChildDelta(msg session.ChildDeltaEvent) (Model, tea.Cmd) {
	m.turnReducer().appendChildDelta(msg.AgentName, msg.Delta)
	return m, m.awaitSessionEvent()
}

func (m Model) handleChildCompleted(msg session.ChildCompletedEvent) (Model, tea.Cmd) {
	committed, ok := m.turnReducer().completeChild(msg.AgentName, msg.Result, msg.Timestamp)
	if !ok {
		return m, m.awaitSessionEvent()
	}

	return m, batchCmds(
		m.terminalCommit().Entries(committed),
		m.persistEntryCmd("persist subagent completion", session.StoreSubagent{
			Type:    "subagent",
			Name:    msg.AgentName,
			Content: committed.Content,
			IsError: false,
			TS:      now(),
		}),
		m.awaitSessionEvent(),
	)
}

func (m Model) handleChildBlocked(msg session.ChildBlockedEvent) (Model, tea.Cmd) {
	m.turnReducer().blockChild(msg.AgentName, msg.Reason)
	return m, m.awaitSessionEvent()
}

func (m Model) handleChildFailed(msg session.ChildFailedEvent) (Model, tea.Cmd) {
	committed, ok := m.turnReducer().failChild(msg.AgentName, msg.Error, msg.Timestamp)
	if !ok {
		return m, m.awaitSessionEvent()
	}

	return m, batchCmds(
		m.terminalCommit().Entries(committed),
		m.persistEntryCmd("persist subagent failure", session.StoreSubagent{
			Type:    "subagent",
			Name:    msg.AgentName,
			Content: committed.Content,
			IsError: true,
			TS:      now(),
		}),
		m.awaitSessionEvent(),
	)
}

func (m Model) handleChildCanceled(msg session.ChildCanceledEvent) (Model, tea.Cmd) {
	committed, ok := m.turnReducer().cancelChild(msg.AgentName, msg.Reason, msg.Timestamp)
	if !ok {
		return m, m.awaitSessionEvent()
	}

	return m, batchCmds(
		m.terminalCommit().Entries(committed),
		m.persistEntryCmd("persist subagent cancellation", session.StoreSubagent{
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
