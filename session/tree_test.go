package session

import (
	"path/filepath"
	"strings"
	"testing"

	"github.com/nijaru/ion/llm"
)

func TestSessionActiveBranchDrivesEffectiveMessages(t *testing.T) {
	sess := New("tree-effective")

	if err := sess.AppendUser(t.Context(), "root"); err != nil {
		t.Fatalf("append root: %v", err)
	}
	root, ok := sess.LastEvent()
	if !ok {
		t.Fatal("missing root event")
	}
	if err := sess.AppendUser(t.Context(), "main"); err != nil {
		t.Fatalf("append main: %v", err)
	}
	main, ok := sess.LastEvent()
	if !ok {
		t.Fatal("missing main event")
	}
	if main.ParentID != root.ID.String() {
		t.Fatalf("main parent = %q, want %q", main.ParentID, root.ID)
	}

	if err := sess.MoveLeaf(t.Context(), root.ID.String()); err != nil {
		t.Fatalf("move leaf: %v", err)
	}
	if err := sess.AppendUser(t.Context(), "branch"); err != nil {
		t.Fatalf("append branch: %v", err)
	}

	messages, err := sess.EffectiveMessages()
	if err != nil {
		t.Fatalf("EffectiveMessages: %v", err)
	}
	got := messageContents(messages)
	want := []string{"root", "branch"}
	if !sameStrings(got, want) {
		t.Fatalf("effective messages = %#v, want %#v", got, want)
	}
	active, err := sess.ActiveEvents()
	if err != nil {
		t.Fatalf("ActiveEvents: %v", err)
	}
	if got := eventContents(t, active); !sameStrings(got, want) {
		t.Fatalf("active events = %#v, want %#v", got, want)
	}
}

func TestSessionLeafMovementPersistsAndReplays(t *testing.T) {
	store, err := NewJSONLStore(filepath.Join(t.TempDir(), "sessions"))
	if err != nil {
		t.Fatalf("NewJSONLStore: %v", err)
	}
	t.Cleanup(func() {
		if err := store.Close(); err != nil {
			t.Fatalf("close store: %v", err)
		}
	})

	sess := New("tree-replay").WithWriter(store)
	if err := sess.AppendUser(t.Context(), "root"); err != nil {
		t.Fatalf("append root: %v", err)
	}
	root, ok := sess.LastEvent()
	if !ok {
		t.Fatal("missing root event")
	}
	if err := sess.AppendUser(t.Context(), "main"); err != nil {
		t.Fatalf("append main: %v", err)
	}
	if err := sess.MoveLeaf(t.Context(), root.ID.String()); err != nil {
		t.Fatalf("move leaf: %v", err)
	}
	if err := sess.AppendUser(t.Context(), "branch"); err != nil {
		t.Fatalf("append branch: %v", err)
	}

	reloaded, err := store.Load(t.Context(), sess.ID())
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	messages, err := reloaded.EffectiveMessages()
	if err != nil {
		t.Fatalf("EffectiveMessages: %v", err)
	}
	got := messageContents(messages)
	want := []string{"root", "branch"}
	if !sameStrings(got, want) {
		t.Fatalf("reloaded effective messages = %#v, want %#v", got, want)
	}
	if reloaded.LeafID() == root.ID.String() {
		t.Fatalf("leaf id remained at root after branch append")
	}
}

func TestSessionMoveLeafWithSummaryAppendsBranchSummary(t *testing.T) {
	store, err := NewJSONLStore(filepath.Join(t.TempDir(), "sessions"))
	if err != nil {
		t.Fatalf("NewJSONLStore: %v", err)
	}
	t.Cleanup(func() {
		if err := store.Close(); err != nil {
			t.Fatalf("close store: %v", err)
		}
	})

	sess := New("tree-summary").WithWriter(store)
	if err := sess.AppendUser(t.Context(), "root"); err != nil {
		t.Fatalf("append root: %v", err)
	}
	root, ok := sess.LastEvent()
	if !ok {
		t.Fatal("missing root event")
	}
	if err := sess.AppendUser(t.Context(), "main"); err != nil {
		t.Fatalf("append main: %v", err)
	}
	if err := sess.MoveLeafWithSummary(t.Context(), root.ID.String(), BranchSummaryData{
		Summary: "explored main and came back",
		Details: map[string]any{
			"read_files": []string{"main.go"},
		},
	}); err != nil {
		t.Fatalf("MoveLeafWithSummary: %v", err)
	}

	active, err := sess.ActiveEvents()
	if err != nil {
		t.Fatalf("ActiveEvents: %v", err)
	}
	if len(active) != 2 || active[0].ID != root.ID || active[1].Type != BranchSummary {
		t.Fatalf("active events = %#v, want root plus branch summary", active)
	}
	summary, ok, err := active[1].BranchSummaryData()
	if err != nil || !ok {
		t.Fatalf("BranchSummaryData ok=%v err=%v", ok, err)
	}
	if summary.FromEventID != root.ID.String() ||
		summary.Summary != "explored main and came back" {
		t.Fatalf("branch summary = %#v, want root source and summary", summary)
	}

	reloaded, err := store.Load(t.Context(), sess.ID())
	if err != nil {
		t.Fatalf("Load: %v", err)
	}
	messages, err := reloaded.EffectiveMessages()
	if err != nil {
		t.Fatalf("EffectiveMessages: %v", err)
	}
	if got := messageContents(messages); len(got) != 2 ||
		got[0] != "root" ||
		!strings.Contains(got[1], "explored main and came back") {
		t.Fatalf("effective messages = %#v, want root and branch summary", got)
	}
}

func TestSessionMoveLeafRejectsMissingTarget(t *testing.T) {
	sess := New("tree-missing")
	if err := sess.AppendUser(t.Context(), "root"); err != nil {
		t.Fatalf("append root: %v", err)
	}
	if err := sess.MoveLeaf(t.Context(), "missing"); err == nil {
		t.Fatal("MoveLeaf missing target succeeded")
	}
}

func TestSessionRejectsEmptyBranchSummary(t *testing.T) {
	sess := New("tree-empty-summary")
	if err := sess.AppendBranchSummary(t.Context(), BranchSummaryData{}); err == nil {
		t.Fatal("AppendBranchSummary empty summary succeeded")
	}
	if err := sess.Append(t.Context(), NewBranchSummaryEvent(sess.ID(), BranchSummaryData{})); err == nil {
		t.Fatal("Append raw empty branch summary succeeded")
	}
}

func TestSessionLegacyEventsReplayAsLinearBranch(t *testing.T) {
	replayer := NewReplayer()
	events := []Event{
		NewUserMessage("legacy-tree", "one"),
		NewUserMessage("legacy-tree", "two"),
		NewUserMessage("legacy-tree", "three"),
	}
	sess := replayer.NewSession("legacy-tree")
	for _, event := range events {
		if event.ParentID != "" {
			t.Fatalf("test event unexpectedly has parent %q", event.ParentID)
		}
		if err := replayer.Apply(sess, event); err != nil {
			t.Fatalf("replay: %v", err)
		}
	}
	messages, err := sess.EffectiveMessages()
	if err != nil {
		t.Fatalf("EffectiveMessages: %v", err)
	}
	got := messageContents(messages)
	want := []string{"one", "two", "three"}
	if !sameStrings(got, want) {
		t.Fatalf("legacy effective messages = %#v, want %#v", got, want)
	}
}

func TestSessionEffectiveSettingsFollowActiveBranch(t *testing.T) {
	sess := New("settings-branch")
	if err := sess.AppendModelSelection(t.Context(), ModelSelection{
		ProviderID: "faux",
		Model:      "model-a",
	}); err != nil {
		t.Fatalf("append model-a: %v", err)
	}
	root, ok := sess.LastEvent()
	if !ok {
		t.Fatal("missing root model event")
	}
	if err := sess.AppendThinkingSelection(t.Context(), ThinkingSelection{Level: "low"}); err != nil {
		t.Fatalf("append low thinking: %v", err)
	}
	if err := sess.AppendModelSelection(t.Context(), ModelSelection{
		ProviderID: "faux",
		Model:      "main-model",
	}); err != nil {
		t.Fatalf("append main model: %v", err)
	}

	if err := sess.MoveLeaf(t.Context(), root.ID.String()); err != nil {
		t.Fatalf("move leaf: %v", err)
	}
	if err := sess.AppendThinkingSelection(t.Context(), ThinkingSelection{Level: "high"}); err != nil {
		t.Fatalf("append branch thinking: %v", err)
	}
	if err := sess.AppendModelSelection(t.Context(), ModelSelection{
		ProviderID: "faux",
		Model:      "branch-model",
	}); err != nil {
		t.Fatalf("append branch model: %v", err)
	}

	settings, err := sess.EffectiveSettings()
	if err != nil {
		t.Fatalf("EffectiveSettings: %v", err)
	}
	if !settings.HasModel || settings.Model.Model != "branch-model" ||
		settings.ThinkingLevel != "high" {
		t.Fatalf("settings = %#v, want branch-model/high", settings)
	}
}

func messageContents(messages []llm.Message) []string {
	out := make([]string, 0, len(messages))
	for _, msg := range messages {
		out = append(out, msg.Content)
	}
	return out
}

func eventContents(t *testing.T, events []Event) []string {
	t.Helper()
	out := make([]string, 0, len(events))
	for i := range events {
		if events[i].Type != MessageAdded {
			continue
		}
		msg, err := events[i].ensureMessage()
		if err != nil {
			t.Fatalf("decode message %s: %v", events[i].ID, err)
		}
		out = append(out, msg.Content)
	}
	return out
}

func sameStrings(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}
