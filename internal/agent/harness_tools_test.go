package agent

import (
	"context"
	"slices"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func TestHarnessActivateToolsAddsDeferredToolForNextTurn(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	if _, err := sess.AppendActiveToolsChange(context.Background(), []string{"search_tools"}); err != nil {
		t.Fatal(err)
	}
	var requested []string
	h := NewHarness(HarnessConfig{
		Session: sess,
		Store:   store,
		Model:   llm.Model{ID: "test"},
		Tools: []Tool{
			{Name: "search_tools"},
			{Name: "deferred"},
		},
		Active: []string{"search_tools"},
		StreamFn: func(_ context.Context, req *llm.Request) (llm.Stream, error) {
			requested = make([]string, len(req.Tools))
			for i, tool := range req.Tools {
				requested[i] = tool.Name
			}
			return &mockStream{chunks: []*llm.Chunk{{Content: "ok", StopReason: "stop"}}}, nil
		},
	})
	defer h.Close()

	events := make(chan session.Event, 1)
	unsubscribe := h.Subscribe(func(event session.Event) {
		if _, ok := event.(session.ToolsUpdate); ok {
			events <- event
		}
	})
	defer unsubscribe()

	if err := h.ActivateTools(context.Background(), []string{"missing"}); err == nil {
		t.Fatal("expected unknown tool activation to fail")
	}
	if got := toolNames(h.buildTools()); !slices.Equal(got, []string{"search_tools"}) {
		t.Fatalf("tools after rejected activation = %#v", got)
	}
	if err := h.ActivateTools(context.Background(), []string{"deferred"}); err != nil {
		t.Fatal(err)
	}
	if err := h.ActivateTools(context.Background(), []string{"deferred"}); err != nil {
		t.Fatal(err)
	}
	if got := toolNames(h.buildTools()); !slices.Equal(got, []string{"search_tools", "deferred"}) {
		t.Fatalf("active tools = %#v", got)
	}

	select {
	case event := <-events:
		update := event.(session.ToolsUpdate)
		if !slices.Equal(update.Previous, []string{"search_tools"}) ||
			!slices.Equal(update.Active, []string{"search_tools", "deferred"}) {
			t.Fatalf("tools update = %#v", update)
		}
	case <-time.After(time.Second):
		t.Fatal("timed out waiting for ToolsUpdate")
	}

	if _, err := h.Prompt(context.Background(), "use deferred"); err != nil {
		t.Fatal(err)
	}
	if !slices.Equal(requested, []string{"search_tools", "deferred"}) {
		t.Fatalf("provider tools = %#v", requested)
	}
	snap, err := sess.BuildContext(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if !slices.Equal(snap.ActiveTools, []string{"search_tools", "deferred"}) {
		t.Fatalf("persisted active tools = %#v", snap.ActiveTools)
	}
}

func TestHarnessActivateToolsDuringRunPersistsForNextTurn(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	if _, err := sess.AppendActiveToolsChange(context.Background(), []string{"search_tools"}); err != nil {
		t.Fatal(err)
	}
	release := make(chan struct{})
	started := make(chan struct{})
	var requests [][]string
	h := NewHarness(HarnessConfig{
		Session: sess,
		Store:   store,
		Model:   llm.Model{ID: "test"},
		Tools: []Tool{
			{Name: "search_tools"},
			{Name: "deferred"},
		},
		Active: []string{"search_tools"},
		StreamFn: func(_ context.Context, req *llm.Request) (llm.Stream, error) {
			requests = append(requests, toolNamesFromSpecs(req.Tools))
			if len(requests) == 1 {
				close(started)
				<-release
			}
			return &mockStream{chunks: []*llm.Chunk{{Content: "ok", StopReason: "stop"}}}, nil
		},
	})
	defer h.Close()

	done := make(chan error, 1)
	go func() {
		_, err := h.Prompt(context.Background(), "first")
		done <- err
	}()
	<-started
	if err := h.ActivateTools(context.Background(), []string{"deferred"}); err != nil {
		t.Fatal(err)
	}
	close(release)
	if err := <-done; err != nil {
		t.Fatal(err)
	}
	if _, err := h.Prompt(context.Background(), "second"); err != nil {
		t.Fatal(err)
	}
	if len(requests) != 2 || !slices.Equal(requests[0], []string{"search_tools"}) ||
		!slices.Equal(requests[1], []string{"search_tools", "deferred"}) {
		t.Fatalf("provider tool snapshots = %#v", requests)
	}
}

func toolNamesFromSpecs(specs []*llm.Spec) []string {
	names := make([]string, len(specs))
	for i, spec := range specs {
		names[i] = spec.Name
	}
	return names
}

func toolNames(tools []Tool) []string {
	names := make([]string, len(tools))
	for i, tool := range tools {
		names[i] = tool.Name
	}
	return names
}
