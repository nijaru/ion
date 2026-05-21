package app

import (
	"fmt"
	"testing"
	"time"
)

func TestInputReducerResetComposerDraftClearsCompletionAndPasteMarkers(t *testing.T) {
	model := readyModel(t)
	model.Input.Composer.SetValue("draft")
	model.Input.Completion = &completionState{items: []completionItem{{Label: "read"}}}
	model.PasteMarkers = map[string]pasteMarker{
		"[paste #1]": {content: "expanded"},
	}

	model.inputReducer().resetComposerDraft()

	if got := model.Input.Composer.Value(); got != "" {
		t.Fatalf("composer = %q, want empty", got)
	}
	if model.Input.Completion != nil {
		t.Fatalf("completion = %#v, want nil", model.Input.Completion)
	}
	if len(model.PasteMarkers) != 0 {
		t.Fatalf("paste markers = %#v, want cleared", model.PasteMarkers)
	}
}

func TestInputReducerAppendHistoryTrimsDedupesCapsAndResetsCursor(t *testing.T) {
	model := readyModel(t)
	for i := range maxInputHistoryEntries {
		model.Input.History = append(model.Input.History, fmt.Sprintf("old-%d", i))
	}
	model.Input.HistoryIdx = 10
	model.Input.HistoryDraft = "draft"

	text, changed := model.inputReducer().appendHistory("  newest  ")
	if !changed || text != "newest" {
		t.Fatalf("appendHistory = %q/%v, want newest/true", text, changed)
	}
	if len(model.Input.History) != maxInputHistoryEntries {
		t.Fatalf(
			"history len = %d, want capped %d",
			len(model.Input.History),
			maxInputHistoryEntries,
		)
	}
	if model.Input.History[0] != "old-1" ||
		model.Input.History[len(model.Input.History)-1] != "newest" {
		t.Fatalf(
			"history cap = first %q last %q, want old-1/newest",
			model.Input.History[0],
			model.Input.History[len(model.Input.History)-1],
		)
	}
	if model.Input.HistoryIdx != -1 || model.Input.HistoryDraft != "" {
		t.Fatalf(
			"history cursor = %d/%q, want reset",
			model.Input.HistoryIdx,
			model.Input.HistoryDraft,
		)
	}

	text, changed = model.inputReducer().appendHistory("newest")
	if changed || text != "" {
		t.Fatalf("duplicate appendHistory = %q/%v, want empty/false", text, changed)
	}
}

func TestInputReducerHistoryNavigationPreservesDraft(t *testing.T) {
	model := readyModel(t)
	model.Input.History = []string{"first", "second"}

	draft, ok := model.inputReducer().previousHistoryDraft("current draft")
	if !ok || draft != "second" {
		t.Fatalf("previous = %q/%v, want second/true", draft, ok)
	}
	draft, ok = model.inputReducer().previousHistoryDraft("ignored")
	if !ok || draft != "first" {
		t.Fatalf("previous = %q/%v, want first/true", draft, ok)
	}
	if _, ok = model.inputReducer().previousHistoryDraft("ignored"); ok {
		t.Fatal("previous at first item returned ok=true")
	}

	draft, ok = model.inputReducer().nextHistoryDraft()
	if !ok || draft != "second" {
		t.Fatalf("next = %q/%v, want second/true", draft, ok)
	}
	draft, ok = model.inputReducer().nextHistoryDraft()
	if !ok || draft != "current draft" {
		t.Fatalf("next = %q/%v, want original draft/true", draft, ok)
	}
	if model.inputReducer().browsingHistory() {
		t.Fatal("history cursor still active after returning to draft")
	}
}

func TestInputReducerPendingActionAndDeferredEnterState(t *testing.T) {
	model := readyModel(t)

	model.inputReducer().armPendingAction(pendingActionQuitCtrlC)
	if model.Input.Pending != pendingActionQuitCtrlC {
		t.Fatalf("pending = %v, want ctrl-c", model.Input.Pending)
	}
	model.inputReducer().clearPendingAction()
	if model.Input.Pending != pendingActionNone {
		t.Fatalf("pending = %v, want none", model.Input.Pending)
	}

	model.inputReducer().holdEnter(50 * time.Millisecond)
	if !model.Input.DelayNextEnter || model.Input.PrintHoldDelay != 50*time.Millisecond {
		t.Fatalf(
			"hold state = delayNext=%v delay=%s, want true/50ms",
			model.Input.DelayNextEnter,
			model.Input.PrintHoldDelay,
		)
	}
	until := time.Now().Add(time.Second)
	model.inputReducer().startDeferredEnter(until)
	if model.Input.DelayNextEnter ||
		!model.Input.DeferredEnter ||
		!model.Input.PrintHoldUntil.Equal(until) {
		t.Fatalf("deferred state = %#v, want active deferred enter", model.Input)
	}
	model.inputReducer().finishDeferredEnter()
	if model.Input.DeferredEnter || model.Input.PrintHoldDelay != 0 {
		t.Fatalf("deferred state = %#v, want finished", model.Input)
	}
}
