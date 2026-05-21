package app

import (
	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func (m Model) handleSubagentMessage(msg session.AgentMessage) (Model, tea.Cmd) {
	p, ok := m.InFlight.Subagents[msg.AgentID]
	if !ok {
		return m, m.awaitSessionEvent()
	}
	content := p.Output
	if msg.Message != "" {
		content = msg.Message
	}
	committed := session.Entry{
		Role:      session.Subagent,
		Timestamp: msg.Timestamp,
		Title:     p.Name,
		Content:   "Completed: " + content,
		Reasoning: p.Reasoning,
	}
	delete(m.InFlight.Subagents, msg.AgentID)
	return m, tea.Sequence(m.printEntries(committed), m.awaitSessionEvent())
}

func (m Model) handleChildRequested(msg session.ChildRequested) (Model, tea.Cmd) {
	p := &SubagentProgress{
		ID:     msg.AgentName,
		Name:   msg.AgentName,
		Intent: msg.Query,
		Status: "Requested",
	}
	if m.InFlight.Subagents == nil {
		m.InFlight.Subagents = make(map[string]*SubagentProgress)
	}
	m.InFlight.Subagents[msg.AgentName] = p
	m.Progress.Mode = stateWorking

	entry := session.Entry{
		Role:      session.Subagent,
		Timestamp: msg.Timestamp,
		Title:     p.Name,
		Content:   "Started: " + p.Intent,
	}
	return m, sequenceCmds(
		m.printEntries(entry),
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
	if p, ok := m.InFlight.Subagents[msg.AgentName]; ok {
		p.Status = "Started"
		m.Progress.Mode = stateWorking
	}
	return m, m.awaitSessionEvent()
}

func (m Model) handleChildDelta(msg session.ChildDelta) (Model, tea.Cmd) {
	if p, ok := m.InFlight.Subagents[msg.AgentName]; ok {
		p.Output += msg.Delta
	}
	return m, m.awaitSessionEvent()
}

func (m Model) handleChildCompleted(msg session.ChildCompleted) (Model, tea.Cmd) {
	p, ok := m.InFlight.Subagents[msg.AgentName]
	if !ok {
		return m, m.awaitSessionEvent()
	}
	p.Status = "Completed"
	p.Output = msg.Result
	committed := session.Entry{
		Role:      session.Subagent,
		Timestamp: msg.Timestamp,
		Title:     p.Name,
		Content:   "Completed: " + p.Output,
	}
	delete(m.InFlight.Subagents, msg.AgentName)
	m.Progress.Status = ""
	switch {
	case len(m.InFlight.Subagents) > 0:
		m.Progress.Mode = stateWorking
	case m.InFlight.Thinking:
		m.Progress.Mode = stateIonizing
	default:
		m.Progress.Mode = stateComplete
	}

	return m, sequenceCmds(
		m.printEntries(committed),
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
	if p, ok := m.InFlight.Subagents[msg.AgentName]; ok {
		p.Status = "Blocked"
		p.Output = "BLOCKED: " + msg.Reason
		m.Progress.Mode = stateBlocked
		m.InFlight.Thinking = false
	}
	return m, m.awaitSessionEvent()
}

func (m Model) handleChildFailed(msg session.ChildFailed) (Model, tea.Cmd) {
	p, ok := m.InFlight.Subagents[msg.AgentName]
	if !ok {
		return m, m.awaitSessionEvent()
	}
	p.Status = "Failed"
	p.Output = "ERROR: " + msg.Error
	committed := session.Entry{
		Role:      session.Subagent,
		Timestamp: msg.Timestamp,
		Title:     p.Name,
		Content:   "Failed: " + msg.Error,
		IsError:   true,
	}
	delete(m.InFlight.Subagents, msg.AgentName)
	m.Progress.Mode = stateError
	m.Progress.LastError = "Subagent failed: " + msg.Error

	return m, sequenceCmds(
		m.printEntries(committed),
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
	p, ok := m.InFlight.Subagents[msg.AgentName]
	if !ok {
		return m, m.awaitSessionEvent()
	}
	p.Status = "Canceled"
	p.Output = childCanceledContent(msg.Reason)
	committed := session.Entry{
		Role:      session.Subagent,
		Timestamp: msg.Timestamp,
		Title:     p.Name,
		Content:   p.Output,
	}
	delete(m.InFlight.Subagents, msg.AgentName)
	m.Progress.Status = ""
	switch {
	case len(m.InFlight.Subagents) > 0:
		m.Progress.Mode = stateWorking
	case m.InFlight.Thinking:
		m.Progress.Mode = stateIonizing
	default:
		m.Progress.Mode = stateComplete
	}

	return m, sequenceCmds(
		m.printEntries(committed),
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
