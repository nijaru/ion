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
		{"Write mode: Read tool Allowed", session.ModeWrite, "read", `{"file_path": "file.txt"}`, PolicyAllow},
		{"Write mode: Write tool Asked", session.ModeWrite, "write", `{"file_path": "file.txt"}`, PolicyAsk},
		{"Write mode: Bash tool Asked", session.ModeWrite, "bash", `{"command": "ls -la"}`, PolicyAsk},

		{"Read mode: Read tool Allowed", session.ModeRead, "read", `{"file_path": "file.txt"}`, PolicyAllow},
		{"Read mode: Write tool Asked (Restricted)", session.ModeRead, "write", `{"file_path": "file.txt"}`, PolicyAsk},
		{"Read mode: Safe Bash Allowed", session.ModeRead, "bash", `{"command": "ls -la"}`, PolicyAllow},
		{"Read mode: Unsafe Bash Asked", session.ModeRead, "bash", `{"command": "rm -rf /"}`, PolicyAsk},
		{"Read mode: Complex Safe Bash Allowed", session.ModeRead, "bash", `{"command": "git status && ls"}`, PolicyAllow},
		{"Read mode: Complex Unsafe Bash Asked", session.ModeRead, "bash", `{"command": "ls && rm -rf /"}`, PolicyAsk},
		{"Read mode: Redirection Asked", session.ModeRead, "bash", `{"command": "ls > out.txt"}`, PolicyAsk},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			pe.SetMode(tt.mode)
			policy, _ := pe.Authorize(ctx, tt.tool, tt.args)
			if policy != tt.expected {
				t.Errorf("Authorize(%q, mode=%v) = %v; want %v", tt.tool, tt.mode, policy, tt.expected)
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
