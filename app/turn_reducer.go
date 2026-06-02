package app

import "github.com/nijaru/ion/internal/core"

// turnReducer creates a core.TurnReducer from the Model's in-flight and
// progress state. This is a thin adapter — all logic lives in core.
func (m *Model) turnReducer() core.TurnReducer {
	return core.NewTurnReducer(&m.InFlight, &m.Progress)
}
