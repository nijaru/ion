package agent

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/session"
	"github.com/nijaru/ion/tool"
)

func TestJournalActionBoundaryBindsCanonicalActionIdentity(t *testing.T) {
	ctx := context.Background()
	store := newTestStore(t)
	workdir := t.TempDir()
	boundary := newJournalActionBoundary(
		store, NewApprovalBroker(ApprovalTrusted, false, nil), ApprovalTrusted, false, workdir,
	)
	requirement := ApprovalRequirement{
		Category: "write", Operation: "write", Resource: "nested/../main.go",
		Environment: []string{"A", "B"},
		Metadata:    map[string]any{"surface": "editor", "revision": 1},
	}
	token, err := boundary.PrepareAndAuthorize(ctx, ActionRequest{
		ToolName: "write", InvocationID: "call-1", Arguments: []byte(`{"path":"nested/../main.go","content":"x"}`),
		SessionID: "session-1", TurnID: "turn-1",
		Requirement: requirement, Required: true,
	})
	if err != nil {
		t.Fatalf("prepare and authorize: %v", err)
	}
	if token == nil || token.Record.State != session.ActionAuthorized {
		t.Fatalf("token = %#v, want authorized token", token)
	}
	resolvedWorkdir, err := filepath.EvalSymlinks(workdir)
	if err != nil {
		t.Fatal(err)
	}
	wantPath := filepath.Join(resolvedWorkdir, "main.go")
	if len(token.Record.Paths) != 1 || token.Record.Paths[0] != wantPath {
		t.Fatalf("canonical paths = %#v, want [%q]", token.Record.Paths, wantPath)
	}
	if token.Record.Fingerprint == "" || token.Record.ID == "" {
		t.Fatalf("identity missing: %#v", token.Record)
	}
	second, err := boundary.PrepareAndAuthorize(ctx, ActionRequest{
		ToolName: "write", InvocationID: "call-2", Arguments: []byte(`{"path":"nested/../main.go","content":"x"}`),
		SessionID: "session-1", TurnID: "turn-1", Requirement: requirement, Required: true,
	})
	if err != nil {
		t.Fatalf("second prepare and authorize: %v", err)
	}
	if second.ID == token.ID || second.Record.Fingerprint != token.Record.Fingerprint {
		t.Fatalf("action identity = first (%s, %s), second (%s, %s); invocation identity must be separate from operation fingerprint", token.ID, token.Record.Fingerprint, second.ID, second.Record.Fingerprint)
	}
	if string(token.Record.Metadata) != `{"revision":1,"surface":"editor"}` {
		t.Fatalf("canonical metadata = %s", token.Record.Metadata)
	}
	equivalent, err := boundary.PrepareAndAuthorize(ctx, ActionRequest{
		ToolName: " write ", InvocationID: "call-equivalent", Arguments: []byte(`{"content":"x","path":"nested/../main.go"}`),
		SessionID: "session-1", TurnID: "turn-1",
		Requirement: ApprovalRequirement{
			Category: "WRITE", Operation: "WRITE", Paths: []string{"main.go", "nested/../main.go"},
			Environment: []string{"B", "A", "A"}, Metadata: map[string]any{"revision": 1, "surface": "editor"},
		}, Required: true,
	})
	if err != nil {
		t.Fatalf("equivalent prepare and authorize: %v", err)
	}
	if equivalent.Record.Fingerprint != token.Record.Fingerprint {
		t.Fatalf("equivalent action fingerprint = %s, want %s", equivalent.Record.Fingerprint, token.Record.Fingerprint)
	}
	if err := boundary.Start(ctx, token); err != nil {
		t.Fatalf("start: %v", err)
	}
	if err := boundary.Finish(ctx, token, ActionResult{ResultIdentity: "result-1"}); err != nil {
		t.Fatalf("finish: %v", err)
	}
	got, err := store.GetAction(ctx, token.ID)
	if err != nil {
		t.Fatal(err)
	}
	if got.State != session.ActionCompleted {
		t.Fatalf("durable state = %s, want completed", got.State)
	}
	if err := boundary.Start(ctx, second); err != nil {
		t.Fatalf("second start: %v", err)
	}
	if err := boundary.Finish(ctx, second, ActionResult{ResultIdentity: "result-2"}); err != nil {
		t.Fatalf("second finish: %v", err)
	}
}

func TestJournalActionBoundaryRejectsWorkspaceEscapeAndMissingJournal(t *testing.T) {
	ctx := context.Background()
	workdir := t.TempDir()
	boundary := newJournalActionBoundary(
		newTestStore(t), NewApprovalBroker(ApprovalTrusted, false, nil), ApprovalTrusted, false, workdir,
	)
	_, err := boundary.PrepareAndAuthorize(ctx, ActionRequest{
		ToolName: "write", InvocationID: "call-escape", Arguments: []byte(`{"path":"../outside","content":"x"}`),
		SessionID: "session-1", TurnID: "turn-1",
		Requirement: ApprovalRequirement{Category: "write", Operation: "write", Resource: "../outside"}, Required: true,
	})
	if err == nil || !containsActionError(err, "escapes workspace") {
		t.Fatalf("escape error = %v, want workspace rejection", err)
	}
	missing := newJournalActionBoundary(nil, nil, ApprovalConfirm, false, workdir)
	_, err = missing.PrepareAndAuthorize(ctx, ActionRequest{
		ToolName: "bash", InvocationID: "call-no-journal", Arguments: []byte(`{"command":"echo hi"}`),
		SessionID: "session-1", TurnID: "turn-1",
		Requirement: ApprovalRequirement{Category: "execute", Operation: "bash", Resource: "echo hi"}, Required: true,
	})
	if err == nil || !strings.Contains(err.Error(), "external action journal is unavailable") {
		t.Fatalf("missing journal error = %v, want fail closed", err)
	}
}

func TestJournalActionBoundaryRejectsChangedFilePreimageBeforeEffect(t *testing.T) {
	ctx := t.Context()
	workdir := t.TempDir()
	path := filepath.Join(workdir, "tracked.txt")
	if err := os.WriteFile(path, []byte("before"), 0o644); err != nil {
		t.Fatal(err)
	}
	store := newTestStore(t)
	boundary := newJournalActionBoundary(
		store, NewApprovalBroker(ApprovalTrusted, false, nil), ApprovalTrusted, false, workdir,
	)
	token, err := boundary.PrepareAndAuthorize(ctx, ActionRequest{
		ToolName: "edit", InvocationID: "call-preimage", SessionID: "session-1", TurnID: "turn-1",
		Arguments: []byte(`{"path":"tracked.txt","edits":[]}`),
		Requirement: ApprovalRequirement{
			Category: "write", Operation: "edit", Resource: "tracked.txt", Paths: []string{"tracked.txt"},
		}, Required: true,
	})
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte("changed"), 0o644); err != nil {
		t.Fatal(err)
	}
	invoked := false
	result, err := boundary.Execute(ctx, token, func(context.Context, <-chan struct{}, func(session.ToolPartial)) (session.ToolResultMessage, error) {
		invoked = true
		return session.ToolResultMessage{}, nil
	}, nil, nil)
	if err == nil || !result.IsError || invoked {
		t.Fatalf("preimage result = %#v, err = %v, invoked = %v; want rejection before effect", result, err, invoked)
	}
	record, err := store.GetAction(ctx, token.ID)
	if err != nil {
		t.Fatal(err)
	}
	if record.State != session.ActionFailed || !strings.Contains(record.Error, "preimage") {
		t.Fatalf("action after preimage rejection = %#v", record)
	}
	transitions, err := store.ActionTransitions(ctx, token.ID)
	if err != nil {
		t.Fatal(err)
	}
	for _, transition := range transitions {
		if transition.To == session.ActionStarted {
			t.Fatalf("preimage rejection crossed start boundary: %#v", transitions)
		}
	}
}

func TestJournalActionBoundaryExecutesWorkspaceWriteAfterDurableStart(t *testing.T) {
	ctx := t.Context()
	workdir := t.TempDir()
	store := newTestStore(t)
	boundary := newJournalActionBoundary(
		store, NewApprovalBroker(ApprovalTrusted, false, nil), ApprovalTrusted, false, workdir,
	)
	token, err := boundary.PrepareAndAuthorize(ctx, ActionRequest{
		ToolName: "write", InvocationID: "call-write-file", SessionID: "session-1", TurnID: "turn-1",
		Arguments: []byte(`{"path":"nested/result.txt","content":"durable\n"}`),
		Requirement: ApprovalRequirement{
			Category: "write", Operation: "write", Resource: "nested/result.txt", Paths: []string{"nested/result.txt"},
		}, Required: true,
	})
	if err != nil {
		t.Fatal(err)
	}
	writer := &tool.Write{FileTool: *tool.NewFileTool(workdir)}
	result, err := boundary.Execute(ctx, token, func(execCtx context.Context, _ <-chan struct{}, _ func(session.ToolPartial)) (session.ToolResultMessage, error) {
		record, recordErr := store.GetAction(execCtx, token.ID)
		if recordErr != nil {
			return session.ToolResultMessage{}, recordErr
		}
		if record.State != session.ActionStarted {
			return session.ToolResultMessage{}, fmt.Errorf("write crossed boundary in state %s", record.State)
		}
		output, writeErr := writer.Execute(execCtx, string(token.Record.Arguments))
		return session.ToolResultMessage{Content: []session.Content{session.TextContent{Text: output}}}, writeErr
	}, nil, nil)
	if err != nil || result.IsError {
		t.Fatalf("write result = %#v, err = %v", result, err)
	}
	content, err := os.ReadFile(filepath.Join(workdir, "nested", "result.txt"))
	if err != nil {
		t.Fatal(err)
	}
	if string(content) != "durable\n" {
		t.Fatalf("written content = %q", content)
	}
	record, err := store.GetAction(ctx, token.ID)
	if err != nil {
		t.Fatal(err)
	}
	if record.State != session.ActionCompleted {
		t.Fatalf("action state = %s, want completed", record.State)
	}
}

func TestControllerRoutesActionTransitionsThroughRuntimeOwner(t *testing.T) {
	ctx := t.Context()
	store := newTestStore(t)
	sess := session.NewSession(store, 64)
	h := NewController(ControllerConfig{
		Session:             sess,
		Store:               store,
		Durable:             store,
		ActionJournal:       store,
		ApprovalMode:        ApprovalTrusted,
		ApprovalInteractive: false,
		Workdir:             t.TempDir(),
	})
	t.Cleanup(func() { _ = h.Close() })

	coordinator, err := h.actionCoordinator()
	if err != nil {
		t.Fatal(err)
	}
	token, err := coordinator.PrepareAndAuthorize(ctx, ActionRequest{
		ToolName: "write", InvocationID: "call-controller", SessionID: sess.Meta().ID,
		TurnID: "turn-controller", Arguments: []byte(`{"path":"main.go","content":"ok"}`),
		Requirement: ApprovalRequirement{
			Category: "write", Operation: "write", Resource: "main.go", Paths: []string{"main.go"},
		}, Required: true,
	})
	if err != nil {
		t.Fatalf("prepare through controller: %v", err)
	}
	if err := coordinator.Start(ctx, token); err != nil {
		t.Fatalf("start through controller: %v", err)
	}
	if err := coordinator.Finish(ctx, token, ActionResult{ResultIdentity: "result-controller"}); err != nil {
		t.Fatalf("finish through controller: %v", err)
	}
	record, err := store.GetAction(ctx, token.ID)
	if err != nil {
		t.Fatal(err)
	}
	if record.State != session.ActionCompleted {
		t.Fatalf("action state = %s, want completed", record.State)
	}
}

func TestJournalActionBoundaryCancellationAfterStartIsIndeterminate(t *testing.T) {
	ctx := t.Context()
	store := newTestStore(t)
	boundary := newJournalActionBoundary(
		store, NewApprovalBroker(ApprovalTrusted, false, nil), ApprovalTrusted, false, t.TempDir(),
	)
	token, err := boundary.PrepareAndAuthorize(ctx, ActionRequest{
		ToolName: "bash", InvocationID: "call-cancel-started", SessionID: "session-1", TurnID: "turn-1",
		Arguments:   []byte(`{"command":"true"}`),
		Requirement: ApprovalRequirement{Category: "execute", Operation: "bash", Resource: "true"}, Required: true,
	})
	if err != nil {
		t.Fatal(err)
	}
	if err := boundary.Start(ctx, token); err != nil {
		t.Fatal(err)
	}
	if err := boundary.Cancel(ctx, token, "user canceled"); err != nil {
		t.Fatalf("cancel: %v", err)
	}
	record, err := store.GetAction(ctx, token.ID)
	if err != nil {
		t.Fatal(err)
	}
	if record.State != session.ActionIndeterminate {
		t.Fatalf("cancelled started action = %s, want indeterminate", record.State)
	}
}

func TestPrepareToolCallUsesInvocationDescriptorForDynamicEffects(t *testing.T) {
	actionBoundary := &actionBoundaryStub{}
	tool := Tool{
		Name:           "bash",
		RequiresAction: true,
		ApprovalRequirement: func(json.RawMessage) (ApprovalRequirement, bool, error) {
			return ApprovalRequirement{}, false, nil
		},
	}
	prepared, result := prepareToolCall(
		context.Background(), TurnContext{}, session.AssistantMessage{},
		&session.ToolCall{ID: "call-output", Name: "bash", Arguments: map[string]any{"action": "output"}},
		LoopConfig{Tools: []Tool{tool}, ActionBoundary: actionBoundary}, nil,
	)
	if result != nil {
		t.Fatalf("preparation result = %#v, want no error result", result)
	}
	if prepared.action != nil {
		t.Fatalf("prepared action = %#v, want no action for a non-effect invocation", prepared.action)
	}
}

func TestPrepareToolCallRejectsEffectWithoutActionBoundary(t *testing.T) {
	tool := Tool{
		Name:           "write",
		RequiresAction: true,
		ApprovalRequirement: func(json.RawMessage) (ApprovalRequirement, bool, error) {
			return ApprovalRequirement{Category: "write", Operation: "write", Resource: "main.go"}, true, nil
		},
	}
	_, result := prepareToolCall(
		context.Background(), TurnContext{}, session.AssistantMessage{},
		&session.ToolCall{ID: "call-unbound", Name: "write", Arguments: map[string]any{}},
		LoopConfig{Tools: []Tool{tool}}, nil,
	)
	if result == nil || !result.IsError || !strings.Contains(firstToolResultText(*result), "action boundary") {
		t.Fatalf("result = %#v, want fail-closed boundary error", result)
	}
}

func TestJournalActionBoundaryRejectsUnboundProcessLaunch(t *testing.T) {
	ctx := t.Context()
	workdir := t.TempDir()
	store := newTestStore(t)
	boundary := newJournalActionBoundary(
		store, NewApprovalBroker(ApprovalTrusted, false, nil), ApprovalTrusted, false, workdir,
	)
	token, err := boundary.PrepareAndAuthorize(ctx, ActionRequest{
		ToolName: "bash", InvocationID: "call-process", SessionID: "session-1", TurnID: "turn-1",
		Arguments:   []byte(`{"command":"true"}`),
		Requirement: ApprovalRequirement{Category: "execute", Operation: "bash", Resource: "true"}, Required: true,
	})
	if err != nil {
		t.Fatal(err)
	}
	_, err = boundary.Execute(ctx, token, func(ctx context.Context, _ <-chan struct{}, _ func(session.ToolPartial)) (session.ToolResultMessage, error) {
		recorder, ok := tool.ProcessIdentityRecorderFromContext(ctx)
		if !ok {
			return session.ToolResultMessage{}, errors.New("process recorder missing from action context")
		}
		if err := recorder(tool.ProcessLaunch{}); !errors.Is(err, tool.ErrInvalidProcessLaunch) {
			return session.ToolResultMessage{}, fmt.Errorf("unbound launch error = %v, want ErrInvalidProcessLaunch", err)
		}
		return session.ToolResultMessage{Content: []session.Content{session.TextContent{Text: "unbound launch rejected"}}}, errors.New("executor did not launch a bound process")
	}, nil, nil)
	if err == nil {
		t.Fatal("unbound process launch unexpectedly completed")
	}
	record, err := store.GetAction(ctx, token.ID)
	if err != nil {
		t.Fatal(err)
	}
	if record.State != session.ActionIndeterminate || record.ProcessIdentity != "" {
		t.Fatalf("unbound process action = %#v, want indeterminate without identity", record)
	}
}

func TestJournalActionBoundaryPreservesUncertainEffectOutcome(t *testing.T) {
	ctx := t.Context()
	workdir := t.TempDir()
	store := newTestStore(t)
	boundary := newJournalActionBoundary(
		store, NewApprovalBroker(ApprovalTrusted, false, nil), ApprovalTrusted, false, workdir,
	)
	token, err := boundary.PrepareAndAuthorize(ctx, ActionRequest{
		ToolName: "bash", InvocationID: "call-uncertain", SessionID: "session-1", TurnID: "turn-1",
		Arguments:   []byte(`{"command":"possibly-mutating"}`),
		Requirement: ApprovalRequirement{Category: "execute", Operation: "bash", Resource: "possibly-mutating"}, Required: true,
	})
	if err != nil {
		t.Fatal(err)
	}
	result, err := boundary.Execute(ctx, token, func(context.Context, <-chan struct{}, func(session.ToolPartial)) (session.ToolResultMessage, error) {
		return session.ToolResultMessage{
			Content: []session.Content{session.TextContent{Text: "executor failed after starting"}},
			IsError: true,
		}, errors.New("executor failed after starting")
	}, nil, nil)
	if err == nil || !result.IsError {
		t.Fatalf("result = %#v, err = %v; want executor error", result, err)
	}
	record, err := store.GetAction(ctx, token.ID)
	if err != nil {
		t.Fatal(err)
	}
	if record.State != session.ActionIndeterminate || !strings.Contains(record.Error, "indeterminate") {
		t.Fatalf("action outcome = %#v, want indeterminate", record)
	}
}

func TestJournalActionBoundaryRecordsRealBashProcessGroup(t *testing.T) {
	t.Setenv("ION_SANDBOX", string(tool.SandboxOff))
	ctx := t.Context()
	workdir := t.TempDir()
	store := newTestStore(t)
	boundary := newJournalActionBoundary(
		store, NewApprovalBroker(ApprovalTrusted, false, nil), ApprovalTrusted, false, workdir,
	)
	token, err := boundary.PrepareAndAuthorize(ctx, ActionRequest{
		ToolName: "bash", InvocationID: "call-real-process", SessionID: "session-1", TurnID: "turn-1",
		Arguments:   []byte(`{"command":"printf ok"}`),
		Requirement: ApprovalRequirement{Category: "execute", Operation: "bash", Resource: "printf ok"}, Required: true,
	})
	if err != nil {
		t.Fatal(err)
	}
	bash := tool.NewBash(workdir)
	result, err := boundary.Execute(ctx, token, func(ctx context.Context, _ <-chan struct{}, _ func(session.ToolPartial)) (session.ToolResultMessage, error) {
		output, err := bash.Execute(ctx, `{"command":"printf ok"}`)
		return session.ToolResultMessage{Content: []session.Content{session.TextContent{Text: output}}}, err
	}, nil, nil)
	if err != nil || result.IsError {
		t.Fatalf("bash result = %#v, err = %v", result, err)
	}
	record, err := store.GetAction(ctx, token.ID)
	if err != nil {
		t.Fatal(err)
	}
	if record.State != session.ActionCompleted || record.ProcessIdentity == "" {
		t.Fatalf("real bash action = %#v, want completed process group identity", record)
	}
}

func TestJournalActionBoundaryTracksBackgroundJobToTerminalState(t *testing.T) {
	t.Setenv("ION_SANDBOX", string(tool.SandboxOff))
	ctx := t.Context()
	workdir := t.TempDir()
	store := newTestStore(t)
	jobs := tool.NewJobManager()
	t.Cleanup(func() { _ = jobs.Close() })
	boundary := newJournalActionBoundary(
		store, NewApprovalBroker(ApprovalTrusted, false, nil), ApprovalTrusted, false, workdir,
	)
	token, err := boundary.PrepareAndAuthorize(ctx, ActionRequest{
		ToolName: "bash", InvocationID: "call-background", SessionID: "session-1", TurnID: "turn-1",
		Arguments: []byte(`{"command":"printf done; sleep 0.05","background":true}`),
		Requirement: ApprovalRequirement{
			Category: "execute", Operation: "bash", Resource: "printf done; sleep 0.05",
			Metadata: map[string]any{"background": true},
		}, Required: true,
	})
	if err != nil {
		t.Fatal(err)
	}
	bash := tool.NewBashWithEnvironmentAndJobs(workdir, tool.NewEnvironmentPolicy("inherit", nil), jobs)
	result, err := boundary.Execute(ctx, token, func(ctx context.Context, _ <-chan struct{}, _ func(session.ToolPartial)) (session.ToolResultMessage, error) {
		output, err := bash.Execute(ctx, `{"command":"printf done; sleep 0.05","background":true}`)
		return session.ToolResultMessage{Content: []session.Content{session.TextContent{Text: output}}}, err
	}, nil, nil)
	if err != nil || result.IsError {
		t.Fatalf("background launch result = %#v, err = %v", result, err)
	}
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		record, err := store.GetAction(ctx, token.ID)
		if err != nil {
			t.Fatal(err)
		}
		if record.State == session.ActionCompleted {
			if record.ProcessIdentity == "" {
				t.Fatalf("completed background action lost process group identity: %#v", record)
			}
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	record, _ := store.GetAction(ctx, token.ID)
	t.Fatalf("background action did not reach terminal state: %#v", record)
}

func TestJournalActionBoundarySurfacesBackgroundFinalizeFailure(t *testing.T) {
	t.Setenv("ION_SANDBOX", string(tool.SandboxOff))
	ctx := t.Context()
	workdir := t.TempDir()
	store := newTestStore(t)
	jobs := tool.NewJobManager()
	t.Cleanup(func() { _ = jobs.Close() })
	finishErr := errors.New("action journal is unavailable")
	journal := &finishFailingActionJournal{ActionJournal: store, err: finishErr}
	boundary := newJournalActionBoundary(
		journal, NewApprovalBroker(ApprovalTrusted, false, nil), ApprovalTrusted, false, workdir,
	)
	token, err := boundary.PrepareAndAuthorize(ctx, ActionRequest{
		ToolName: "bash", InvocationID: "call-background-finish-failure", SessionID: "session-1", TurnID: "turn-1",
		Arguments: []byte(`{"command":"printf done; sleep 0.05","background":true}`),
		Requirement: ApprovalRequirement{
			Category: "execute", Operation: "bash", Resource: "printf done; sleep 0.05",
			Metadata: map[string]any{"background": true},
		}, Required: true,
	})
	if err != nil {
		t.Fatal(err)
	}
	bash := tool.NewBashWithEnvironmentAndJobs(workdir, tool.NewEnvironmentPolicy("inherit", nil), jobs)
	result, err := boundary.Execute(ctx, token, func(ctx context.Context, _ <-chan struct{}, _ func(session.ToolPartial)) (session.ToolResultMessage, error) {
		output, err := bash.Execute(ctx, `{"command":"printf done; sleep 0.05","background":true}`)
		return session.ToolResultMessage{Content: []session.Content{session.TextContent{Text: output}}}, err
	}, nil, nil)
	if err != nil || result.IsError {
		t.Fatalf("background launch result = %#v, err = %v", result, err)
	}

	deadline := time.Now().Add(2 * time.Second)
	var job tool.JobSnapshot
	for time.Now().Before(deadline) {
		job, err = jobs.Get("job-1")
		if err != nil {
			t.Fatal(err)
		}
		if job.Error != "" {
			break
		}
		time.Sleep(10 * time.Millisecond)
	}
	if !strings.Contains(job.Error, "finalize background action") || !strings.Contains(job.Error, finishErr.Error()) {
		t.Fatalf("job error = %q, want surfaced finalize failure", job.Error)
	}
	record, err := store.GetAction(ctx, token.ID)
	if err != nil {
		t.Fatal(err)
	}
	if record.State != session.ActionStarted {
		t.Fatalf("action state = %s, want started/unsettled after failed finalization", record.State)
	}
}

type finishFailingActionJournal struct {
	session.ActionJournal
	err error
}

func (j *finishFailingActionJournal) FinishAction(context.Context, string, session.ActionState, string, string, string) (session.ActionRecord, error) {
	return session.ActionRecord{}, j.err
}

func containsActionError(err error, text string) bool {
	return err != nil && strings.Contains(err.Error(), text)
}

type actionBoundaryStub struct {
	startErr  error
	started   bool
	finished  ActionResult
	finishSet bool
}

func (s *actionBoundaryStub) PrepareAndAuthorize(context.Context, ActionRequest) (*ActionToken, error) {
	return &ActionToken{ID: "stub-action", Record: session.ActionRecord{ID: "stub-action"}}, nil
}

func (s *actionBoundaryStub) Start(context.Context, *ActionToken) error {
	if s.startErr != nil {
		return s.startErr
	}
	s.started = true
	return nil
}

func (s *actionBoundaryStub) Finish(_ context.Context, _ *ActionToken, result ActionResult) error {
	s.finished = result
	s.finishSet = true
	return nil
}

func (s *actionBoundaryStub) Execute(
	ctx context.Context,
	token *ActionToken,
	invoke ActionInvoker,
	signal <-chan struct{},
	progress func(session.ToolPartial),
) (session.ToolResultMessage, error) {
	if err := s.Start(ctx, token); err != nil {
		return session.ToolResultMessage{IsError: true}, err
	}
	result, err := invoke(ctx, signal, progress)
	actionResult := ActionResult{State: session.ActionCompleted}
	if err != nil || result.IsError {
		actionResult.State = session.ActionFailed
	}
	_ = s.Finish(ctx, token, actionResult)
	return result, err
}

func (s *actionBoundaryStub) Cancel(ctx context.Context, token *ActionToken, reason string) error {
	return s.Finish(ctx, token, ActionResult{State: session.ActionCancelled, Error: reason})
}

func TestToolExecutionCannotCrossFailedStartBoundary(t *testing.T) {
	stub := &actionBoundaryStub{startErr: errors.New("journal start failed")}
	executed := false
	tool := &Tool{
		Name:           "write",
		RequiresAction: true,
		Execute: func(context.Context, string, json.RawMessage, <-chan struct{}, func(session.ToolPartial)) (session.ToolResultMessage, error) {
			executed = true
			return session.ToolResultMessage{}, nil
		},
	}
	call := &session.ToolCall{ID: "call-1", Name: "write"}
	result := executePreparedToolCall(
		context.Background(), TurnContext{}, session.AssistantMessage{},
		preparedToolCall{tool: tool, tc: call, argsRaw: []byte(`{}`), action: &ActionToken{ID: "stub-action"}},
		LoopConfig{ActionBoundary: stub}, func(session.Event) {}, make(chan struct{}),
	)
	if executed {
		t.Fatal("tool executed after durable start failure")
	}
	if !result.IsError || stub.finishSet {
		t.Fatalf("result = %#v, finish = %#v, start failure must remain recoverable", result, stub.finished)
	}
}

func TestToolExecutionFinalizesStartedActionFailure(t *testing.T) {
	stub := &actionBoundaryStub{}
	tool := &Tool{
		Name: "bash",
		Execute: func(context.Context, string, json.RawMessage, <-chan struct{}, func(session.ToolPartial)) (session.ToolResultMessage, error) {
			return session.ToolResultMessage{
				Content: []session.Content{session.TextContent{Text: "command failed"}}, IsError: true,
			}, nil
		},
	}
	result := executePreparedToolCall(
		context.Background(), TurnContext{}, session.AssistantMessage{},
		preparedToolCall{tool: tool, tc: &session.ToolCall{ID: "call-2", Name: "bash"}, argsRaw: []byte(`{}`), action: &ActionToken{ID: "stub-action"}},
		LoopConfig{ActionBoundary: stub}, func(session.Event) {}, make(chan struct{}),
	)
	if !result.IsError || !stub.started || !stub.finishSet || stub.finished.State != session.ActionFailed {
		t.Fatalf("result = %#v, started=%v, finish=%#v", result, stub.started, stub.finished)
	}
}
