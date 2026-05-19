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
	var cmd tea.Cmd
	m.Input.Composer, cmd = m.Input.Composer.Update(msg)
	m.relayoutComposer()
	m.refreshComposerCompletions()
	return cmd
}

func (m *Model) insertComposerText(value string) {
	m.Input.Composer.InsertString(value)
	m.relayoutComposer()
	m.refreshComposerCompletions()
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
	m.Input.Completion = nil
	m.clearPasteMarkers()
	m.relayoutComposer()
}

func (m *Model) setComposerDraft(value string) {
	m.Input.Composer.SetValue(value)
	m.relayoutComposer()
	m.refreshComposerCompletions()
}

func (m *Model) appendInputHistory(text string) (string, bool) {
	text = strings.TrimSpace(text)
	if text == "" {
		return "", false
	}
	if len(m.Input.History) > 0 && m.Input.History[len(m.Input.History)-1] == text {
		m.resetHistoryCursor()
		return "", false
	}
	m.Input.History = append(m.Input.History, text)
	if overflow := len(m.Input.History) - maxInputHistoryEntries; overflow > 0 {
		m.Input.History = append([]string(nil), m.Input.History[overflow:]...)
	}
	m.resetHistoryCursor()
	return text, true
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
	store := m.Model.Store
	workdir := m.App.Workdir
	return func() tea.Msg {
		if err := store.AddInput(ctx, workdir, text); err != nil {
			return localErrorMsg{err: fmt.Errorf("persist input history: %w", err)}
		}
		return nil
	}
}
