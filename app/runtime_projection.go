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
	if snapshot.ActiveTurnToken == 0 {
		if m.Model.turnCancellation == nil || m.Model.turnCancellation.isStarted() {
			m.clearTurnCancellation()
		}
	} else {
		if m.Model.turnCancellation == nil ||
			m.Model.turnCancellation.turnToken() != snapshot.ActiveTurnToken {
			state, _ := newTurnCancellationState(m.runtimeOperationContext())
			m.replaceTurnCancellation(state)
		}
		m.Model.turnCancellation.setToken(snapshot.ActiveTurnToken)
		m.Model.turnCancellation.markStarted()
	}

	// Approval state is runtime-owned and may have changed while the previous
	// subscription was lagged. Replace the prompt wholesale so a missed
	// resolution cannot leave stale input interception in the TUI.
	m.replaceApprovalRequests(snapshot.PendingApprovals)

	turn := m.turnReducer()
	turn.ClearActiveState(true)
	turn.RestoreRuntimePhase(snapshot.Phase)
	m.preserveCancellationProjection()
	steer, followUp, nextTurn := snapshot.Queues.Texts()
	m.InFlight.QueuedSteering = steer
	m.InFlight.QueuedTurns = append(followUp, nextTurn...)
}
