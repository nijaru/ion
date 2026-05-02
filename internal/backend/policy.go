package backend

import (
	"context"
	"fmt"
	"os"
	"strings"
	"sync"
	"time"

	"gopkg.in/yaml.v3"

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
	// ToolPolicies maps exact tool names to explicit handling.
	ToolPolicies map[string]Policy

	mu                sync.RWMutex
	mode              session.Mode
	autoApprove       bool
	classifier        PolicyClassifier
	classifierTimeout time.Duration
	auditSink         PolicyAuditSink
}

// NewPolicyEngine creates a default policy engine.
func NewPolicyEngine() *PolicyEngine {
	return &PolicyEngine{
		Categories: map[string]ToolCategory{
			"read":            CategoryRead,
			"read_skill":      CategoryRead,
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
		Policies:          defaultCategoryPolicies(),
		ToolPolicies:      map[string]Policy{},
		mode:              session.ModeEdit,
		classifierTimeout: 2 * time.Second,
	}
}

type PolicyConfig struct {
	Rules []PolicyRule `yaml:"rules"`
}

type PolicyRule struct {
	Tool     string       `yaml:"tool"`
	Category ToolCategory `yaml:"category"`
	Action   Policy       `yaml:"action"`
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

func LoadPolicyConfig(path string) (*PolicyConfig, error) {
	if path == "" {
		return nil, nil
	}
	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, err
	}
	var cfg PolicyConfig
	if err := yaml.Unmarshal(data, &cfg); err != nil {
		return nil, fmt.Errorf("parse policy config: %w", err)
	}
	if err := cfg.Validate(); err != nil {
		return nil, err
	}
	return &cfg, nil
}

func (cfg *PolicyConfig) Validate() error {
	if cfg == nil {
		return nil
	}
	for i, rule := range cfg.Rules {
		if rule.Tool == "" && rule.Category == "" {
			return fmt.Errorf("policy rule %d: tool or category is required", i)
		}
		if rule.Tool != "" && rule.Category != "" {
			return fmt.Errorf("policy rule %d: specify only one of tool or category", i)
		}
		if rule.Category != "" && !validCategory(rule.Category) {
			return fmt.Errorf("policy rule %d: invalid category %q", i, rule.Category)
		}
		if !validPolicy(rule.Action) {
			return fmt.Errorf("policy rule %d: invalid action %q", i, rule.Action)
		}
	}
	return nil
}

func (pe *PolicyEngine) ApplyConfig(cfg *PolicyConfig) {
	pe.mu.Lock()
	defer pe.mu.Unlock()
	pe.Policies = defaultCategoryPolicies()
	pe.ToolPolicies = map[string]Policy{}
	if cfg == nil {
		return
	}
	for _, rule := range cfg.Rules {
		if rule.Tool != "" {
			pe.ToolPolicies[rule.Tool] = rule.Action
			continue
		}
		pe.Policies[rule.Category] = rule.Action
	}
}

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

// VisibleToolNames filters registered tool names to the active mode's
// model-visible surface.
func (pe *PolicyEngine) VisibleToolNames(names []string) []string {
	pe.mu.RLock()
	defer pe.mu.RUnlock()

	visible := make([]string, 0, len(names))
	for _, name := range names {
		if pe.mode == session.ModeRead {
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
	toolPolicies := make(map[string]Policy)
	for k, v := range pe.ToolPolicies {
		toolPolicies[k] = v
	}
	classifier := pe.classifier
	classifierTimeout := pe.classifierTimeout
	auditSink := pe.auditSink
	pe.mu.RUnlock()

	category, ok := pe.Categories[toolName]

	switch mode {
	case session.ModeRead:
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

	case session.ModeEdit:
		if auto {
			return PolicyAllow, ""
		}
		if !ok {
			return PolicyAsk, fmt.Sprintf("Unknown tool %q requested.", toolName)
		}
		if category == CategoryRead {
			return PolicyAllow, ""
		}
		if p, ok := toolPolicies[toolName]; ok {
			return p, fmt.Sprintf("Tool %q policy is %s.", toolName, p)
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

	case session.ModeYolo:
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

func validCategory(category ToolCategory) bool {
	switch category {
	case CategoryRead, CategoryWrite, CategoryExecute, CategoryNetwork, CategorySensitive:
		return true
	default:
		return false
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
