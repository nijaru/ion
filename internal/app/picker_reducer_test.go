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

func TestPickerReducerOverlayLoadRequestsFenceStaleResults(t *testing.T) {
	model := readyModel(t)
	model.Picker.ModelLoadRequest = 7

	model.pickerReducer().openOverlayInvalidatingModelLoads(pickerOverlayState{
		purpose: pickerPurposeProvider,
	})
	if got := model.Picker.ModelLoadRequest; got != 8 {
		t.Fatalf("model load request = %d, want 8", got)
	}

	requestID := model.pickerReducer().beginModelOverlayLoad(pickerOverlayState{
		purpose: pickerPurposeModel,
		loading: true,
	})
	if requestID != 9 {
		t.Fatalf("request id = %d, want 9", requestID)
	}
	if model.Picker.Overlay == nil || model.Picker.Overlay.request != requestID {
		t.Fatalf("overlay = %#v, want request %d", model.Picker.Overlay, requestID)
	}
	if _, ok := model.pickerReducer().modelLoadOverlay(requestID - 1); ok {
		t.Fatal("stale model load request matched overlay")
	}
	if overlay, ok := model.pickerReducer().modelLoadOverlay(requestID); !ok || overlay == nil {
		t.Fatalf("current model load did not match: overlay=%#v ok=%v", overlay, ok)
	}
}

func TestPickerReducerOverlayQueryAndPaging(t *testing.T) {
	model := readyModel(t)
	model.pickerReducer().openOverlay(pickerOverlayState{
		items: []pickerItem{
			{
				Label:  "Alpha",
				Value:  "alpha",
				Search: pickerSearchIndex("Alpha", "alpha", "", "", nil),
			},
			{Label: "Beta", Value: "beta", Search: pickerSearchIndex("Beta", "beta", "", "", nil)},
			{
				Label:  "Gamma",
				Value:  "gamma",
				Search: pickerSearchIndex("Gamma", "gamma", "", "", nil),
			},
		},
		filtered: []pickerItem{
			{Label: "Alpha", Value: "alpha"},
			{Label: "Beta", Value: "beta"},
			{Label: "Gamma", Value: "gamma"},
		},
		index: 2,
	})

	model.pickerReducer().appendOverlayQuery("be")
	if got := model.Picker.Overlay.query; got != "be" {
		t.Fatalf("query = %q, want be", got)
	}
	if len(model.Picker.Overlay.filtered) != 1 ||
		model.Picker.Overlay.filtered[0].Value != "beta" ||
		model.Picker.Overlay.index != 0 {
		t.Fatalf(
			"filtered = %#v index=%d, want beta at index 0",
			model.Picker.Overlay.filtered,
			model.Picker.Overlay.index,
		)
	}

	model.pickerReducer().backspaceOverlayQuery()
	if got := model.Picker.Overlay.query; got != "b" {
		t.Fatalf("query = %q, want b", got)
	}
	model.Picker.Overlay.query = ""
	model.Picker.Overlay.index = 99
	model.pickerReducer().refreshOverlayFilter()
	if len(model.Picker.Overlay.filtered) != 3 || model.Picker.Overlay.index != 2 {
		t.Fatalf(
			"filtered = %#v index=%d, want all items with clamped index 2",
			model.Picker.Overlay.filtered,
			model.Picker.Overlay.index,
		)
	}

	model.pickerReducer().pageOverlaySelection(-1)
	if got := model.Picker.Overlay.index; got != 0 {
		t.Fatalf("index = %d, want 0 after page up clamp", got)
	}
	model.pickerReducer().moveOverlaySelection(10)
	if got := model.Picker.Overlay.index; got != 2 {
		t.Fatalf("index = %d, want max clamp 2", got)
	}
}

func TestPickerReducerProviderSelectionSettlement(t *testing.T) {
	model := readyModel(t)
	model.pickerReducer().openOverlay(pickerOverlayState{purpose: pickerPurposeProvider})

	requestID := model.pickerReducer().beginProviderSelection()
	model.pickerReducer().markProviderOverlayLoading(requestID)
	if model.Picker.Overlay == nil ||
		!model.Picker.Overlay.loading ||
		model.Picker.Overlay.request != requestID {
		t.Fatalf("provider overlay = %#v, want loading request", model.Picker.Overlay)
	}
	if model.pickerReducer().settleProviderSelection(requestID + 1) {
		t.Fatal("stale provider selection settled")
	}
	if !model.Picker.Overlay.loading || model.Picker.ProviderSelectionRequest != requestID {
		t.Fatalf(
			"stale settlement changed state: overlay=%#v request=%d",
			model.Picker.Overlay,
			model.Picker.ProviderSelectionRequest,
		)
	}
	if !model.pickerReducer().settleProviderSelection(requestID) {
		t.Fatal("current provider selection did not settle")
	}
	if model.Picker.ProviderSelectionRequest != 0 ||
		model.Picker.Overlay.loading ||
		model.Picker.Overlay.request != 0 {
		t.Fatalf(
			"provider settlement state = overlay=%#v request=%d, want idle",
			model.Picker.Overlay,
			model.Picker.ProviderSelectionRequest,
		)
	}
}
