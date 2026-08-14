package tool

import (
	"context"
	"errors"
	"strings"
	"testing"
)

type fakeCompactRunner struct {
	calledInstructions string
	calledContinue     bool
	summary            string
	err                error
}

func (r *fakeCompactRunner) CompactSession(_ context.Context, instructions string, continueAfter bool) (string, error) {
	r.calledInstructions = instructions
	r.calledContinue = continueAfter
	return r.summary, r.err
}

func TestCompactToolRequiresRunner(t *testing.T) {
	tool := NewCompactTool()
	_, err := tool.Execute(context.Background(), `{}`)
	if err == nil || !strings.Contains(err.Error(), "runner is unavailable") {
		t.Fatalf("Execute() error = %v, want unavailable runner", err)
	}
}

func TestCompactToolExecutesWithInstructions(t *testing.T) {
	runner := &fakeCompactRunner{summary: "Architecture summary"}
	tool := NewCompactTool()
	tool.SetRunner(runner)

	result, err := tool.Execute(
		context.Background(),
		`{"instructions":"Keep database schema decisions","continueAfterCompaction":true}`,
	)
	if err != nil {
		t.Fatalf("Execute() error = %v", err)
	}
	if runner.calledInstructions != "Keep database schema decisions" {
		t.Fatalf("calledInstructions = %q, want Keep database schema decisions", runner.calledInstructions)
	}
	if !runner.calledContinue {
		t.Fatalf("calledContinue = false, want true")
	}
	if !strings.Contains(result, "unfinished work will resume") || !strings.Contains(result, "Architecture summary") {
		t.Fatalf("result = %q, want summary and resume text in result", result)
	}
}

func TestCompactToolExecutesWithoutFollowUp(t *testing.T) {
	runner := &fakeCompactRunner{summary: "Final summary"}
	tool := NewCompactTool()
	tool.SetRunner(runner)

	result, err := tool.Execute(context.Background(), `{"instructions":"Finished task","continue_after":false}`)
	if err != nil {
		t.Fatalf("Execute() error = %v", err)
	}
	if runner.calledInstructions != "Finished task" {
		t.Fatalf("calledInstructions = %q, want Finished task", runner.calledInstructions)
	}
	if runner.calledContinue {
		t.Fatalf("calledContinue = true, want false")
	}
	if !strings.Contains(result, "no follow-up turn will be started") {
		t.Fatalf("result = %q, want no follow-up text in result", result)
	}
}

func TestCompactToolPropagatesFailure(t *testing.T) {
	wantErr := errors.New("context too small to compact")
	runner := &fakeCompactRunner{err: wantErr}
	tool := NewCompactTool()
	tool.SetRunner(runner)

	_, err := tool.Execute(context.Background(), `{}`)
	if err == nil || !strings.Contains(err.Error(), wantErr.Error()) {
		t.Fatalf("Execute() error = %v, want %v", err, wantErr)
	}
}
