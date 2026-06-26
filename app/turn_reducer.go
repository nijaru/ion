package app


// turnReducer creates a TurnReducer from the Model's in-flight and
// progress state. This is a thin adapter — all logic lives in 
func (m *Model) turnReducer() TurnReducer {
	return NewTurnReducer(&m.InFlight, &m.Progress)
}
