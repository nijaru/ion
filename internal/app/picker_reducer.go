package app

import (
	"fmt"
	"strings"
	"unicode/utf8"

	"github.com/nijaru/ion/internal/storage"
)

type pickerReducer struct {
	picker *PickerState
}

func (m *Model) pickerReducer() pickerReducer {
	return pickerReducer{picker: &m.Picker}
}

func (r pickerReducer) showSessionUnavailable() {
	r.picker.Overlay = nil
	r.picker.Session = &sessionPickerState{err: "session store not available"}
}

func (r pickerReducer) beginSessionLoad() uint64 {
	r.picker.Overlay = nil
	r.picker.SessionLoadRequest++
	requestID := r.picker.SessionLoadRequest
	r.picker.Session = &sessionPickerState{
		loading: true,
		request: requestID,
	}
	return requestID
}

func (r pickerReducer) applySessionLoad(
	requestID uint64,
	sessions []storage.SessionInfo,
	err error,
) bool {
	if !r.sessionLoadMatches(requestID) {
		return false
	}
	if err != nil {
		r.picker.Session = &sessionPickerState{
			err: fmt.Sprintf("failed to list sessions: %v", err),
		}
		return true
	}
	items := make([]sessionPickerItem, 0, len(sessions))
	for _, info := range sessions {
		if !storage.IsConversationSessionInfo(info) {
			continue
		}
		items = append(items, sessionPickerItem{info: info})
	}

	state := &sessionPickerState{
		items:    items,
		filtered: append([]sessionPickerItem(nil), items...),
		index:    0,
	}
	if len(items) == 0 {
		state.err = "no recent sessions in this workspace"
	}
	r.picker.Session = state
	return true
}

func (r pickerReducer) sessionLoadMatches(requestID uint64) bool {
	return r.picker.Session != nil &&
		r.picker.Session.request != 0 &&
		r.picker.Session.request == requestID &&
		requestID == r.picker.SessionLoadRequest
}

func (r pickerReducer) closeSession() {
	r.picker.Session = nil
}

func (r pickerReducer) selectedSession() (storage.SessionInfo, bool) {
	if r.picker.Session == nil || len(r.picker.Session.filtered) == 0 {
		return storage.SessionInfo{}, false
	}
	index := r.picker.Session.index
	if index < 0 || index >= len(r.picker.Session.filtered) {
		return storage.SessionInfo{}, false
	}
	return r.picker.Session.filtered[index].info, true
}

func (r pickerReducer) appendSessionQuery(text, workdir string) {
	if r.picker.Session == nil || text == "" {
		return
	}
	r.picker.Session.query += text
	r.refreshSessionFilter(workdir)
}

func (r pickerReducer) backspaceSessionQuery(workdir string) {
	if r.picker.Session == nil || r.picker.Session.query == "" {
		return
	}
	_, size := utf8.DecodeLastRuneInString(r.picker.Session.query)
	r.picker.Session.query = r.picker.Session.query[:len(r.picker.Session.query)-size]
	r.refreshSessionFilter(workdir)
}

func (r pickerReducer) moveSessionSelection(delta int) {
	if r.picker.Session == nil || len(r.picker.Session.filtered) == 0 {
		return
	}
	next := r.picker.Session.index + delta
	if next < 0 {
		next = 0
	}
	if max := len(r.picker.Session.filtered) - 1; next > max {
		next = max
	}
	r.picker.Session.index = next
}

func (r pickerReducer) pageSessionSelection(delta int) {
	r.moveSessionSelection(delta * pickerPageSize)
}

func (r pickerReducer) refreshSessionFilter(workdir string) {
	if r.picker.Session == nil {
		return
	}
	query := strings.TrimSpace(r.picker.Session.query)
	if query == "" {
		r.picker.Session.filtered = append(
			[]sessionPickerItem(nil),
			r.picker.Session.items...,
		)
		if len(r.picker.Session.filtered) == 0 {
			r.picker.Session.index = 0
			return
		}
		if r.picker.Session.index >= len(r.picker.Session.filtered) {
			r.picker.Session.index = len(r.picker.Session.filtered) - 1
		}
		return
	}
	r.picker.Session.filtered = rankedSessionPickerItems(
		r.picker.Session.items,
		query,
		workdir,
	)
	if len(r.picker.Session.filtered) == 0 {
		r.picker.Session.index = 0
		return
	}
	r.picker.Session.index = 0
}
