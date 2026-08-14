package tool

import (
	"context"
	"errors"
	"strings"
	"testing"
)

type fakeCompactRunner struct {
	calledInstructions string
	summary            string
	err                error
}

func (r *fakeCompactRunner) CompactSession(_ context.Context, customInstructions string) (string, error) {
	r.calledInstructions = customInstructions
	return r.summary, r.err
}

func TestCompactToolRequiresRunner(t *testing.T) {
	tool := NewCompactTool()
	_, err := tool.Execute(context.Background(), `{}`)
	if err == nil || !strings.Contains(err.Error(), "runner is unavailable") {
		t.Fatalf("Execute() error = %v, want unavailable runner", err)
	}
}

func TestCompactToolExecutesWithCustomInstructions(t *testing.T) {
	runner := &fakeCompactRunner{summary: "Architecture summary"}
	tool := NewCompactTool()
	tool.SetRunner(runner)

	result, err := tool.Execute(context.Background(), `{"custom_instructions":"Keep database schema decisions"}`)
	if err != nil {
		t.Fatalf("Execute() error = %v", err)
	}
	if runner.calledInstructions != "Keep database schema decisions" {
		t.Fatalf("calledInstructions = %q, want Keep database schema decisions", runner.calledInstructions)
	}
	if !strings.Contains(result, "Architecture summary") {
		t.Fatalf("result = %q, want summary in result", result)
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
