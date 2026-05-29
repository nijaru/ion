package acp

import (
	"context"
	"fmt"
	"strings"
	"sync"
	"time"

	"github.com/nijaru/ion/internal/apperrors"
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

// PolicyEngine manages ACP permission policy decisions.
type PolicyEngine struct {
	// Categories maps tool names to their categories.
	Categories map[string]ToolCategory
	// Policies maps categories to their default handling.
	Policies map[ToolCategory]Policy

	mu                sync.RWMutex
	mode              Mode
	autoApprove       bool
	classifier        PolicyClassifier
	classifierTimeout time.Duration
	auditSink         PolicyAuditSink
}

// NewPolicyEngine creates a default ACP permission policy engine.
func NewPolicyEngine() *PolicyEngine {
	return &PolicyEngine{
		Categories: map[string]ToolCategory{
			"read":       CategoryRead,
			"read_skill": CategoryRead,
			"grep":       CategoryRead,
			"find":       CategoryRead,
			"ls":         CategoryRead,
			"write":      CategoryWrite,
			"edit":       CategoryWrite,
			"bash":       CategoryExecute,
			"mcp":        CategorySensitive,
			"subagent":   CategorySensitive,
		},
		Policies:          defaultCategoryPolicies(),
		mode:              ModeEdit,
		classifierTimeout: 2 * time.Second,
	}
}

type PolicyClassification struct {
	ToolName string
	Args     string
	Category ToolCategory
}

type PolicyDecision struct {
	Action Policy
	Reason string
}

type PolicyClassifier interface {
	ClassifyPolicy(context.Context, PolicyClassification) (PolicyDecision, error)
}

type PolicyAuditEvent struct {
	ToolName string
	Category ToolCategory
	Action   Policy
	Reason   string
	Source   string
}

type PolicyAuditSink func(PolicyAuditEvent)

func (pe *PolicyEngine) SetClassifier(classifier PolicyClassifier, timeout time.Duration) {
	pe.mu.Lock()
	defer pe.mu.Unlock()
	pe.classifier = classifier
	if timeout > 0 {
		pe.classifierTimeout = timeout
	}
}

func (pe *PolicyEngine) SetAuditSink(sink PolicyAuditSink) {
	pe.mu.Lock()
	defer pe.mu.Unlock()
	pe.auditSink = sink
}

// SetMode updates the active session mode.
func (pe *PolicyEngine) SetMode(mode Mode) {
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

// VisibleToolNames filters registered tool names to the active mode's
// model-visible surface.
func (pe *PolicyEngine) VisibleToolNames(names []string) []string {
	pe.mu.RLock()
	defer pe.mu.RUnlock()

	visible := make([]string, 0, len(names))
	for _, name := range names {
		if pe.mode == ModeRead {
			if pe.Categories[name] != CategoryRead {
				continue
			}
		}
		visible = append(visible, name)
	}
	return visible
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
	classifier := pe.classifier
	classifierTimeout := pe.classifierTimeout
	auditSink := pe.auditSink
	pe.mu.RUnlock()

	category, ok := pe.Categories[toolName]

	switch mode {
	case ModeRead:
		if !ok {
			return PolicyAsk, fmt.Sprintf("Unknown tool %q requested.", toolName)
		}
		switch category {
		case CategoryRead:
			return PolicyAllow, ""
		case CategorySensitive:
			return PolicyAsk, fmt.Sprintf("Tool %q requires approval in READ mode.", toolName)
		default:
			return PolicyDeny, fmt.Sprintf("Tool %q is blocked in READ mode.", toolName)
		}

	case ModeEdit:
		if auto {
			return PolicyAllow, ""
		}
		if !ok {
			return PolicyAsk, fmt.Sprintf("Unknown tool %q requested.", toolName)
		}
		if category == CategoryRead {
			return PolicyAllow, ""
		}
		if p, ok := policies[category]; ok {
			switch p {
			case PolicyAllow:
				return PolicyAllow, ""
			case PolicyDeny:
				return PolicyDeny, fmt.Sprintf(
					"Tool %q (%s) is blocked by policy.",
					toolName,
					category,
				)
			case PolicyAsk:
				return pe.classifyOrAsk(
					ctx,
					classifier,
					classifierTimeout,
					auditSink,
					toolName,
					args,
					category,
				)
			}
		}
		return pe.classifyOrAsk(
			ctx,
			classifier,
			classifierTimeout,
			auditSink,
			toolName,
			args,
			category,
		)

	case ModeYolo:
		return PolicyAllow, ""
	}

	return PolicyAsk, ""
}

func (pe *PolicyEngine) classifyOrAsk(
	ctx context.Context,
	classifier PolicyClassifier,
	timeout time.Duration,
	auditSink PolicyAuditSink,
	toolName string,
	args string,
	category ToolCategory,
) (Policy, string) {
	defaultReason := fmt.Sprintf("Tool %q (%s) requires approval.", toolName, category)
	if classifier == nil {
		return PolicyAsk, defaultReason
	}
	if timeout <= 0 {
		timeout = 2 * time.Second
	}
	classifyCtx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	decision, err := classifier.ClassifyPolicy(classifyCtx, PolicyClassification{
		ToolName: toolName,
		Args:     args,
		Category: category,
	})
	if err != nil {
		err = apperrors.WrapContext("classify ACP policy", err)
		reason := defaultReason + " Classifier unavailable: " + err.Error()
		auditPolicyDecision(
			auditSink,
			toolName,
			category,
			PolicyAsk,
			reason,
			"classifier_unavailable",
		)
		return PolicyAsk, reason
	}
	if !validPolicy(decision.Action) {
		reason := defaultReason + " Classifier returned invalid action."
		auditPolicyDecision(auditSink, toolName, category, PolicyAsk, reason, "classifier_invalid")
		return PolicyAsk, reason
	}
	reason := strings.TrimSpace(decision.Reason)
	if reason == "" {
		reason = "classifier decision"
	}
	switch decision.Action {
	case PolicyAllow:
		reason = "Classifier allowed: " + reason
		auditPolicyDecision(auditSink, toolName, category, PolicyAllow, reason, "classifier")
		return PolicyAllow, reason
	case PolicyDeny:
		reason = "Classifier denied: " + reason
		auditPolicyDecision(auditSink, toolName, category, PolicyDeny, reason, "classifier")
		return PolicyDeny, reason
	default:
		reason = "Classifier requested approval: " + reason
		auditPolicyDecision(auditSink, toolName, category, PolicyAsk, reason, "classifier")
		return PolicyAsk, reason
	}
}

func auditPolicyDecision(
	sink PolicyAuditSink,
	toolName string,
	category ToolCategory,
	action Policy,
	reason string,
	source string,
) {
	if sink == nil {
		return
	}
	sink(PolicyAuditEvent{
		ToolName: toolName,
		Category: category,
		Action:   action,
		Reason:   reason,
		Source:   source,
	})
}

func defaultCategoryPolicies() map[ToolCategory]Policy {
	return map[ToolCategory]Policy{
		CategoryRead:      PolicyAllow,
		CategoryWrite:     PolicyAsk,
		CategoryExecute:   PolicyAsk,
		CategoryNetwork:   PolicyAsk,
		CategorySensitive: PolicyAsk,
	}
}

func validPolicy(policy Policy) bool {
	switch policy {
	case PolicyAllow, PolicyAsk, PolicyDeny:
		return true
	default:
		return false
	}
}
