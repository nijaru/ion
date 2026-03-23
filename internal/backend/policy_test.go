package backend

import (
	"context"
	"testing"
)

func TestPolicyEngine(t *testing.T) {
	pe := NewPolicyEngine()
	ctx := context.Background()

	tests := []struct {
		name     string
		tool     string
		args     string
		expected Policy
	}{
		{"Read tool should be Allowed", "read", "file.txt", PolicyAllow},
		{"List tool should be Allowed", "list", ".", PolicyAllow},
		{"Write tool should be Asked", "write", "file.txt", PolicyAsk},
		{"Edit tool should be Asked", "edit", "file.txt", PolicyAsk},
		{"Bash tool should be Asked", "bash", "ls -la", PolicyAsk},
		{"MCP tool should be Asked", "mcp", "...", PolicyAsk},
		{"Unknown tool should be Asked", "unknown", "...", PolicyAsk},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			policy, _ := pe.Authorize(ctx, tt.tool, tt.args)
			if policy != tt.expected {
				t.Errorf("Authorize(%q) = %v; want %v", tt.tool, policy, tt.expected)
			}
		})
	}
}
