package agent

import (
	"context"
	"strings"
	"testing"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func TestIdleModelSelectionPersistsWhenRuntimeAlreadyUsesTarget(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	if _, err := sess.AppendModelChange(context.Background(), "old", "old-model"); err != nil {
		t.Fatal(err)
	}
	h := NewController(ControllerConfig{
		Session: sess,
		Store:   store,
		Model:   llm.Model{Provider: "new", ID: "new-model"},
	})
	defer h.Close()

	if err := h.SetModel(llm.Model{Provider: "new", ID: "new-model"}); err != nil {
		t.Fatalf("SetModel: %v", err)
	}
	snapshot, err := sess.BuildContext(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if snapshot.ActiveProvider != "new" || snapshot.ActiveModel != "new-model" {
		t.Fatalf("persisted model = %s/%s, want new/new-model", snapshot.ActiveProvider, snapshot.ActiveModel)
	}
}

func TestIdleModelAndToolChangesPersistBeforeLiveMutation(t *testing.T) {
	store := newTestStore(t)
	base := session.NewSession(store, 64)
	failing := &failingPersistenceSession{Session: base}
	h := NewController(ControllerConfig{
		Session: failing,
		Store:   store,
		Model:   llm.Model{Provider: "old", ID: "old-model"},
	})
	defer h.Close()

	failing.failModel.Store(true)
	err := h.SetModel(llm.Model{Provider: "new", ID: "new-model"})
	if err == nil || !strings.Contains(err.Error(), "persist model change") {
		t.Fatalf("SetModel error = %v, want persistence failure", err)
	}
	h.mu.Lock()
	model := h.model
	h.mu.Unlock()
	if model.ID != "old-model" || model.Provider != "old" {
		t.Fatalf("live model = %#v, want old model after failed persistence", model)
	}

	failing.failModel.Store(false)
	if err := h.SetModel(llm.Model{Provider: "new", ID: "new-model"}); err != nil {
		t.Fatalf("SetModel success: %v", err)
	}
	failing.failTools.Store(true)
	err = h.SetTools([]Tool{{Name: "read"}}, []string{"read"})
	if err == nil || !strings.Contains(err.Error(), "persist active tools") {
		t.Fatalf("SetTools error = %v, want persistence failure", err)
	}
	h.mu.Lock()
	active := append([]string(nil), h.active...)
	h.mu.Unlock()
	if len(active) != 0 {
		t.Fatalf("live active tools = %#v, want unchanged after failed persistence", active)
	}

	failing.failTools.Store(false)
	if err := h.SetTools([]Tool{{Name: "read"}}, []string{"read"}); err != nil {
		t.Fatalf("SetTools success: %v", err)
	}
	snapshot, err := failing.BuildContext(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if snapshot.ActiveModel != "new-model" {
		t.Fatalf("persisted model = %q, want new-model", snapshot.ActiveModel)
	}
	if len(snapshot.ActiveTools) != 1 || snapshot.ActiveTools[0] != "read" {
		t.Fatalf("persisted active tools = %#v, want read", snapshot.ActiveTools)
	}
}
