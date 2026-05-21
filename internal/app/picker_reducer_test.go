package app

import (
	"errors"
	"testing"

	"github.com/nijaru/ion/internal/storage"
)

func TestPickerReducerAppliesOnlyCurrentSessionLoad(t *testing.T) {
	model := readyModel(t)
	staleRequest := model.pickerReducer().beginSessionLoad()
	currentRequest := model.pickerReducer().beginSessionLoad()

	applied := model.pickerReducer().applySessionLoad(staleRequest, []storage.SessionInfo{
		{
			ID:          "stale",
			Title:       "stale session",
			LastPreview: "old",
		},
	}, nil)
	if applied {
		t.Fatal("stale session load was applied")
	}
	if model.Picker.Session == nil ||
		!model.Picker.Session.loading ||
		model.Picker.Session.request != currentRequest {
		t.Fatalf("session picker = %#v, want current loading request", model.Picker.Session)
	}

	applied = model.pickerReducer().applySessionLoad(currentRequest, []storage.SessionInfo{
		{
			ID:          "current",
			Title:       "current session",
			LastPreview: "recent",
		},
	}, nil)
	if !applied {
		t.Fatal("current session load was not applied")
	}
	if model.Picker.Session == nil ||
		model.Picker.Session.loading ||
		len(model.Picker.Session.items) != 1 ||
		model.Picker.Session.items[0].info.ID != "current" {
		t.Fatalf("session picker = %#v, want loaded current session", model.Picker.Session)
	}
}

func TestPickerReducerSessionLoadErrorReplacesLoadingState(t *testing.T) {
	model := readyModel(t)
	requestID := model.pickerReducer().beginSessionLoad()

	if !model.pickerReducer().applySessionLoad(requestID, nil, errors.New("disk unavailable")) {
		t.Fatal("session load error was not applied")
	}
	if model.Picker.Session == nil ||
		model.Picker.Session.loading ||
		model.Picker.Session.err != "failed to list sessions: disk unavailable" {
		t.Fatalf("session picker = %#v, want visible load error", model.Picker.Session)
	}
}

func TestPickerReducerSessionQueryFiltersAndClampsIndex(t *testing.T) {
	model := readyModel(t)
	model.App.Workdir = "/tmp/project"
	model.Picker.Session = &sessionPickerState{
		items: []sessionPickerItem{
			{info: storage.SessionInfo{
				ID:          "sess-alpha",
				Title:       "alpha plan",
				LastPreview: "review tests",
			}},
			{info: storage.SessionInfo{
				ID:          "sess-beta",
				Title:       "beta resume",
				LastPreview: "continue reducer work",
			}},
		},
		filtered: []sessionPickerItem{
			{info: storage.SessionInfo{ID: "sess-alpha"}},
			{info: storage.SessionInfo{ID: "sess-beta"}},
		},
		index: 1,
	}

	model.pickerReducer().appendSessionQuery("alpha", model.App.Workdir)
	if got := model.Picker.Session.query; got != "alpha" {
		t.Fatalf("query = %q, want alpha", got)
	}
	if len(model.Picker.Session.filtered) != 1 ||
		model.Picker.Session.filtered[0].info.ID != "sess-alpha" ||
		model.Picker.Session.index != 0 {
		t.Fatalf(
			"filtered = %#v index=%d, want alpha at index 0",
			model.Picker.Session.filtered,
			model.Picker.Session.index,
		)
	}

	model.pickerReducer().backspaceSessionQuery(model.App.Workdir)
	if got := model.Picker.Session.query; got != "alph" {
		t.Fatalf("query = %q, want alph", got)
	}
	model.Picker.Session.query = ""
	model.Picker.Session.index = 99
	model.pickerReducer().refreshSessionFilter(model.App.Workdir)
	if len(model.Picker.Session.filtered) != 2 || model.Picker.Session.index != 1 {
		t.Fatalf(
			"filtered = %#v index=%d, want all items with clamped index 1",
			model.Picker.Session.filtered,
			model.Picker.Session.index,
		)
	}
}

func TestPickerReducerSessionSelectionPaging(t *testing.T) {
	model := readyModel(t)
	model.Picker.Session = &sessionPickerState{
		filtered: make([]sessionPickerItem, pickerPageSize+2),
	}
	for i := range model.Picker.Session.filtered {
		model.Picker.Session.filtered[i].info.ID = string(rune('a' + i))
	}

	model.pickerReducer().pageSessionSelection(1)
	if got := model.Picker.Session.index; got != pickerPageSize {
		t.Fatalf("index = %d, want page size %d", got, pickerPageSize)
	}
	model.pickerReducer().moveSessionSelection(10)
	if got, want := model.Picker.Session.index, len(model.Picker.Session.filtered)-1; got != want {
		t.Fatalf("index = %d, want clamped max %d", got, want)
	}
	selected, ok := model.pickerReducer().selectedSession()
	if !ok || selected.ID != string(rune('a'+len(model.Picker.Session.filtered)-1)) {
		t.Fatalf("selected = %#v ok=%v, want final item", selected, ok)
	}
	model.pickerReducer().pageSessionSelection(-1)
	if got := model.Picker.Session.index; got != 1 {
		t.Fatalf("index = %d, want one page above final item", got)
	}
}
