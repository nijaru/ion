package app

import (
	"context"
	"slices"
	"strings"

	tea "charm.land/bubbletea/v2"
)

const maxInputHistoryEntries = 200

func (m *Model) updateComposer(msg tea.Msg) tea.Cmd {
	var cmd tea.Cmd
	m.Input.Composer, cmd = m.Input.Composer.Update(msg)
	m.relayoutComposer()
	return cmd
}

func (m *Model) insertComposerText(value string) {
	m.Input.Composer.InsertString(value)
	m.relayoutComposer()
}

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

func (m *Model) loadInputHistory(ctx context.Context) {
	if m.Model.Store == nil || strings.TrimSpace(m.App.Workdir) == "" {
		return
	}
	inputs, err := m.Model.Store.GetInputs(ctx, m.App.Workdir, maxInputHistoryEntries)
	if err != nil {
		return
	}
	slices.Reverse(inputs)
	m.Input.History = inputs
	m.resetHistoryCursor()
}

func (m Model) persistInputHistory(ctx context.Context, text string) tea.Cmd {
	if m.Model.Store == nil || strings.TrimSpace(m.App.Workdir) == "" {
		return nil
	}
	if err := m.Model.Store.AddInput(ctx, m.App.Workdir, text); err != nil {
		return persistErrorCmd("persist input history", err)
	}
	return nil
}
