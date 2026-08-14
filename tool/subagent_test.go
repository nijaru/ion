package tool

import (
	"context"
	"errors"
	"strings"
	"testing"
)

type fakeSubagentRunner struct {
	request SubagentRequest
	result  SubagentResult
	err     error
	emit    bool
}

func (r *fakeSubagentRunner) RunSubagent(_ context.Context, request SubagentRequest) (SubagentResult, error) {
	r.request = request
	if r.emit && request.Progress != nil {
		request.Progress("live child output")
	}
	return r.result, r.err
}

func TestSubagentToolRequiresRuntimeRunner(t *testing.T) {
	tool := NewSubagentTool()
	_, err := tool.Execute(context.Background(), `{"task":"inspect"}`)
	if err == nil || !strings.Contains(err.Error(), "runtime is unavailable") {
		t.Fatalf("Execute() error = %v, want unavailable runtime", err)
	}
}

func TestSubagentToolForwardsLiveProgress(t *testing.T) {
	runner := &fakeSubagentRunner{emit: true, result: SubagentResult{Status: "completed", Output: "done"}}
	tool := NewSubagentTool()
	tool.SetRunner(runner)
	var updates []StreamUpdate

	content, _, err := tool.ExecuteDetailedWithProgress(
		context.Background(),
		`{"task":"inspect"}`,
		func(update StreamUpdate) { updates = append(updates, update) },
	)
	if err != nil || content != "done" {
		t.Fatalf("ExecuteDetailedWithProgress() = %q, %v", content, err)
	}
	if len(updates) != 1 || updates[0].Text != "live child output" {
		t.Fatalf("updates = %#v, want live child output", updates)
	}
}

func TestSubagentToolDelegatesAndReturnsDetails(t *testing.T) {
	runner := &fakeSubagentRunner{result: SubagentResult{
		ChildID: "child-1",
		Status:  "completed",
		Output:  "done",
	}}
	tool := NewSubagentTool()
	tool.SetRunner(runner)

	content, details, err := tool.ExecuteDetailed(
		context.Background(),
		`{"task":"  inspect  ","max_tool_iterations":3}`,
	)
	if err != nil {
		t.Fatalf("ExecuteDetailed() error = %v", err)
	}
	if content != "done" {
		t.Fatalf("content = %q, want done", content)
	}
	if got, ok := details.(SubagentResult); !ok || got.ChildID != "child-1" {
		t.Fatalf("details = %#v, want child result", details)
	}
	if runner.request.Task != "inspect" || runner.request.MaxToolIterations != 3 {
		t.Fatalf("runner request = %#v", runner.request)
	}
}

func TestSubagentToolDecodesModel(t *testing.T) {
	runner := &fakeSubagentRunner{result: SubagentResult{Status: "completed", Output: "done"}}
	tool := NewSubagentTool()
	tool.SetRunner(runner)

	_, _, err := tool.ExecuteDetailed(
		context.Background(),
		`{"task":"inspect","model":"google/gemini-2.5-flash"}`,
	)
	if err != nil {
		t.Fatalf("ExecuteDetailed() error = %v", err)
	}
	if runner.request.Model != "google/gemini-2.5-flash" {
		t.Fatalf("runner request model = %q, want google/gemini-2.5-flash", runner.request.Model)
	}
}

func TestSubagentToolRejectsInvalidInput(t *testing.T) {
	tool := NewSubagentTool()
	tool.SetRunner(&fakeSubagentRunner{})
	for _, input := range []string{
		`{}`,
		`{"task":"inspect","max_tool_iterations":-1}`,
		`{"task":"inspect","max_tool_iterations":17}`,
	} {
		if _, err := tool.Execute(context.Background(), input); err == nil {
			t.Fatalf("Execute(%s) succeeded, want validation error", input)
		}
	}
}

func TestSubagentToolPropagatesRunnerFailure(t *testing.T) {
	wantErr := errors.New("child provider failed")
	tool := NewSubagentTool()
	tool.SetRunner(&fakeSubagentRunner{
		result: SubagentResult{Status: "failed", Error: wantErr.Error()},
		err:    wantErr,
	})
	_, err := tool.Execute(context.Background(), `{"task":"inspect"}`)
	if !errors.Is(err, wantErr) {
		t.Fatalf("Execute() error = %v, want %v", err, wantErr)
	}
}

func TestSubagentToolApprovalRequirementDisablesRecursion(t *testing.T) {
	tool := NewSubagentTool()
	req, ok, err := tool.ApprovalRequirement(`{"task":"inspect","max_tool_iterations":2}`)
	if err != nil || !ok {
		t.Fatalf("ApprovalRequirement() = %#v, %t, %v", req, ok, err)
	}
	if req.Operation != "run_subagent" || req.Metadata["recursion"] != "disabled" {
		t.Fatalf("approval requirement = %#v", req)
	}
}
