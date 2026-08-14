package tool

import (
	"context"
	"errors"
	"strings"
	"sync"
	"testing"
)

type fakeSubagentRunner struct {
	mu       sync.Mutex
	requests []SubagentRequest
	result   SubagentResult
	err      error
	emit     bool
}

func (r *fakeSubagentRunner) RunSubagent(_ context.Context, request SubagentRequest) (SubagentResult, error) {
	r.mu.Lock()
	r.requests = append(r.requests, request)
	r.mu.Unlock()

	if r.emit && request.Progress != nil {
		request.Progress("live child output")
	}
	res := r.result
	if res.Output == "" && res.Status == "" {
		res.Status = "completed"
		res.Output = "result for " + request.Task
	}
	return res, r.err
}

func (r *fakeSubagentRunner) lastRequest() SubagentRequest {
	r.mu.Lock()
	defer r.mu.Unlock()
	if len(r.requests) == 0 {
		return SubagentRequest{}
	}
	return r.requests[len(r.requests)-1]
}

func (r *fakeSubagentRunner) allRequests() []SubagentRequest {
	r.mu.Lock()
	defer r.mu.Unlock()
	return append([]SubagentRequest(nil), r.requests...)
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
	last := runner.lastRequest()
	if last.Task != "inspect" || last.MaxToolIterations != 3 {
		t.Fatalf("runner request = %#v", last)
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
	last := runner.lastRequest()
	if last.Model != "google/gemini-2.5-flash" {
		t.Fatalf("runner request model = %q, want google/gemini-2.5-flash", last.Model)
	}
}

func TestSubagentToolParallelExecution(t *testing.T) {
	runner := &fakeSubagentRunner{}
	tool := NewSubagentTool()
	tool.SetRunner(runner)

	input := `{"tasks":[
		{"task":"review auth"},
		{"task":"review performance","model":"deepseek/deepseek-v4-pro"}
	]}`

	content, details, err := tool.ExecuteDetailed(context.Background(), input)
	if err != nil {
		t.Fatalf("ExecuteDetailed(parallel) error = %v", err)
	}
	if !strings.Contains(content, "### Task 1: review auth") ||
		!strings.Contains(content, "### Task 2: review performance") {
		t.Fatalf("unexpected content = %q", content)
	}
	batch, ok := details.(SubagentBatchResult)
	if !ok || batch.Mode != "parallel" || len(batch.Results) != 2 {
		t.Fatalf("details = %#v, want parallel batch result with 2 items", details)
	}

	reqs := runner.allRequests()
	if len(reqs) != 2 {
		t.Fatalf("runner received %d requests, want 2", len(reqs))
	}
}

func TestSubagentToolChainExecutionWithInterpolation(t *testing.T) {
	runner := &fakeSubagentRunner{}
	tool := NewSubagentTool()
	tool.SetRunner(runner)

	input := `{"chain":[
		{"task":"explore codebase"},
		{"task":"apply fix based on: {previous}"}
	]}`

	content, details, err := tool.ExecuteDetailed(context.Background(), input)
	if err != nil {
		t.Fatalf("ExecuteDetailed(chain) error = %v", err)
	}
	if !strings.Contains(content, "### Step 1:") || !strings.Contains(content, "### Step 2:") {
		t.Fatalf("unexpected content = %q", content)
	}
	batch, ok := details.(SubagentBatchResult)
	if !ok || batch.Mode != "chain" || len(batch.Results) != 2 {
		t.Fatalf("details = %#v, want chain batch result with 2 items", details)
	}

	reqs := runner.allRequests()
	if len(reqs) != 2 {
		t.Fatalf("runner received %d requests, want 2", len(reqs))
	}
	if reqs[0].Task != "explore codebase" {
		t.Fatalf("step 1 task = %q", reqs[0].Task)
	}
	if !strings.Contains(reqs[1].Task, "result for explore codebase") {
		t.Fatalf("step 2 task = %q, expected interpolation of previous output", reqs[1].Task)
	}
}

func TestSubagentToolRejectsInvalidInput(t *testing.T) {
	tool := NewSubagentTool()
	tool.SetRunner(&fakeSubagentRunner{})
	for _, input := range []string{
		`{}`,
		`{"task":"inspect","max_tool_iterations":-1}`,
		`{"task":"inspect","max_tool_iterations":101}`,
		`{"task":"inspect","tasks":[{"task":"sub"}]}`,
		`{"tasks":[]}`,
		`{"chain":[]}`,
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

	// Test parallel mode requirement
	reqParallel, ok, err := tool.ApprovalRequirement(`{"tasks":[{"task":"a"},{"task":"b"}]}`)
	if err != nil || !ok {
		t.Fatalf("ApprovalRequirement(parallel) = %#v, %t, %v", reqParallel, ok, err)
	}
	if reqParallel.Resource != "2 parallel tasks" {
		t.Fatalf("resource = %q, want '2 parallel tasks'", reqParallel.Resource)
	}
}
