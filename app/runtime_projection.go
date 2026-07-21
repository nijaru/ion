package app

import "github.com/nijaru/ion/internal/agent"

// applyAgentRuntimeSnapshot replaces the TUI's ephemeral runtime projection
// from one authoritative runtime snapshot. It must be complete: resync is a
// recovery boundary, so retaining local queue or phase state would create a
// second, potentially divergent runtime.
func (m *Model) applyAgentRuntimeSnapshot(snapshot agent.RuntimeSnapshot) {
	if snapshot.SessionID != "" {
		m.Model.Runtime.SessionID = snapshot.SessionID
	}
	m.Model.LeafID = snapshot.LeafID
	m.Model.Runtime.Materialized = true
	if snapshot.Model.Provider != "" {
		m.Model.Runtime.Provider = snapshot.Model.Provider
	}
	if snapshot.Model.ID != "" {
		m.Model.Runtime.Model = snapshot.Model.ID
	}
	if snapshot.Thinking != "" {
		m.Model.Runtime.Reasoning = normalizeThinkingValue(string(snapshot.Thinking))
		m.Progress.ReasoningEffort = m.Model.Runtime.Reasoning
	}
	m.Model.ActiveTools = append(m.Model.ActiveTools[:0], snapshot.ActiveTools...)

	turn := m.turnReducer()
	turn.ClearActiveState(true)
	turn.RestoreRuntimePhase(snapshot.Phase)
	steer, followUp, nextTurn := snapshot.Queues.Texts()
	m.InFlight.QueuedSteering = steer
	m.InFlight.QueuedTurns = append(followUp, nextTurn...)
	m.InFlight.QueuedTurnsRuntimeOwned = true
}
