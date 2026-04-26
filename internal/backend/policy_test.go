package backend

import (
	"context"
	"testing"

	"github.com/nijaru/ion/internal/session"
)

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

func TestYoloAllowsUnknownTools(t *testing.T) {
	pe := NewPolicyEngine()
	pe.SetMode(session.ModeYolo)

	policy, reason := pe.Authorize(context.Background(), "external_tool", `{}`)
	if policy != PolicyAllow {
		t.Fatalf("YOLO unknown tool policy = %v (%q), want allow", policy, reason)
	}
}

func TestIsSafeBashCommand(t *testing.T) {
	tests := []struct {
		command string
		safe    bool
	}{
		{"ls -la", true},
		{"git status", true},
		{"git log --oneline", true},
		{"cat file.txt | grep foo", true},
		{"ls && pwd", true},
		{"rm -rf /", false},
		{"git commit -m 'test'", false},
		{"ls > out.txt", false},
		{"echo $(rm -rf /)", false},
		{"sudo ls", false},
		{"", false},
	}

	for _, tt := range tests {
		t.Run(tt.command, func(t *testing.T) {
			if got := IsSafeBashCommand(tt.command); got != tt.safe {
				t.Errorf("IsSafeBashCommand(%q) = %v; want %v", tt.command, got, tt.safe)
			}
		})
	}
}
