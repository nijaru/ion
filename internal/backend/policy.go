package backend

import (
	"context"
	"fmt"
)

// Policy defines how a tool call should be handled.
type Policy string

const (
	PolicyAllow Policy = "allow"
	PolicyDeny  Policy = "deny"
	PolicyAsk   Policy = "ask"
)

// ToolCategory groups tools by their risk level.
type ToolCategory string

const (
	CategoryRead      ToolCategory = "read"
	CategoryWrite     ToolCategory = "write"
	CategoryExecute   ToolCategory = "execute"
	CategoryNetwork   ToolCategory = "network"
	CategorySensitive ToolCategory = "sensitive"
)

// PolicyEngine manages the approval logic for tool calls.
type PolicyEngine struct {
	// Categories maps tool names to their categories.
	Categories map[string]ToolCategory
	// Policies maps categories to their default handling.
	Policies map[ToolCategory]Policy
}

// NewPolicyEngine creates a default policy engine.
func NewPolicyEngine() *PolicyEngine {
	return &PolicyEngine{
		Categories: map[string]ToolCategory{
			"read":           CategoryRead,
			"search":         CategoryRead,
			"list":           CategoryRead,
			"task":           CategoryRead,
			"write":          CategoryWrite,
			"edit":           CategoryWrite,
			"multi_edit":     CategoryWrite,
			"bash":           CategoryExecute,
			"verify":         CategoryExecute,
			"mcp":            CategorySensitive,
			"subagent":       CategorySensitive,
		},
		Policies: map[ToolCategory]Policy{
			CategoryRead:      PolicyAllow,
			CategoryWrite:     PolicyAsk,
			CategoryExecute:   PolicyAsk,
			CategoryNetwork:   PolicyAsk,
			CategorySensitive: PolicyAsk,
		},
	}
}

// Authorize checks if a tool call is permitted by the policy.
func (pe *PolicyEngine) Authorize(ctx context.Context, toolName string, args string) (Policy, string) {
	category, ok := pe.Categories[toolName]
	if !ok {
		// Default to Ask for unknown tools
		return PolicyAsk, fmt.Sprintf("Unknown tool %q requested.", toolName)
	}

	policy := pe.Policies[category]
	reason := ""
	if policy == PolicyAsk {
		reason = fmt.Sprintf("Tool %q belongs to the %q category, which requires approval.", toolName, category)
	} else if policy == PolicyDeny {
		reason = fmt.Sprintf("Tool %q is denied by policy.", toolName)
	}

	return policy, reason
}
