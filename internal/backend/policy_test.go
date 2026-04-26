package backend

import (
	"context"
	"os"
	"path/filepath"
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

func TestPolicyConfigAppliesEditModeRules(t *testing.T) {
	pe := NewPolicyEngine()
	pe.ApplyConfig(&PolicyConfig{Rules: []PolicyRule{
		{Tool: "bash", Action: PolicyDeny},
		{Category: CategoryWrite, Action: PolicyAllow},
	}})
	pe.SetMode(session.ModeEdit)

	policy, reason := pe.Authorize(context.Background(), "bash", `{"command":"go test ./..."}`)
	if policy != PolicyDeny {
		t.Fatalf("bash policy = %v (%q), want deny", policy, reason)
	}
	policy, reason = pe.Authorize(context.Background(), "write", `{"file_path":"file.txt"}`)
	if policy != PolicyAllow {
		t.Fatalf("write policy = %v (%q), want allow", policy, reason)
	}
}

func TestPolicyConfigCannotWeakenReadMode(t *testing.T) {
	pe := NewPolicyEngine()
	pe.ApplyConfig(&PolicyConfig{Rules: []PolicyRule{
		{Tool: "bash", Action: PolicyAllow},
		{Category: CategoryWrite, Action: PolicyAllow},
	}})
	pe.SetMode(session.ModeRead)

	for _, toolName := range []string{"bash", "write"} {
		policy, reason := pe.Authorize(context.Background(), toolName, `{}`)
		if policy != PolicyDeny {
			t.Fatalf("%s READ policy = %v (%q), want deny", toolName, policy, reason)
		}
	}
}

func TestPolicyConfigCannotGateReadTools(t *testing.T) {
	pe := NewPolicyEngine()
	pe.ApplyConfig(&PolicyConfig{Rules: []PolicyRule{
		{Tool: "read", Action: PolicyDeny},
		{Category: CategoryRead, Action: PolicyDeny},
	}})
	pe.SetMode(session.ModeEdit)

	policy, reason := pe.Authorize(context.Background(), "read", `{"file_path":"file.txt"}`)
	if policy != PolicyAllow {
		t.Fatalf("read EDIT policy = %v (%q), want allow", policy, reason)
	}
}

func TestPolicyConfigCanDenyCategories(t *testing.T) {
	pe := NewPolicyEngine()
	pe.ApplyConfig(&PolicyConfig{Rules: []PolicyRule{
		{Category: CategoryWrite, Action: PolicyDeny},
		{Category: CategorySensitive, Action: PolicyDeny},
	}})
	pe.SetMode(session.ModeEdit)

	for _, toolName := range []string{"write", "mcp"} {
		policy, reason := pe.Authorize(context.Background(), toolName, `{}`)
		if policy != PolicyDeny {
			t.Fatalf("%s EDIT policy = %v (%q), want deny", toolName, policy, reason)
		}
	}
}

func TestLoadPolicyConfig(t *testing.T) {
	path := filepath.Join(t.TempDir(), "policy.yaml")
	if err := os.WriteFile(path, []byte(`rules:
  - tool: bash
    action: deny
  - category: write
    action: allow
`), 0o600); err != nil {
		t.Fatalf("write policy config: %v", err)
	}

	cfg, err := LoadPolicyConfig(path)
	if err != nil {
		t.Fatalf("LoadPolicyConfig returned error: %v", err)
	}
	if len(cfg.Rules) != 2 {
		t.Fatalf("rules len = %d, want 2", len(cfg.Rules))
	}
	if cfg.Rules[0].Tool != "bash" || cfg.Rules[0].Action != PolicyDeny {
		t.Fatalf("first rule = %+v", cfg.Rules[0])
	}
	if cfg.Rules[1].Category != CategoryWrite || cfg.Rules[1].Action != PolicyAllow {
		t.Fatalf("second rule = %+v", cfg.Rules[1])
	}
}

func TestPolicyConfigRejectsInvalidRules(t *testing.T) {
	cases := []struct {
		name string
		cfg  *PolicyConfig
	}{
		{name: "missing selector", cfg: &PolicyConfig{Rules: []PolicyRule{{Action: PolicyDeny}}}},
		{name: "two selectors", cfg: &PolicyConfig{Rules: []PolicyRule{{Tool: "bash", Category: CategoryExecute, Action: PolicyDeny}}}},
		{name: "bad action", cfg: &PolicyConfig{Rules: []PolicyRule{{Tool: "bash", Action: "sometimes"}}}},
		{name: "bad category", cfg: &PolicyConfig{Rules: []PolicyRule{{Category: "filesystem", Action: PolicyAsk}}}},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if err := tc.cfg.Validate(); err == nil {
				t.Fatal("Validate returned nil error")
			}
		})
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
