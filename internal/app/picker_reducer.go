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

func (r pickerReducer) openOverlay(state pickerOverlayState) {
	r.picker.Overlay = &state
}

func (r pickerReducer) openOverlayInvalidatingModelLoads(state pickerOverlayState) {
	r.picker.ModelLoadRequest++
	r.openOverlay(state)
}

func (r pickerReducer) beginModelOverlayLoad(state pickerOverlayState) uint64 {
	r.picker.ModelLoadRequest++
	requestID := r.picker.ModelLoadRequest
	state.request = requestID
	r.openOverlay(state)
	return requestID
}

func (r pickerReducer) modelSetupRequestMatches(requestID uint64) bool {
	overlay := r.picker.Overlay
	if overlay == nil ||
		overlay.purpose != pickerPurposeModel ||
		!overlay.setup ||
		overlay.request != requestID ||
		requestID != r.picker.ModelLoadRequest {
		return false
	}
	return true
}

func (r pickerReducer) failModelSetup(requestID uint64, message string) bool {
	if !r.modelSetupRequestMatches(requestID) {
		return false
	}
	r.picker.Overlay.loading = false
	r.picker.Overlay.setup = false
	r.picker.Overlay.err = message
	return true
}

func (r pickerReducer) modelLoadRequestMatches(requestID uint64) bool {
	overlay := r.picker.Overlay
	if overlay == nil ||
		overlay.purpose != pickerPurposeModel ||
		overlay.request != requestID ||
		requestID != r.picker.ModelLoadRequest {
		return false
	}
	return true
}

func (r pickerReducer) failModelLoad(requestID uint64, message string) bool {
	if !r.modelLoadRequestMatches(requestID) {
		return false
	}
	r.picker.Overlay.loading = false
	r.picker.Overlay.err = message
	if len(r.picker.Overlay.items) == 0 {
		r.picker.Overlay.filtered = nil
	}
	return true
}

func (r pickerReducer) completeModelLoad(
	requestID uint64,
	items []pickerItem,
	selectedValue string,
) bool {
	if !r.modelLoadRequestMatches(requestID) {
		return false
	}
	r.picker.Overlay.loading = false
	r.picker.Overlay.err = ""
	r.picker.Overlay.items = items
	r.picker.Overlay.filtered = clonePickerItems(items)
	r.picker.Overlay.index = pickerIndex(items, selectedValue)
	r.refreshOverlayFilter()
	return true
}

func (r pickerReducer) closeOverlay() {
	r.picker.Overlay = nil
}

func (r pickerReducer) openSetup(state setupPromptState) {
	r.picker.Overlay = nil
	r.picker.Setup = &state
}

func (r pickerReducer) closeSetup() {
	r.picker.Setup = nil
}

func (r pickerReducer) appendSetupValue(text string) {
	if r.picker.Setup == nil || text == "" {
		return
	}
	r.picker.Setup.value += text
	r.picker.Setup.err = ""
}

func (r pickerReducer) backspaceSetupValue() {
	if r.picker.Setup == nil || r.picker.Setup.value == "" {
		return
	}
	_, size := utf8.DecodeLastRuneInString(r.picker.Setup.value)
	r.picker.Setup.value = r.picker.Setup.value[:len(r.picker.Setup.value)-size]
}

func (r pickerReducer) setSetupError(message string) {
	if r.picker.Setup != nil {
		r.picker.Setup.err = message
	}
}

func (r pickerReducer) beginSetupSave() (uint64, bool) {
	if r.picker.Setup == nil {
		return 0, false
	}
	r.picker.SetupSaveRequest++
	requestID := r.picker.SetupSaveRequest
	r.picker.Setup.saving = true
	r.picker.Setup.request = requestID
	r.picker.Setup.err = ""
	return requestID, true
}

func (r pickerReducer) failSetupSave(requestID uint64, message string) bool {
	if !r.setupSaveMatches(requestID) {
		return false
	}
	r.picker.SetupSaveRequest = 0
	r.picker.Setup.saving = false
	r.picker.Setup.request = 0
	r.picker.Setup.err = message
	return true
}

func (r pickerReducer) completeSetupSave(requestID uint64) bool {
	if !r.setupSaveMatches(requestID) {
		return false
	}
	r.picker.SetupSaveRequest = 0
	r.picker.Setup = nil
	return true
}

func (r pickerReducer) setupSaveMatches(requestID uint64) bool {
	return requestID != 0 &&
		requestID == r.picker.SetupSaveRequest &&
		r.picker.Setup != nil &&
		r.picker.Setup.request == requestID
}

func (r pickerReducer) appendOverlayQuery(text string) {
	if r.picker.Overlay == nil || text == "" {
		return
	}
	r.picker.Overlay.query += text
	r.refreshOverlayFilter()
}

func (r pickerReducer) backspaceOverlayQuery() {
	if r.picker.Overlay == nil || r.picker.Overlay.query == "" {
		return
	}
	_, size := utf8.DecodeLastRuneInString(r.picker.Overlay.query)
	r.picker.Overlay.query = r.picker.Overlay.query[:len(r.picker.Overlay.query)-size]
	r.refreshOverlayFilter()
}

func (r pickerReducer) moveOverlaySelection(delta int) {
	if r.picker.Overlay == nil {
		return
	}
	items := pickerDisplayItems(r.picker.Overlay)
	if len(items) == 0 {
		return
	}
	next := r.picker.Overlay.index + delta
	if next < 0 {
		next = 0
	}
	if max := len(items) - 1; next > max {
		next = max
	}
	r.picker.Overlay.index = next
}

func (r pickerReducer) pageOverlaySelection(delta int) {
	r.moveOverlaySelection(delta * pickerPageSize)
}

func (r pickerReducer) refreshOverlayFilter() {
	if r.picker.Overlay == nil {
		return
	}
	query := strings.TrimSpace(r.picker.Overlay.query)
	if query == "" {
		r.picker.Overlay.filtered = append([]pickerItem(nil), r.picker.Overlay.items...)
		if len(r.picker.Overlay.filtered) == 0 {
			r.picker.Overlay.index = 0
			return
		}
		if r.picker.Overlay.index >= len(r.picker.Overlay.filtered) {
			r.picker.Overlay.index = len(r.picker.Overlay.filtered) - 1
		}
		return
	}
	r.picker.Overlay.filtered = rankedPickerItems(r.picker.Overlay.items, query)
	if len(r.picker.Overlay.filtered) == 0 {
		r.picker.Overlay.index = 0
		return
	}
	r.picker.Overlay.index = 0
}

func (r pickerReducer) beginProviderSelection() uint64 {
	r.picker.ProviderSelectionRequest++
	return r.picker.ProviderSelectionRequest
}

func (r pickerReducer) markProviderOverlayLoading(requestID uint64) {
	if r.picker.Overlay == nil || r.picker.Overlay.purpose != pickerPurposeProvider {
		return
	}
	r.picker.Overlay.loading = true
	r.picker.Overlay.err = ""
	r.picker.Overlay.request = requestID
}

func (r pickerReducer) settleProviderSelection(requestID uint64) bool {
	if requestID == 0 || requestID != r.picker.ProviderSelectionRequest {
		return false
	}
	r.picker.ProviderSelectionRequest = 0
	if r.picker.Overlay != nil &&
		r.picker.Overlay.purpose == pickerPurposeProvider &&
		r.picker.Overlay.request == requestID {
		r.picker.Overlay.loading = false
		r.picker.Overlay.request = 0
	}
	return true
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
