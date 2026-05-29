package safety_test

import (
	"bytes"
	"context"
	"strings"
	"testing"

	"github.com/go-json-experiment/json"

	"github.com/nijaru/ion/approval"
	"github.com/nijaru/ion/audit"
	"github.com/nijaru/ion/safety"
	"github.com/nijaru/ion/session"
)

func TestPolicy_LogsAuditEvents(t *testing.T) {
	sess := session.New("test")
	var buf strings.Builder
	policy := safety.NewConfig(safety.ModeRead).WithAuditLogger(audit.NewStreamLogger(&buf))

	res, handled, err := policy.Decide(context.Background(), sess, approval.Request{
		SessionID: "s-1",
		Tool:      "bash",
		Category:  string(safety.CategoryWrite),
		Operation: "write_file",
		Resource:  "secrets.txt",
	})
	if err != nil {
		t.Fatalf("Decide: %v", err)
	}
	if !handled {
		t.Fatal("expected handled policy decision")
	}
	if res.Decision != approval.DecisionDeny {
		t.Fatalf("decision = %v, want deny", res.Decision)
	}

	lines := strings.Split(strings.TrimSpace(buf.String()), "\n")
	if len(lines) != 1 {
		t.Fatalf("expected 1 audit line, got %d", len(lines))
	}

	var event audit.Event
	if err := json.Unmarshal([]byte(lines[0]), &event); err != nil {
		t.Fatalf("decode audit event: %v", err)
	}
	if event.Kind != audit.KindPolicyDenied {
		t.Fatalf("event kind = %q, want %q", event.Kind, audit.KindPolicyDenied)
	}
	if event.Decision != string(approval.DecisionDeny) {
		t.Fatalf("decision = %q, want deny", event.Decision)
	}
}

func TestProtectedPathsWithAudit_LogsBlockedPaths(t *testing.T) {
	sess := session.New("test")
	var buf bytes.Buffer
	autoPolicy := approval.PolicyFunc(
		func(ctx context.Context, sess *session.Session, req approval.Request) (approval.Result, bool, error) {
			return approval.Result{Decision: approval.DecisionAllow}, true, nil
		},
	)

	protected := safety.ProtectedPathsWithAudit(
		autoPolicy,
		[]string{".git", ".env"},
		audit.NewStreamLogger(&buf),
	)

	res, handled, err := protected.Decide(context.Background(), sess, approval.Request{
		SessionID: "s-2",
		Tool:      "bash",
		Category:  string(safety.CategoryWrite),
		Operation: "write_file",
		Resource:  ".env",
	})
	if err != nil {
		t.Fatalf("Decide: %v", err)
	}
	if handled {
		t.Fatal("expected protected path to defer to manual approval")
	}
	if res.Decision != "" {
		t.Fatalf("decision = %v, want empty", res.Decision)
	}

	lines := strings.Split(strings.TrimSpace(buf.String()), "\n")
	if len(lines) != 1 {
		t.Fatalf("expected 1 audit line, got %d", len(lines))
	}

	var event audit.Event
	if err := json.Unmarshal([]byte(lines[0]), &event); err != nil {
		t.Fatalf("decode audit event: %v", err)
	}
	if event.Kind != audit.KindProtectedPathBlocked {
		t.Fatalf("event kind = %q, want %q", event.Kind, audit.KindProtectedPathBlocked)
	}
	if event.Resource != ".env" {
		t.Fatalf("resource = %q, want .env", event.Resource)
	}
}
