package app

import (
	"context"
	"fmt"
	"slices"
	"strings"

	tea "charm.land/bubbletea/v2"
)

const maxInputHistoryEntries = 200

func (m *Model) updateComposer(msg tea.Msg) tea.Cmd {
	cmd := m.inputReducer().updateComposer(msg)
	m.relayoutComposer()
	m.refreshComposerCompletions()
	return cmd
}

func (m *Model) insertComposerText(value string) {
	m.inputReducer().insertComposerText(value)
	m.relayoutComposer()
	m.refreshComposerCompletions()
}

func (m *Model) clearPasteMarkers() {
	m.inputReducer().clearPasteMarkers()
}

func (m *Model) resetHistoryCursor() {
	m.inputReducer().resetHistoryCursor()
}

func (m *Model) resetComposerDraft() {
	m.inputReducer().resetComposerDraft()
	m.relayoutComposer()
}

func (m *Model) setComposerDraft(value string) {
	m.inputReducer().setComposerDraft(value)
	m.relayoutComposer()
	m.refreshComposerCompletions()
}

func (m *Model) appendInputHistory(text string) (string, bool) {
	return m.inputReducer().appendHistory(text)
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
	m.inputReducer().setHistory(inputs)
}

func (m Model) persistInputHistory(ctx context.Context, text string) tea.Cmd {
	if m.Model.Store == nil || strings.TrimSpace(m.App.Workdir) == "" {
		return nil
	}
	store := m.Model.Store
	workdir := m.App.Workdir
	return func() tea.Msg {
		if err := store.AddInput(ctx, workdir, text); err != nil {
			return localErrorMsg{err: fmt.Errorf("persist input history: %w", err)}
		}
		return nil
	}
}
