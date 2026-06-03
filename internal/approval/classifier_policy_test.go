package approval_test

import (
	"context"
	"testing"

	"github.com/nijaru/ion/internal/approval"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

type mockClassifier struct {
	Label  string
	Reason string
	Err    error
}

func (m *mockClassifier) Classify(
	ctx context.Context,
	input string,
	labels []string,
) (*llm.Classification, error) {
	if m.Err != nil {
		return nil, m.Err
	}
	return &llm.Classification{Label: m.Label, Reason: m.Reason}, nil
}

func TestClassifierPolicy(t *testing.T) {
	classifier := &mockClassifier{Label: "allow", Reason: "good"}
	policy := approval.NewClassifierPolicy(classifier, []string{"allow", "deny"})
	sess := session.New("test")

	res, handled, err := policy.Decide(context.Background(), sess, approval.Request{Tool: "t1"})
	if err != nil {
		t.Fatalf("Decide: %v", err)
	}
	if !handled {
		t.Fatal("expected handled policy")
	}
	if res.Decision != approval.DecisionAllow {
		t.Errorf("got decision %v, want allow", res.Decision)
	}

	classifier.Label = "deny"
	res, _, _ = policy.Decide(context.Background(), sess, approval.Request{Tool: "t2"})
	if res.Decision != approval.DecisionDeny {
		t.Errorf("got decision %v, want deny", res.Decision)
	}
}
