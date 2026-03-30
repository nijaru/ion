package backend

import (
	"context"
	"encoding/json"
	"fmt"
	"sync"

	"github.com/nijaru/ion/internal/session"
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

	mu          sync.RWMutex
	mode        session.Mode
	autoApprove bool
}

// NewPolicyEngine creates a default policy engine.
func NewPolicyEngine() *PolicyEngine {
	return &PolicyEngine{
		Categories: map[string]ToolCategory{
			"read":            CategoryRead,
			"grep":            CategoryRead,
			"glob":            CategoryRead,
			"list":            CategoryRead,
			"recall_memory":   CategoryRead,
			"remember_memory": CategoryRead,
			"compact":         CategoryRead,
			"write":           CategoryWrite,
			"edit":            CategoryWrite,
			"multi_edit":      CategoryWrite,
			"bash":            CategoryExecute,
			"verify":          CategoryExecute,
			"mcp":             CategorySensitive,
			"subagent":        CategorySensitive,
		},
		Policies: map[ToolCategory]Policy{
			CategoryRead:      PolicyAllow,
			CategoryWrite:     PolicyAsk,
			CategoryExecute:   PolicyAsk,
			CategoryNetwork:   PolicyAsk,
			CategorySensitive: PolicyAsk,
		},
		mode: session.ModeWrite,
	}
}

// SetMode updates the active session mode.
func (pe *PolicyEngine) SetMode(mode session.Mode) {
	pe.mu.Lock()
	defer pe.mu.Unlock()
	pe.mode = mode
}

// SetAutoApprove toggles auto-approval for all tool categories.
// When enabled, Authorize always returns PolicyAllow.
func (pe *PolicyEngine) SetAutoApprove(enabled bool) {
	pe.mu.Lock()
	defer pe.mu.Unlock()
	pe.autoApprove = enabled
}

// AutoApprove returns whether auto-approval is enabled.
func (pe *PolicyEngine) AutoApprove() bool {
	pe.mu.RLock()
	defer pe.mu.RUnlock()
	return pe.autoApprove
}

// Authorize checks if a tool call is permitted by the policy.
func (pe *PolicyEngine) Authorize(ctx context.Context, toolName string, args string) (Policy, string) {
	pe.mu.RLock()
	mode := pe.mode
	auto := pe.autoApprove
	pe.mu.RUnlock()

	if auto {
		return PolicyAllow, ""
	}

	category, ok := pe.Categories[toolName]
	if !ok {
		return PolicyAsk, fmt.Sprintf("Unknown tool %q requested.", toolName)
	}

	policy := pe.Policies[category]

	// Enforce READ mode restrictions
	if mode == session.ModeRead {
		if category == CategoryWrite || category == CategorySensitive {
			return PolicyAsk, fmt.Sprintf("Tool %q is restricted in READ mode and requires explicit approval.", toolName)
		}

		if toolName == "bash" {
			var input struct {
				Command string `json:"command"`
			}
			if err := json.Unmarshal([]byte(args), &input); err == nil {
				if !IsSafeBashCommand(input.Command) {
					return PolicyAsk, fmt.Sprintf("Bash command %q is not considered safe for READ mode and requires explicit approval.", input.Command)
				}
				// Safe bash commands are allowed in READ mode
				return PolicyAllow, ""
			}
		}

		if category == CategoryExecute {
			return PolicyAsk, fmt.Sprintf("Tool %q is restricted in READ mode and requires explicit approval.", toolName)
		}
	}

	reason := ""
	if policy == PolicyAsk {
		reason = fmt.Sprintf("Tool %q belongs to the %q category, which requires approval.", toolName, category)
	} else if policy == PolicyDeny {
		reason = fmt.Sprintf("Tool %q is denied by policy.", toolName)
	}

	return policy, reason
}
