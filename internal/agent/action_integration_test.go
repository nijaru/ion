package agent

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"sync"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func TestHarnessRoutesEffectThroughDurableActionBoundary(t *testing.T) {
	ctx := t.Context()
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	workdir := t.TempDir()

	var (
		mu              sync.Mutex
		approvedAction  string
		effectObserved  bool
		executionRecord session.ActionRecord
	)
	mutatingTool := Tool{
		Name:           "write_probe",
		Description:    "test mutation",
		RequiresAction: true,
		Parameters:     `{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}`,
		ApprovalRequirement: func(json.RawMessage) (ApprovalRequirement, bool, error) {
			return ApprovalRequirement{
				Category:  "write",
				Operation: "write",
				Resource:  "probe.txt",
				Paths:     []string{"probe.txt"},
				Metadata:  map[string]any{"source": "integration"},
			}, true, nil
		},
		Execute: func(ctx context.Context, id string, _ json.RawMessage, _ <-chan struct{}, _ func(session.ToolPartial)) (session.ToolResultMessage, error) {
			mu.Lock()
			actionID := approvedAction
			mu.Unlock()
			if actionID == "" {
				return session.ToolResultMessage{ToolCallID: id, ToolName: "write_probe", IsError: true}, errors.New("effect ran before approval identity was visible")
			}
			record, err := store.GetAction(ctx, actionID)
			if err != nil {
				return session.ToolResultMessage{}, err
			}
			if record.State != session.ActionStarted {
				return session.ToolResultMessage{}, fmt.Errorf("effect crossed boundary in state %s", record.State)
			}
			mu.Lock()
			effectObserved = true
			executionRecord = record
			mu.Unlock()
			return session.ToolResultMessage{
				ToolCallID: id,
				ToolName:   "write_probe",
				Content:    []session.Content{session.TextContent{Text: "ok"}},
				Terminate:  true,
			}, nil
		},
	}

	h := NewController(ControllerConfig{
		Session:             sess,
		Store:               store,
		Durable:             store,
		ActionJournal:       store,
		Workdir:             workdir,
		Model:               llm.Model{ID: "test"},
		Tools:               []Tool{mutatingTool},
		ApprovalMode:        ApprovalConfirm,
		ApprovalInteractive: true,
		StreamFn: func(_ context.Context, _ *llm.Request) (llm.Stream, error) {
			return &mockStream{chunks: []*llm.Chunk{{
				Calls: []llm.Call{{
					ID:   "write-call",
					Type: "function",
					Function: struct {
						Name      string `json:"name"`
						Arguments string `json:"arguments"`
					}{Name: "write_probe", Arguments: `{"path":"probe.txt"}`},
				}},
				StopReason: "stop",
			}}}, nil
		},
	})
	defer h.Close()

	sub, err := h.Subscribe(ctx, EventCursor{})
	if err != nil {
		t.Fatal(err)
	}
	defer sub.Close()

	type promptResult struct {
		message session.Message
		err     error
	}
	resultCh := make(chan promptResult, 1)
	go func() {
		message, err := h.Prompt(ctx, "write a probe")
		resultCh <- promptResult{message: message, err: err}
	}()

	var request session.ApprovalRequest
	deadline := time.NewTimer(2 * time.Second)
	defer deadline.Stop()
	for request.ID == "" {
		select {
		case envelope := <-sub.Events:
			if candidate, ok := envelope.Event.(session.ApprovalRequest); ok {
				request = candidate
				mu.Lock()
				approvedAction = request.ActionID
				mu.Unlock()
				if request.ActionID == "" || request.Fingerprint == "" {
					t.Fatalf("approval request lacks action identity: %#v", request)
				}
				if err := h.ResolveApproval(request.ID, session.ApprovalAllow); err != nil {
					t.Fatalf("resolve approval: %v", err)
				}
			}
		case <-deadline.C:
			t.Fatal("timed out waiting for durable action approval")
		}
	}

	select {
	case result := <-resultCh:
		if result.err != nil {
			t.Fatalf("Prompt: %v", result.err)
		}
		if result.message == nil {
			t.Fatal("Prompt returned no assistant message")
		}
	case <-time.After(2 * time.Second):
		t.Fatal("timed out waiting for Prompt")
	}

	mu.Lock()
	observed, record := effectObserved, executionRecord
	mu.Unlock()
	if !observed || record.State != session.ActionStarted {
		t.Fatalf("effect observation = %v, record = %#v; want started action", observed, record)
	}
	final, err := store.GetAction(ctx, request.ActionID)
	if err != nil {
		t.Fatal(err)
	}
	if final.State != session.ActionCompleted {
		t.Fatalf("final action state = %s, want completed", final.State)
	}
	if string(final.Metadata) != `{"source":"integration"}` {
		t.Fatalf("final metadata = %s", final.Metadata)
	}
	transitions, err := store.ActionTransitions(ctx, request.ActionID)
	if err != nil {
		t.Fatal(err)
	}
	want := []session.ActionState{session.ActionPrepared, session.ActionAuthorized, session.ActionStarted, session.ActionCompleted}
	if len(transitions) != len(want) {
		t.Fatalf("transitions = %#v, want %d transitions", transitions, len(want))
	}
	for i, state := range want {
		if transitions[i].To != state {
			t.Fatalf("transition %d = %#v, want %s", i, transitions[i], state)
		}
	}
}
