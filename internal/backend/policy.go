package backend

import (
	"context"
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
		mode: session.ModeEdit,
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

// AllowCategoryOf sets the policy for the category of the given tool to PolicyAllow.
func (pe *PolicyEngine) AllowCategoryOf(toolName string) {
	pe.mu.Lock()
	defer pe.mu.Unlock()
	if cat, ok := pe.Categories[toolName]; ok {
		pe.Policies[cat] = PolicyAllow
	}
}

// AutoApprove returns whether auto-approval is enabled.
func (pe *PolicyEngine) AutoApprove() bool {
	pe.mu.RLock()
	defer pe.mu.RUnlock()
	return pe.autoApprove
}

// Authorize checks if a tool call is permitted by the policy.
func (pe *PolicyEngine) Authorize(
	ctx context.Context,
	toolName string,
	args string,
) (Policy, string) {
	pe.mu.RLock()
	mode := pe.mode
	auto := pe.autoApprove
	policies := make(map[ToolCategory]Policy)
	for k, v := range pe.Policies {
		policies[k] = v
	}
	pe.mu.RUnlock()

	if auto {
		return PolicyAllow, ""
	}

	category, ok := pe.Categories[toolName]
	if !ok {
		return PolicyAsk, fmt.Sprintf("Unknown tool %q requested.", toolName)
	}

	// Check for category override
	if p, ok := policies[category]; ok && p == PolicyAllow {
		return PolicyAllow, ""
	}

	switch mode {
	case session.ModeRead:
		switch category {
		case CategoryRead:
			return PolicyAllow, ""
		case CategorySensitive:
			return PolicyAsk, fmt.Sprintf("Tool %q requires approval in READ mode.", toolName)
		default:
			return PolicyDeny, fmt.Sprintf("Tool %q is blocked in READ mode.", toolName)
		}

	case session.ModeEdit:
		switch category {
		case CategoryRead:
			return PolicyAllow, ""
		case CategorySensitive:
			return PolicyAsk, fmt.Sprintf("Tool %q requires approval.", toolName)
		default:
			return PolicyAsk, fmt.Sprintf("Tool %q (%s) requires approval.", toolName, category)
		}

	case session.ModeYolo:
		return PolicyAllow, ""
	}

	return PolicyAsk, ""
}
