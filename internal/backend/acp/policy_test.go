package acp

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/internal/session"
)

type classifierFunc func(context.Context, PolicyClassification) (PolicyDecision, error)

func (f classifierFunc) ClassifyPolicy(
	ctx context.Context,
	req PolicyClassification,
) (PolicyDecision, error) {
	return f(ctx, req)
}

func TestPolicyEngine(t *testing.T) {
	pe := NewPolicyEngine()
	ctx := context.Background()

	tests := []struct {
		name     string
		mode     session.Mode
		tool     string
		args     string
		expected Policy
	}{
		{
			"EDIT mode: Read tool Allowed",
			session.ModeEdit,
			"read",
			`{"file_path": "file.txt"}`,
			PolicyAllow,
		},
		{
			"EDIT mode: Write tool Asked",
			session.ModeEdit,
			"write",
			`{"file_path": "file.txt"}`,
			PolicyAsk,
		},
		{
			"EDIT mode: Bash tool Asked",
			session.ModeEdit,
			"bash",
			`{"command": "ls -la"}`,
			PolicyAsk,
		},
		{"EDIT mode: Sensitive tool Asked", session.ModeEdit, "mcp", `{}`, PolicyAsk},

		{
			"READ mode: Read tool Allowed",
			session.ModeRead,
			"read",
			`{"file_path": "file.txt"}`,
			PolicyAllow,
		},
		{
			"READ mode: Write tool Denied",
			session.ModeRead,
			"write",
			`{"file_path": "file.txt"}`,
			PolicyDeny,
		},
		{
			"READ mode: Bash tool Denied",
			session.ModeRead,
			"bash",
			`{"command": "ls -la"}`,
			PolicyDeny,
		},
		{"READ mode: Sensitive tool Asked", session.ModeRead, "mcp", `{}`, PolicyAsk},

		{
			"YOLO mode: Read tool Allowed",
			session.ModeYolo,
			"read",
			`{"file_path": "file.txt"}`,
			PolicyAllow,
		},
		{
			"YOLO mode: Write tool Allowed",
			session.ModeYolo,
			"write",
			`{"file_path": "file.txt"}`,
			PolicyAllow,
		},
		{
			"YOLO mode: Bash tool Allowed",
			session.ModeYolo,
			"bash",
			`{"command": "rm -rf /"}`,
			PolicyAllow,
		},
		{"YOLO mode: Sensitive tool Allowed", session.ModeYolo, "mcp", `{}`, PolicyAllow},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			pe.SetMode(tt.mode)
			policy, _ := pe.Authorize(ctx, tt.tool, tt.args)
			if policy != tt.expected {
				t.Errorf(
					"Authorize(%q, mode=%v) = %v; want %v",
					tt.tool,
					tt.mode,
					policy,
					tt.expected,
				)
			}
		})
	}
}

func TestReadModeCannotBeWeakenedBySessionApprovals(t *testing.T) {
	pe := NewPolicyEngine()
	ctx := context.Background()

	pe.SetMode(session.ModeEdit)
	pe.AllowCategoryOf("write")

	policy, _ := pe.Authorize(ctx, "write", `{"file_path":"file.txt"}`)
	if policy != PolicyAllow {
		t.Fatalf("EDIT override policy = %v, want allow", policy)
	}

	pe.SetAutoApprove(true)
	pe.SetMode(session.ModeRead)

	policy, reason := pe.Authorize(ctx, "write", `{"file_path":"file.txt"}`)
	if policy != PolicyDeny {
		t.Fatalf("READ write policy = %v (%q), want deny", policy, reason)
	}
	policy, reason = pe.Authorize(ctx, "bash", `{"command":"go test ./..."}`)
	if policy != PolicyDeny {
		t.Fatalf("READ bash policy = %v (%q), want deny", policy, reason)
	}
}

func TestVisibleToolNamesHidesNonReadToolsInReadMode(t *testing.T) {
	pe := NewPolicyEngine()
	names := []string{
		"bash",
		"edit",
		"glob",
		"grep",
		"list",
		"read",
		"read_skill",
		"unknown",
		"write",
	}

	pe.SetMode(session.ModeRead)
	got := pe.VisibleToolNames(names)
	want := []string{"glob", "grep", "list", "read", "read_skill"}
	if strings.Join(got, ",") != strings.Join(want, ",") {
		t.Fatalf("READ visible tools = %#v, want %#v", got, want)
	}

	pe.SetMode(session.ModeEdit)
	got = pe.VisibleToolNames(names)
	if strings.Join(got, ",") != strings.Join(names, ",") {
		t.Fatalf("EDIT visible tools = %#v, want %#v", got, names)
	}
}

func TestYoloAllowsUnknownTools(t *testing.T) {
	pe := NewPolicyEngine()
	pe.SetMode(session.ModeYolo)

	policy, reason := pe.Authorize(context.Background(), "external_tool", `{}`)
	if policy != PolicyAllow {
		t.Fatalf("YOLO unknown tool policy = %v (%q), want allow", policy, reason)
	}
}

func TestPolicyClassifierCanRefineEditAskCases(t *testing.T) {
	pe := NewPolicyEngine()
	pe.SetMode(session.ModeEdit)
	var audit PolicyAuditEvent
	pe.SetAuditSink(func(event PolicyAuditEvent) {
		audit = event
	})
	pe.SetClassifier(
		classifierFunc(func(ctx context.Context, req PolicyClassification) (PolicyDecision, error) {
			if req.ToolName != "bash" || req.Category != CategoryExecute {
				t.Fatalf("classification = %+v, want bash execute", req)
			}
			return PolicyDecision{Action: PolicyDeny, Reason: "destructive command"}, nil
		}),
		time.Second,
	)

	policy, reason := pe.Authorize(context.Background(), "bash", `{"command":"rm -rf build"}`)
	if policy != PolicyDeny {
		t.Fatalf("policy = %v (%q), want deny", policy, reason)
	}
	if !strings.Contains(reason, "destructive command") {
		t.Fatalf("reason = %q, want classifier reason", reason)
	}
	if audit.ToolName != "bash" || audit.Category != CategoryExecute ||
		audit.Action != PolicyDeny ||
		audit.Source != "classifier" {
		t.Fatalf("audit = %+v, want classifier deny event", audit)
	}
}

func TestPolicyClassifierFailuresFailClosedToAsk(t *testing.T) {
	pe := NewPolicyEngine()
	pe.SetMode(session.ModeEdit)
	pe.SetClassifier(
		classifierFunc(func(ctx context.Context, req PolicyClassification) (PolicyDecision, error) {
			return PolicyDecision{}, errors.New("model unavailable")
		}),
		time.Second,
	)

	policy, reason := pe.Authorize(context.Background(), "write", `{"file_path":"file.txt"}`)
	if policy != PolicyAsk {
		t.Fatalf("policy = %v (%q), want ask", policy, reason)
	}
	if !strings.Contains(reason, "Classifier unavailable") {
		t.Fatalf("reason = %q, want classifier failure", reason)
	}
}

func TestPolicyClassifierTimeoutFailsClosedToAsk(t *testing.T) {
	pe := NewPolicyEngine()
	pe.SetMode(session.ModeEdit)
	pe.SetClassifier(
		classifierFunc(func(ctx context.Context, req PolicyClassification) (PolicyDecision, error) {
			<-ctx.Done()
			return PolicyDecision{}, ctx.Err()
		}),
		time.Nanosecond,
	)

	policy, reason := pe.Authorize(t.Context(), "write", `{"file_path":"file.txt"}`)
	if policy != PolicyAsk {
		t.Fatalf("policy = %v (%q), want ask", policy, reason)
	}
	if !strings.Contains(reason, "Classifier unavailable") {
		t.Fatalf("reason = %q, want classifier timeout failure", reason)
	}
}

func TestPolicyClassifierInvalidDecisionFailsClosedToAsk(t *testing.T) {
	pe := NewPolicyEngine()
	pe.SetMode(session.ModeEdit)
	pe.SetClassifier(
		classifierFunc(func(ctx context.Context, req PolicyClassification) (PolicyDecision, error) {
			return PolicyDecision{Action: "maybe", Reason: "invalid parse"}, nil
		}),
		time.Second,
	)

	policy, reason := pe.Authorize(t.Context(), "write", `{"file_path":"file.txt"}`)
	if policy != PolicyAsk {
		t.Fatalf("policy = %v (%q), want ask", policy, reason)
	}
	if !strings.Contains(reason, "invalid action") {
		t.Fatalf("reason = %q, want invalid action failure", reason)
	}
}

func TestPolicyClassifierCannotWeakenHardBoundaries(t *testing.T) {
	pe := NewPolicyEngine()
	pe.SetClassifier(
		classifierFunc(func(ctx context.Context, req PolicyClassification) (PolicyDecision, error) {
			return PolicyDecision{Action: PolicyAllow, Reason: "looks safe"}, nil
		}),
		time.Second,
	)

	pe.SetMode(session.ModeRead)
	policy, reason := pe.Authorize(context.Background(), "write", `{"file_path":"file.txt"}`)
	if policy != PolicyDeny {
		t.Fatalf("READ write policy = %v (%q), want deny", policy, reason)
	}

	pe.SetMode(session.ModeEdit)
	policy, reason = pe.Authorize(context.Background(), "read", `{"file_path":"file.txt"}`)
	if policy != PolicyAllow {
		t.Fatalf("read policy = %v (%q), want allow", policy, reason)
	}
}
