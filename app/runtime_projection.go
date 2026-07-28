package app

import "github.com/nijaru/ion/internal/agent"

// applyAgentRuntimeSnapshot replaces the TUI's ephemeral runtime projection
// from one authoritative runtime snapshot. It must be complete: resync is a
// recovery boundary, so retaining local queue or phase state would create a
// second, potentially divergent runtime.
func (m *Model) applyAgentRuntimeSnapshot(snapshot agent.RuntimeSnapshot) {
	// RuntimeSnapshot is authoritative, including empty values. Conditional
	// assignment would retain state from the previous snapshot when a runtime
	// clears its provider, model, or thinking selection.
	m.Model.Runtime.SessionID = snapshot.SessionID
	m.Model.LeafID = snapshot.LeafID
	m.Model.Runtime.Materialized = true
	m.Model.Runtime.Provider = snapshot.Model.Provider
	m.Model.Runtime.Model = snapshot.Model.ID
	m.Model.Runtime.Reasoning = normalizeThinkingValue(string(snapshot.Thinking))
	m.Progress.ReasoningEffort = m.Model.Runtime.Reasoning
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
