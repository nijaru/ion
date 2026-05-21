package app

type transcriptReducer struct {
	app *AppState
}

func (m *Model) transcriptReducer() transcriptReducer {
	return transcriptReducer{app: &m.App}
}

func (r transcriptReducer) markPrinted() {
	r.app.PrintedTranscript = true
}
