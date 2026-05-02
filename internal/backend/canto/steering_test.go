package canto

import (
	"context"
	"strings"
	"testing"
	"time"

	"github.com/go-json-experiment/json"
	"github.com/nijaru/canto/llm"
	csession "github.com/nijaru/canto/session"
	"github.com/nijaru/ion/internal/config"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func TestSteeringMutatorConsumesPendingAtProviderBoundary(t *testing.T) {
	mutator := newSteeringMutator()
	result, err := mutator.Submit(t.Context(), "s1", "use the smaller test")
	if err != nil {
		t.Fatalf("submit steering: %v", err)
	}
	if result.Outcome != ionsession.SteeringAccepted {
		t.Fatalf("steering outcome = %q, want accepted", result.Outcome)
	}

	sess := csession.New("s1")
	if err := mutator.Mutate(t.Context(), nil, "", sess); err != nil {
		t.Fatalf("mutate steering: %v", err)
	}
	if err := mutator.Mutate(t.Context(), nil, "", sess); err != nil {
		t.Fatalf("second mutate steering: %v", err)
	}

	events, err := steeringEvents(sess.Events())
	if err != nil {
		t.Fatalf("decode steering events: %v", err)
	}
	if len(events) != 2 {
		t.Fatalf("steering events = %d, want pending+consumed", len(events))
	}
	if events[0].Status != "pending" || events[0].Input != "use the smaller test" {
		t.Fatalf("pending event = %#v", events[0])
	}
	if events[1].Status != "consumed" || events[1].PendingEventID == "" {
		t.Fatalf("consumed event = %#v", events[1])
	}

	entries, err := sess.EffectiveEntries()
	if err != nil {
		t.Fatalf("effective entries: %v", err)
	}
	if len(entries) != 1 || entries[0].EventType != csession.ContextAdded {
		t.Fatalf("entries = %#v, want one steering context", entries)
	}
	if !strings.Contains(entries[0].Message.Content, "use the smaller test") {
		t.Fatalf("steering context = %q", entries[0].Message.Content)
	}
}

func TestSteeringMutatorKeepsOtherSessionsPending(t *testing.T) {
	mutator := newSteeringMutator()
	if _, err := mutator.Submit(t.Context(), "s1", "first"); err != nil {
		t.Fatalf("submit s1: %v", err)
	}
	if _, err := mutator.Submit(t.Context(), "s2", "second"); err != nil {
		t.Fatalf("submit s2: %v", err)
	}

	sess := csession.New("s1")
	if err := mutator.Mutate(t.Context(), nil, "", sess); err != nil {
		t.Fatalf("mutate s1: %v", err)
	}

	other := csession.New("s2")
	if err := mutator.Mutate(t.Context(), nil, "", other); err != nil {
		t.Fatalf("mutate s2: %v", err)
	}
	entries, err := other.EffectiveEntries()
	if err != nil {
		t.Fatalf("effective entries: %v", err)
	}
	if len(entries) != 1 || !strings.Contains(entries[0].Message.Content, "second") {
		t.Fatalf("other entries = %#v, want deferred second steering", entries)
	}
}

func TestBackendSteerTurnQueuesWithoutActiveTurn(t *testing.T) {
	backend := &Backend{steering: newSteeringMutator()}
	result, err := backend.SteerTurn(t.Context(), "later")
	if err != nil {
		t.Fatalf("steer turn: %v", err)
	}
	if result.Outcome != ionsession.SteeringQueued {
		t.Fatalf("outcome = %q, want queued", result.Outcome)
	}
}

func TestBackendSteerTurnQueuesWithoutActiveTool(t *testing.T) {
	backend := &Backend{
		steering:   newSteeringMutator(),
		turnActive: true,
	}
	result, err := backend.SteerTurn(t.Context(), "later")
	if err != nil {
		t.Fatalf("steer turn: %v", err)
	}
	if result.Outcome != ionsession.SteeringQueued {
		t.Fatalf("outcome = %q, want queued", result.Outcome)
	}
}

func TestBackendSteerTurnAcceptsDuringActiveTurn(t *testing.T) {
	backend := &Backend{
		steering:      newSteeringMutator(),
		turnActive:    true,
		activeToolIDs: map[string]struct{}{"tool-call-1": {}},
	}
	result, err := backend.SteerTurn(t.Context(), "use the test output")
	if err != nil {
		t.Fatalf("steer turn: %v", err)
	}
	if result.Outcome != ionsession.SteeringAccepted {
		t.Fatalf("outcome = %q, want accepted", result.Outcome)
	}

	sess := csession.New("default")
	if err := backend.steering.Mutate(t.Context(), nil, "", sess); err != nil {
		t.Fatalf("mutate steering: %v", err)
	}
	entries, err := sess.EffectiveEntries()
	if err != nil {
		t.Fatalf("effective entries: %v", err)
	}
	if len(entries) != 1 || !strings.Contains(entries[0].Message.Content, "use the test output") {
		t.Fatalf("entries = %#v, want accepted steering context", entries)
	}
}

func TestBackendSteeringAppearsInNextProviderRequestAfterTool(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	store, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new canto store: %v", err)
	}

	cwd := t.TempDir()
	storageSession, err := store.OpenSession(ctx, cwd, "local-api/model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}

	call := llm.Call{ID: "tool-call-steer", Type: "function"}
	call.Function.Name = "bash"
	call.Function.Arguments = `{"command":"sleep 0.3; echo steering-ready"}`
	provider := llm.NewFauxProvider(
		"local-api",
		llm.FauxStep{Calls: []llm.Call{call}},
		llm.FauxStep{Content: "done"},
	)

	oldFactory := providerFactory
	providerFactory = func(ctx context.Context, cfg *config.Config) (llm.Provider, error) {
		if cfg.Provider == "local-api" {
			return provider, nil
		}
		return oldFactory(ctx, cfg)
	}
	defer func() { providerFactory = oldFactory }()

	b := New()
	b.SetStore(store)
	b.SetSession(storageSession)
	b.SetConfig(&config.Config{
		Provider: "local-api",
		Model:    "model-a",
		Endpoint: "http://localhost:8080/v1",
	})
	b.SetMode(ionsession.ModeYolo)
	if err := b.Open(ctx); err != nil {
		t.Fatalf("open backend: %v", err)
	}
	defer func() { _ = b.Close() }()

	if err := b.SubmitTurn(ctx, "run the slow command"); err != nil {
		t.Fatalf("submit turn: %v", err)
	}
	waitForToolStarted(t, b.Events(), "bash")

	result, err := b.SteerTurn(ctx, "use the smaller test")
	if err != nil {
		t.Fatalf("steer turn: %v", err)
	}
	if result.Outcome != ionsession.SteeringAccepted {
		t.Fatalf("steering outcome = %q, want accepted", result.Outcome)
	}

	waitForTurnFinished(t, b.Events())

	calls := provider.Calls()
	if len(calls) != 2 {
		t.Fatalf("provider calls = %d, want 2", len(calls))
	}
	if requestHasMessage(calls[0].Messages, llm.RoleUser, "use the smaller test") {
		t.Fatalf("first provider request unexpectedly contains steering: %#v", calls[0].Messages)
	}
	if !requestHasMessage(calls[1].Messages, llm.RoleUser, "use the smaller test") {
		t.Fatalf("second provider request missing steering context: %#v", calls[1].Messages)
	}
}

func waitForToolStarted(t *testing.T, events <-chan ionsession.Event, toolName string) {
	t.Helper()

	timeout := time.After(2 * time.Second)
	for {
		select {
		case ev, ok := <-events:
			if !ok {
				t.Fatal("event stream closed before tool start")
			}
			switch msg := ev.(type) {
			case ionsession.Error:
				t.Fatalf("unexpected session error: %v", msg.Err)
			case ionsession.ToolCallStarted:
				if msg.ToolName == toolName {
					return
				}
			}
		case <-timeout:
			t.Fatalf("timed out waiting for %s tool start", toolName)
		}
	}
}

func steeringEvents(events []csession.Event) ([]steeringEvent, error) {
	out := make([]steeringEvent, 0)
	for _, event := range events {
		if event.Type != csession.ExternalInput {
			continue
		}
		var data steeringEvent
		if err := json.Unmarshal(event.Data, &data); err != nil {
			return nil, err
		}
		if data.Kind == steeringKind {
			out = append(out, data)
		}
	}
	return out, nil
}
