package agent

import (
	"context"
	"encoding/json"
	"errors"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
	"github.com/nijaru/ion/tool"
)

func TestControllerRunSubagentSuccessIsolatedAndBounded(t *testing.T) {
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	var sessionID string
	var maxTokens int
	var toolNames []string
	var progress []string

	h := NewController(ControllerConfig{
		Session: sess,
		Store:   store,
		Model:   llm.Model{ID: "parent"},
		Tools: []Tool{
			{Name: "marker", Description: "visible to the child"},
			{Name: tool.SubagentToolName, Description: "must not reach the child"},
		},
		Child: ChildRunConfig{
			MaxConcurrent:     2,
			MaxToolIterations: 3,
			MaxTokens:         1024,
			MaxOutputChars:    100,
			MaxDuration:       time.Second,
		},
		StreamFn: func(_ context.Context, req *llm.Request) (llm.Stream, error) {
			sessionID = req.SessionID
			maxTokens = req.MaxTokens
			for _, spec := range req.Tools {
				toolNames = append(toolNames, spec.Name)
			}
			return &mockStream{chunks: []*llm.Chunk{{Content: "child result", StopReason: "stop"}}}, nil
		},
	})
	t.Cleanup(func() { _ = h.Close() })

	result, err := h.RunSubagent(context.Background(), tool.SubagentRequest{
		Task:              "inspect the marker",
		MaxToolIterations: 2,
		Progress:          func(text string) { progress = append(progress, text) },
	})
	if err != nil {
		t.Fatalf("RunSubagent() error = %v", err)
	}
	if result.Status != "completed" || result.Output != "child result" {
		t.Fatalf("result = %#v, want completed child result", result)
	}
	if result.ChildID == "" || result.StartedAt == "" || result.EndedAt == "" {
		t.Fatalf("result identity/timestamps = %#v", result)
	}
	if result.Budget.MaxToolIterations != 2 || result.Budget.MaxTokens != 1024 {
		t.Fatalf("child budget = %#v", result.Budget)
	}
	if sessionID == "" || sessionID == sess.Meta().ID {
		t.Fatalf("child provider session ID = %q, parent = %q", sessionID, sess.Meta().ID)
	}
	if maxTokens != 1024 {
		t.Fatalf("child max tokens = %d, want 1024", maxTokens)
	}
	if len(toolNames) != 1 || toolNames[0] != "marker" {
		t.Fatalf("child tools = %#v, want only marker", toolNames)
	}
	foundProgress := false
	for _, text := range progress {
		if strings.Contains(text, "child result") {
			foundProgress = true
			break
		}
	}
	if !foundProgress {
		t.Fatalf("child progress = %#v, want streamed child result", progress)
	}
}

func TestControllerRunSubagentCancellationStopsProvider(t *testing.T) {
	h := newChildTestController(
		t,
		ChildRunConfig{MaxDuration: time.Second},
		func(ctx context.Context, _ *llm.Request) (llm.Stream, error) {
			<-ctx.Done()
			return nil, ctx.Err()
		},
	)

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Millisecond)
	defer cancel()
	started := time.Now()
	result, err := h.RunSubagent(ctx, tool.SubagentRequest{Task: "cancel me"})
	if err == nil || !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("RunSubagent() error = %v, want deadline exceeded", err)
	}
	if result.Status != "canceled" {
		t.Fatalf("result status = %q, want canceled", result.Status)
	}
	if elapsed := time.Since(started); elapsed > time.Second {
		t.Fatalf("canceled child took %s", elapsed)
	}
}

func TestControllerRunSubagentBoundsConcurrency(t *testing.T) {
	started := make(chan struct{}, 1)
	release := make(chan struct{})
	h := newChildTestController(
		t,
		ChildRunConfig{MaxConcurrent: 1},
		func(ctx context.Context, _ *llm.Request) (llm.Stream, error) {
			select {
			case started <- struct{}{}:
			case <-ctx.Done():
				return nil, ctx.Err()
			}
			select {
			case <-release:
				return &mockStream{chunks: []*llm.Chunk{{Content: "released", StopReason: "stop"}}}, nil
			case <-ctx.Done():
				return nil, ctx.Err()
			}
		},
	)

	firstResult := make(chan error, 1)
	go func() {
		_, err := h.RunSubagent(context.Background(), tool.SubagentRequest{Task: "first"})
		firstResult <- err
	}()
	select {
	case <-started:
	case <-time.After(time.Second):
		t.Fatal("first child did not start")
	}

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Millisecond)
	defer cancel()
	second, err := h.RunSubagent(ctx, tool.SubagentRequest{Task: "second"})
	if err == nil || !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("second RunSubagent() error = %v, want deadline exceeded", err)
	}
	if second.Status != "canceled" {
		t.Fatalf("second result status = %q, want canceled", second.Status)
	}

	close(release)
	select {
	case err := <-firstResult:
		if err != nil {
			t.Fatalf("first RunSubagent() error = %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("first child did not finish after release")
	}
}

func TestControllerRunSubagentDeniesChildExternalEffectsWithoutApproval(t *testing.T) {
	store := newTestStore(t)
	executed := atomic.Bool{}
	callCount := atomic.Int32{}
	var effectCall llm.Call
	effectCall.ID = "effect-1"
	effectCall.Type = "function"
	effectCall.Function.Name = "effect"
	effectCall.Function.Arguments = `{}`

	effect := Tool{
		Name:           "effect",
		RequiresAction: true,
		ApprovalRequirement: func(json.RawMessage) (ApprovalRequirement, bool, error) {
			return ApprovalRequirement{Category: "write", Operation: "effect", Resource: "test"}, true, nil
		},
		Execute: func(context.Context, string, json.RawMessage, <-chan struct{}, func(session.ToolPartial)) (session.ToolResultMessage, error) {
			executed.Store(true)
			return session.ToolResultMessage{}, nil
		},
	}
	h := NewController(ControllerConfig{
		Session:             session.NewSession(store, 64),
		Store:               store,
		ActionJournal:       store,
		ApprovalMode:        ApprovalConfirm,
		ApprovalInteractive: false,
		Workdir:             t.TempDir(),
		Tools:               []Tool{effect},
		StreamFn: func(_ context.Context, _ *llm.Request) (llm.Stream, error) {
			if callCount.Add(1) == 1 {
				return &mockStream{chunks: []*llm.Chunk{{Calls: []llm.Call{effectCall}, StopReason: "tool_use"}}}, nil
			}
			return &mockStream{chunks: []*llm.Chunk{{Content: "safe completion", StopReason: "stop"}}}, nil
		},
	})
	t.Cleanup(func() { _ = h.Close() })

	result, err := h.RunSubagent(context.Background(), tool.SubagentRequest{Task: "try the effect"})
	if err != nil {
		t.Fatalf("RunSubagent() error = %v", err)
	}
	if result.Status != "completed" || result.Output != "safe completion" {
		t.Fatalf("result = %#v, want safe completion", result)
	}
	if executed.Load() {
		t.Fatal("child executed an external-effect tool without interactive approval")
	}
}

func TestControllerSubagentToolPersistsChildOutcomeInParentTurn(t *testing.T) {
	path := filepath.Join(t.TempDir(), "parent.db")
	store, err := session.NewSQLiteStore(path, "parent")
	if err != nil {
		t.Fatal(err)
	}
	sess := session.NewSession(store, 64)
	subagentTool := tool.NewSubagentTool()
	callCount := atomic.Int32{}

	var subagentCall llm.Call
	subagentCall.ID = "subagent-1"
	subagentCall.Type = "function"
	subagentCall.Function.Name = tool.SubagentToolName
	subagentCall.Function.Arguments = `{"task":"return evidence"}`

	stream := func(_ context.Context, _ *llm.Request) (llm.Stream, error) {
		switch callCount.Add(1) {
		case 1:
			return &mockStream{chunks: []*llm.Chunk{{Calls: []llm.Call{subagentCall}, StopReason: "tool_use"}}}, nil
		case 2:
			return &mockStream{chunks: []*llm.Chunk{{Content: "child evidence", StopReason: "stop"}}}, nil
		default:
			return &mockStream{chunks: []*llm.Chunk{{Content: "parent complete", StopReason: "stop"}}}, nil
		}
	}

	var parent *Controller
	closed := false
	t.Cleanup(func() {
		if !closed {
			if parent != nil {
				_ = parent.Close()
			}
			_ = store.Close()
		}
	})
	parentTool := Tool{
		Name:           tool.SubagentToolName,
		Description:    subagentTool.Spec().Description,
		Parameters:     subagentTool.Spec().Parameters,
		RequiresAction: true,
		ApprovalRequirement: func(args json.RawMessage) (ApprovalRequirement, bool, error) {
			requirement, required, err := subagentTool.ApprovalRequirement(string(args))
			return ApprovalRequirement{
				Category:  requirement.Category,
				Operation: requirement.Operation,
				Resource:  requirement.Resource,
				Metadata:  requirement.Metadata,
			}, required, err
		},
		Execute: func(ctx context.Context, id string, args json.RawMessage, _ <-chan struct{}, _ func(session.ToolPartial)) (session.ToolResultMessage, error) {
			content, details, err := subagentTool.ExecuteDetailed(ctx, string(args))
			result := session.ToolResultMessage{ToolCallID: id, ToolName: tool.SubagentToolName, Timestamp: time.Now()}
			if content != "" {
				result.Content = []session.Content{session.TextContent{Text: content}}
			}
			if details != nil {
				result.Details, _ = json.Marshal(details)
			}
			if err != nil {
				result.IsError = true
				result.Content = append(result.Content, session.TextContent{Text: err.Error()})
			}
			return result, err
		},
	}
	parent = NewController(ControllerConfig{
		Session:        sess,
		Store:          store,
		Durable:        store,
		RequireDurable: true,
		ActionJournal:  store,
		ApprovalMode:   ApprovalTrusted,
		Tools:          []Tool{parentTool},
		StreamFn:       stream,
	})
	subagentTool.SetRunner(parent)

	if _, err := parent.Prompt(context.Background(), "delegate"); err != nil {
		t.Fatalf("parent Prompt() error = %v", err)
	}
	snapshot, err := sess.BuildContext(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	var persisted *session.ToolResultMessage
	for _, message := range snapshot.Messages {
		if result, ok := message.(*session.ToolResultMessage); ok && result.ToolName == tool.SubagentToolName {
			persisted = result
			break
		}
	}
	if persisted == nil {
		t.Fatal("parent session did not persist subagent tool result")
	}
	if session.MessageText(persisted) != "child evidence" || persisted.IsError {
		t.Fatalf("persisted child result = %#v", persisted)
	}
	var details tool.SubagentResult
	if err := json.Unmarshal(persisted.Details, &details); err != nil {
		t.Fatalf("decode persisted child details: %v", err)
	}
	if details.Status != "completed" || details.Output != "child evidence" || details.ChildID == "" {
		t.Fatalf("persisted child details = %#v", details)
	}

	if err := parent.Close(); err != nil {
		t.Fatalf("close parent runtime: %v", err)
	}
	if err := store.Close(); err != nil {
		t.Fatalf("close parent store: %v", err)
	}
	closed = true

	reopened, err := session.NewSQLiteStore(path, "parent")
	if err != nil {
		t.Fatalf("reopen parent store: %v", err)
	}
	defer reopened.Close()
	replayed, err := session.NewSession(reopened, 64).BuildContext(context.Background())
	if err != nil {
		t.Fatalf("replay parent context: %v", err)
	}
	var replayedResult *session.ToolResultMessage
	for _, message := range replayed.Messages {
		if result, ok := message.(*session.ToolResultMessage); ok && result.ToolName == tool.SubagentToolName {
			replayedResult = result
			break
		}
	}
	if replayedResult == nil || session.MessageText(replayedResult) != "child evidence" {
		t.Fatalf("replayed child result = %#v, want persisted evidence", replayedResult)
	}
	var replayedDetails tool.SubagentResult
	if err := json.Unmarshal(replayedResult.Details, &replayedDetails); err != nil {
		t.Fatalf("decode replayed child details: %v", err)
	}
	if replayedDetails.Status != "completed" || replayedDetails.Output != "child evidence" ||
		replayedDetails.ChildID != details.ChildID {
		t.Fatalf("replayed child details = %#v, want original result", replayedDetails)
	}
}

func newChildTestController(
	t *testing.T,
	child ChildRunConfig,
	stream func(context.Context, *llm.Request) (llm.Stream, error),
) *Controller {
	t.Helper()
	store := newTestStore(t)
	h := NewController(ControllerConfig{
		Session:      session.NewSession(store, 64),
		Store:        store,
		Model:        llm.Model{ID: "child-test"},
		Child:        child,
		StreamFn:     stream,
		ApprovalMode: ApprovalConfirm,
	})
	t.Cleanup(func() { _ = h.Close() })
	return h
}
