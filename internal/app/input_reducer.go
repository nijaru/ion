package app

import (
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
)

type inputReducer struct {
	input        *InputState
	pasteMarkers *map[string]pasteMarker
}

func (m *Model) inputReducer() inputReducer {
	return inputReducer{
		input:        &m.Input,
		pasteMarkers: &m.PasteMarkers,
	}
}

func (r inputReducer) updateComposer(msg tea.Msg) tea.Cmd {
	var cmd tea.Cmd
	r.input.Composer, cmd = r.input.Composer.Update(msg)
	return cmd
}

func (r inputReducer) insertComposerText(value string) {
	r.input.Composer.InsertString(value)
}

func (r inputReducer) resetComposerDraft() {
	r.input.Composer.Reset()
	r.clearCompletion()
	if r.pasteMarkers != nil {
		*r.pasteMarkers = make(map[string]pasteMarker)
	}
}

func (r inputReducer) setComposerDraft(value string) {
	r.input.Composer.SetValue(value)
}

func (r inputReducer) resetHistoryCursor() {
	r.input.HistoryIdx = -1
	r.input.HistoryDraft = ""
}

func (r inputReducer) appendHistory(text string) (string, bool) {
	text = strings.TrimSpace(text)
	if text == "" {
		return "", false
	}
	if len(r.input.History) > 0 && r.input.History[len(r.input.History)-1] == text {
		r.resetHistoryCursor()
		return "", false
	}
	r.input.History = append(r.input.History, text)
	if overflow := len(r.input.History) - maxInputHistoryEntries; overflow > 0 {
		r.input.History = append([]string(nil), r.input.History[overflow:]...)
	}
	r.resetHistoryCursor()
	return text, true
}

func (r inputReducer) setHistory(inputs []string) {
	r.input.History = inputs
	r.resetHistoryCursor()
}

func (r inputReducer) setCompletionItems(items []completionItem) {
	if len(items) == 0 {
		r.clearCompletion()
		return
	}
	r.input.Completion = &completionState{items: items}
}

func (r inputReducer) clearCompletion() {
	r.input.Completion = nil
}

func (r inputReducer) clearPasteMarkers() {
	if r.pasteMarkers == nil {
		return
	}
	*r.pasteMarkers = make(map[string]pasteMarker)
}

func (r inputReducer) previousHistoryDraft(current string) (string, bool) {
	if len(r.input.History) == 0 {
		return "", false
	}
	if r.input.HistoryIdx == -1 {
		r.input.HistoryDraft = current
		r.input.HistoryIdx = len(r.input.History) - 1
		return r.input.History[r.input.HistoryIdx], true
	}
	if r.input.HistoryIdx <= 0 {
		return "", false
	}
	r.input.HistoryIdx--
	return r.input.History[r.input.HistoryIdx], true
}

func (r inputReducer) nextHistoryDraft() (string, bool) {
	if r.input.HistoryIdx == -1 {
		return "", false
	}
	if r.input.HistoryIdx < len(r.input.History)-1 {
		r.input.HistoryIdx++
		return r.input.History[r.input.HistoryIdx], true
	}
	draft := r.input.HistoryDraft
	r.resetHistoryCursor()
	return draft, true
}

func (r inputReducer) browsingHistory() bool {
	return r.input.HistoryIdx != -1
}

func (r inputReducer) clearPendingAction() {
	r.input.Pending = pendingActionNone
}

func (r inputReducer) armPendingAction(action pendingAction) {
	r.input.Pending = action
}

func (r inputReducer) holdEnter(delay time.Duration) {
	if delay > r.input.PrintHoldDelay {
		r.input.PrintHoldDelay = delay
	}
	r.input.DelayNextEnter = true
}

func (r inputReducer) startDeferredEnter(until time.Time) {
	r.input.DelayNextEnter = false
	r.input.DeferredEnter = true
	r.input.PrintHoldUntil = until
}

func (r inputReducer) markDeferredEnter() {
	r.input.DeferredEnter = true
}

func (r inputReducer) finishDeferredEnter() {
	r.input.DeferredEnter = false
	r.input.PrintHoldDelay = 0
}
