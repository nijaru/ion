package app

import (
	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/session"
)

func (m Model) handleSubagentMessage(msg session.AgentMessage) (Model, tea.Cmd) {
	committed, ok := m.turnReducer().CommitSubagentMessage(
		msg.AgentID,
		msg.Message,
		msg.Timestamp,
	)
	if !ok {
		return m, m.awaitSessionEvent()
	}
	return m, tea.Sequence(m.terminalCommit().Entries(committed), m.awaitSessionEvent())
}

func (m Model) handleChildRequested(msg session.ChildRequest) (Model, tea.Cmd) {
	p := m.turnReducer().RequestChild(msg.AgentName, msg.Query)

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

func (m Model) handleChildStarted(msg session.ChildStart) (Model, tea.Cmd) {
	m.turnReducer().StartChild(msg.AgentName)
	return m, m.awaitSessionEvent()
}

func (m Model) handleChildDelta(msg session.ChildDelta) (Model, tea.Cmd) {
	m.turnReducer().AppendChildDelta(msg.AgentName, msg.Delta)
	return m, m.awaitSessionEvent()
}

func (m Model) handleChildCompleted(msg session.ChildComplete) (Model, tea.Cmd) {
	committed, ok := m.turnReducer().CompleteChild(msg.AgentName, msg.Result, msg.Timestamp)
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

func (m Model) handleChildBlocked(msg session.ChildBlock) (Model, tea.Cmd) {
	m.turnReducer().BlockChild(msg.AgentName, msg.Reason)
	return m, m.awaitSessionEvent()
}

func (m Model) handleChildFailed(msg session.ChildFail) (Model, tea.Cmd) {
	committed, ok := m.turnReducer().FailChild(msg.AgentName, msg.Error, msg.Timestamp)
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

func (m Model) handleChildCanceled(msg session.ChildCancel) (Model, tea.Cmd) {
	committed, ok := m.turnReducer().CancelChild(msg.AgentName, msg.Reason, msg.Timestamp)
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
