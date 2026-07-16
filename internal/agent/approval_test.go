package agent

import (
	"context"
	"encoding/json"
	"slices"
	"sync"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func TestApprovalBrokerTrustedAllowsWithoutRequest(t *testing.T) {
	emitted := make(chan session.Event, 1)
	broker := NewApprovalBroker(ApprovalTrusted, true, func(event session.Event) {
		emitted <- event
	})
	outcome := broker.Request(context.Background(), session.ApprovalRequest{ToolName: "write"})
	if outcome.decision != session.ApprovalAllow {
		t.Fatalf("decision = %q, want allow", outcome.decision)
	}
	select {
	case event := <-emitted:
		t.Fatalf("trusted request emitted %T", event)
	default:
	}
}

func TestApprovalBrokerResolvesExactlyOnce(t *testing.T) {
	events := make(chan session.Event, 2)
	broker := NewApprovalBroker(ApprovalConfirm, true, func(event session.Event) {
		events <- event
	})
	result := make(chan approvalOutcome, 1)
	go func() {
		result <- broker.Request(context.Background(), session.ApprovalRequest{
			ToolCallID: "call-1",
			ToolName:   "write",
			Operation:  "write",
			Resource:   "main.go",
		})
	}()

	event := <-events
	request, ok := event.(session.ApprovalRequest)
	if !ok {
		t.Fatalf("first event = %T, want ApprovalRequest", event)
	}
	if request.ID == "" {
		t.Fatal("approval request has no ID")
	}
	if err := broker.Resolve(request.ID, session.ApprovalAllow); err != nil {
		t.Fatalf("Resolve: %v", err)
	}
	if err := broker.Resolve(request.ID, session.ApprovalAllow); err == nil {
		t.Fatal("duplicate resolution unexpectedly succeeded")
	}
	select {
	case outcome := <-result:
		if outcome.decision != session.ApprovalAllow {
			t.Fatalf("decision = %q, want allow", outcome.decision)
		}
	case <-time.After(time.Second):
		t.Fatal("approval request did not resolve")
	}
	resolution, ok := (<-events).(session.ApprovalResolution)
	if !ok || resolution.ID != request.ID || resolution.Decision != session.ApprovalAllow {
		t.Fatalf("resolution = %#v, want request %q allow", resolution, request.ID)
	}
}

func TestApprovalBrokerCancellationDenies(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	events := make(chan session.Event, 2)
	broker := NewApprovalBroker(ApprovalConfirm, true, func(event session.Event) {
		events <- event
	})
	result := make(chan approvalOutcome, 1)
	go func() {
		result <- broker.Request(ctx, session.ApprovalRequest{ToolName: "bash"})
	}()
	request := (<-events).(session.ApprovalRequest)
	cancel()
	outcome := <-result
	if outcome.decision != session.ApprovalDeny {
		t.Fatalf("decision = %q, want deny", outcome.decision)
	}
	if outcome.reason == "" {
		t.Fatal("cancellation denial has no reason")
	}
	resolution := (<-events).(session.ApprovalResolution)
	if resolution.ID != request.ID || resolution.Decision != session.ApprovalDeny {
		t.Fatalf("resolution = %#v, want denied request %q", resolution, request.ID)
	}
}

func TestApprovalBrokerNonInteractiveConfirmDenies(t *testing.T) {
	broker := NewApprovalBroker(ApprovalConfirm, false, nil)
	outcome := broker.Request(context.Background(), session.ApprovalRequest{ToolName: "edit"})
	if outcome.decision != session.ApprovalDeny {
		t.Fatalf("decision = %q, want deny", outcome.decision)
	}
}

func TestHarnessApprovalGateWiresRequirementAndResolution(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	events := make(chan session.Event, 4)
	h := NewHarness(HarnessConfig{
		Events:              events,
		Session:             sess,
		Store:               store,
		ApprovalMode:        ApprovalConfirm,
		ApprovalInteractive: true,
		Model:               llm.Model{ID: "test"},
		Tools: []Tool{{
			Name: "write",
			ApprovalRequirement: func(json.RawMessage) (ApprovalRequirement, bool, error) {
				return ApprovalRequirement{Category: "write", Operation: "write", Resource: "main.go"}, true, nil
			},
		}},
	})
	defer h.Close()

	loopCfg := h.buildLoopConfig(context.Background(), h.buildTools(), nil)
	decisionCh := make(chan *ToolCallDecision, 1)
	go func() {
		decisionCh <- loopCfg.BeforeToolCall(ToolCallContext{
			RunContext: context.Background(),
			ToolCall:   &session.ToolCall{ID: "call-1", Name: "write"},
			Args:       json.RawMessage(`{"path":"main.go"}`),
		})
	}()

	event := <-events
	request, ok := event.(session.ApprovalRequest)
	if !ok {
		t.Fatalf("event = %T, want ApprovalRequest", event)
	}
	if err := h.ResolveApproval(request.ID, session.ApprovalDeny); err != nil {
		t.Fatalf("ResolveApproval: %v", err)
	}
	decision := <-decisionCh
	if decision == nil || !decision.Block || decision.Reason == "" {
		t.Fatalf("decision = %#v, want recoverable block", decision)
	}
}

func TestDeniedApprovalPersistsRecoverableToolResult(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	var streamMu sync.Mutex
	streamCalls := 0
	executed := false
	h := NewHarness(HarnessConfig{
		Session:             sess,
		Store:               store,
		ApprovalMode:        ApprovalConfirm,
		ApprovalInteractive: true,
		Model:               llm.Model{ID: "test"},
		Tools: []Tool{{
			Name: "write",
			ApprovalRequirement: func(json.RawMessage) (ApprovalRequirement, bool, error) {
				return ApprovalRequirement{Category: "write", Operation: "write", Resource: "main.go"}, true, nil
			},
			Execute: func(context.Context, string, json.RawMessage, <-chan struct{}, func(session.ToolPartial)) (session.ToolResultMessage, error) {
				executed = true
				return session.ToolResultMessage{Content: []session.Content{session.TextContent{Text: "executed"}}}, nil
			},
		}},
		StreamFn: func(context.Context, *llm.Request) (llm.Stream, error) {
			streamMu.Lock()
			streamCalls++
			call := streamCalls
			streamMu.Unlock()
			if call == 1 {
				return &mockStream{chunks: []*llm.Chunk{{Calls: []llm.Call{{
					ID: "call-1",
					Function: struct {
						Name      string `json:"name"`
						Arguments string `json:"arguments"`
					}{Name: "write", Arguments: `{"path":"main.go"}`},
				}}, StopReason: "toolUse"}}}, nil
			}
			return &mockStream{chunks: []*llm.Chunk{{Content: "denied safely", StopReason: "stop"}}}, nil
		},
	})
	defer h.Close()
	h.Subscribe(func(event session.Event) {
		if request, ok := event.(session.ApprovalRequest); ok {
			if err := h.ResolveApproval(request.ID, session.ApprovalDeny); err != nil {
				t.Errorf("ResolveApproval: %v", err)
			}
		}
	})

	if _, err := h.Prompt(context.Background(), "write the file"); err != nil {
		t.Fatalf("Prompt: %v", err)
	}
	if executed {
		t.Fatal("denied tool executed")
	}
	entries, err := sess.Entries(context.Background())
	if err != nil {
		t.Fatalf("Entries: %v", err)
	}
	var foundError bool
	for _, entry := range entries {
		messageEntry, ok := entry.(*session.MessageEntry)
		if !ok {
			continue
		}
		result, ok := messageEntry.Message.(*session.ToolResultMessage)
		if ok && result.ToolCallID == "call-1" {
			foundError = result.IsError
		}
	}
	if !foundError {
		t.Fatalf("denied tool result was not persisted as an error; entries=%d", len(entries))
	}
	if !slices.ContainsFunc(entries, func(entry session.Entry) bool {
		messageEntry, ok := entry.(*session.MessageEntry)
		if !ok {
			return false
		}
		_, ok = messageEntry.Message.(*session.AssistantMessage)
		return ok
	}) {
		t.Fatal("final assistant response was not persisted")
	}
}
