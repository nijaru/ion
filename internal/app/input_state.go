package app

func (m *Model) clearPasteMarkers() {
	m.PasteMarkers = make(map[string]pasteMarker)
}

func (m *Model) resetHistoryCursor() {
	m.Input.HistoryIdx = -1
	m.Input.HistoryDraft = ""
}

func (m *Model) resetComposerDraft() {
	m.Input.Composer.Reset()
	m.clearPasteMarkers()
	m.relayoutComposer()
}

func (m *Model) setComposerDraft(value string) {
	m.Input.Composer.SetValue(value)
	m.relayoutComposer()
}
