package app

import (
	"fmt"
	"strings"
	"testing"
	"time"

	ionclipboard "github.com/nijaru/ion/internal/clipboard"
	"github.com/nijaru/ion/session"
)

func TestInputReducerResetComposerDraftClearsCompletionAndPasteMarkers(t *testing.T) {
	model := readyModel(t)
	model.Input.Composer.SetValue("draft")
	model.Input.Images = []session.ImageContent{{Data: []byte{1}, MimeType: "image/png"}}
	model.Input.Completion = &completionState{items: []completionItem{{Label: "read"}}}
	model.PasteMarkers = map[string]pasteMarker{
		"[paste #1]": {content: "expanded"},
	}

	model.inputReducer().resetComposerDraft()

	if got := model.Input.Composer.Value(); got != "" {
		t.Fatalf("composer = %q, want empty", got)
	}
	if len(model.Input.Images) != 0 {
		t.Fatalf("images = %#v, want none", model.Input.Images)
	}
	if model.Input.Completion != nil {
		t.Fatalf("completion = %#v, want nil", model.Input.Completion)
	}
	if len(model.PasteMarkers) != 0 {
		t.Fatalf("paste markers = %#v, want cleared", model.PasteMarkers)
	}
}

func TestClipboardImageAttachmentFlowsIntoPrompt(t *testing.T) {
	model := readyModel(t)
	runner := &stubRunner{}
	model.Model.Runner = runner

	data := []byte{1, 2, 3, 4}
	model, _ = model.attachClipboardImage(&ionclipboard.ImageData{
		Bytes:    data,
		MimeType: "image/jpeg",
		FilePath: "/tmp/clipboard-image.jpg",
	})
	data[0] = 99
	model.Input.Composer.SetValue("describe this")

	updated, cmd := model.submitComposer()
	if cmd == nil {
		t.Fatal("submit command = nil, want prompt command")
	}
	if len(updated.Input.Images) != 0 {
		t.Fatalf("images after submit = %#v, want cleared", updated.Input.Images)
	}
	message := cmd()
	result, ok := message.(turnSubmitResultMsg)
	if !ok {
		t.Fatalf("submit result = %T, want turnSubmitResultMsg", message)
	}
	if result.err != nil {
		t.Fatalf("submit error = %v", result.err)
	}
	if len(runner.promptImages) != 1 || len(runner.promptImages[0]) != 1 {
		t.Fatalf("prompt images = %#v, want one attachment", runner.promptImages)
	}
	image := runner.promptImages[0][0]
	if image.MimeType != "image/jpeg" || string(image.Data) != string([]byte{1, 2, 3, 4}) {
		t.Fatalf("prompt image = %#v, want copied jpeg bytes", image)
	}
}

func TestFailedImagePromptRestoresAttachment(t *testing.T) {
	model := readyModel(t)
	runner := &stubRunner{promptErr: fmt.Errorf("provider unavailable")}
	model.Model.Runner = runner
	model, _ = model.attachClipboardImage(&ionclipboard.ImageData{
		Bytes:    []byte{5, 6, 7},
		MimeType: "image/png",
	})
	model.Input.Composer.SetValue("describe this")

	submitted, cmd := model.submitComposer()
	message := cmd()
	result, ok := message.(turnSubmitResultMsg)
	if !ok {
		t.Fatalf("submit result = %T, want turnSubmitResultMsg", message)
	}
	if result.err == nil {
		t.Fatal("submit error = nil, want provider error")
	}
	recovered, _ := submitted.handleTurnSubmitResult(result)
	if len(recovered.Input.Images) != 1 {
		t.Fatalf("restored images = %#v, want one attachment", recovered.Input.Images)
	}
	if string(recovered.Input.Images[0].Data) != string([]byte{5, 6, 7}) {
		t.Fatalf("restored image data = %v, want original bytes", recovered.Input.Images[0].Data)
	}
}

func TestInputReducerCompletionAndPasteMarkerOwnership(t *testing.T) {
	model := readyModel(t)

	model.inputReducer().setCompletionItems([]completionItem{{Label: "/settings"}})
	if model.Input.Completion == nil ||
		len(model.Input.Completion.items) != 1 ||
		model.Input.Completion.items[0].Label != "/settings" {
		t.Fatalf("completion = %#v, want /settings item", model.Input.Completion)
	}

	model.inputReducer().setCompletionItems(nil)
	if model.Input.Completion != nil {
		t.Fatalf("completion = %#v, want nil after empty completion set", model.Input.Completion)
	}

	model.PasteMarkers = map[string]pasteMarker{"[paste #1]": {content: "expanded"}}
	model.inputReducer().clearPasteMarkers()
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

func TestDirectShellExecution(t *testing.T) {
	model := readyModel(t)
	model.Input.Composer.SetValue("!echo 'hello ion'")

	submitted, cmd := model.submitComposer()
	if cmd == nil {
		t.Fatal("expected non-nil command for direct shell execution")
	}
	if submitted.Input.Composer.Value() != "" {
		t.Fatalf("composer draft = %q, want reset", submitted.Input.Composer.Value())
	}
	if len(submitted.Input.History) == 0 ||
		submitted.Input.History[len(submitted.Input.History)-1] != "!echo 'hello ion'" {
		t.Fatalf("history = %#v, want !echo in history", submitted.Input.History)
	}

	// Execute cmd
	res := cmd()
	msg, ok := res.(directShellResultMsg)
	if !ok {
		// sequenceCmds may return multiple messages, check type
		t.Fatalf("msg = %T, want directShellResultMsg", res)
	}
	if !strings.Contains(msg.content, "hello ion") {
		t.Fatalf("content = %q, want 'hello ion'", msg.content)
	}
}
