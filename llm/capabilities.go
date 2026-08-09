package llm

import (
	"slices"
	"strings"
)

// Capabilities describes what features a model supports.
// The pipeline uses these to adapt requests before they reach the provider.
type Capabilities struct {
	// Streaming indicates the model supports token-by-token streaming.
	Streaming bool
	// Tools indicates the model supports tool/function calling.
	Tools bool
	// Temperature indicates the model accepts a temperature parameter.
	// Models with internal fixed-temperature reasoning should set this to false.
	Temperature bool
	// SystemRole is the role to use when passing system-level instructions.
	// RoleSystem (default) passes them through unchanged.
	// RoleUser means the model has no system role; Capabilities injects
	// system content as user messages with an "Instructions:" prefix.
	// RoleDeveloper means the model accepts a privileged instruction channel
	// distinct from the assistant conversation.
	SystemRole Role
	// InputModalities lists known input types such as "text" and "image".
	// An empty list means the provider has not supplied modality metadata and
	// preserves the historical permissive behavior.
	InputModalities []string
	// Reasoning describes typed reasoning controls accepted by the model.
	Reasoning ReasoningCapabilities
}

type ReasoningKind string

const (
	ReasoningKindNone    ReasoningKind = ""
	ReasoningKindEffort  ReasoningKind = "effort"
	ReasoningKindBudget  ReasoningKind = "budget"
	ReasoningKindBoolean ReasoningKind = "boolean"
)

type ReasoningCapabilities struct {
	Kind                ReasoningKind
	Efforts             []string
	CanDisable          bool
	BudgetMinTokens     int
	BudgetMaxTokens     int
	BudgetDefaultTokens int
}

const (
	defaultThinkingBudgetMinTokens = 1024
	defaultThinkingBudgetTokens    = 4096
	defaultThinkingBudgetMaxTokens = 32768
)

// DefaultCapabilities returns full capabilities — suitable for most chat models.
func DefaultCapabilities() Capabilities {
	return Capabilities{
		Streaming:   true,
		Tools:       true,
		Temperature: false, // Match Pi: temperature is opt-in, not default
		SystemRole:  RoleSystem,
	}
}

// SupportsImages reports whether image parts may be sent to the model.
// Unknown modality metadata remains permissive so existing providers do not
// silently lose user input; an explicit text-only list is restrictive.
func (c Capabilities) SupportsImages() bool {
	if len(c.InputModalities) == 0 {
		return true
	}
	for _, modality := range c.InputModalities {
		if strings.EqualFold(strings.TrimSpace(modality), "image") {
			return true
		}
	}
	return false
}

func (c Capabilities) ReasoningCaps() ReasoningCapabilities {
	return c.Reasoning
}

func (c Capabilities) SupportsReasoningEffort(effort string) bool {
	effort = strings.ToLower(strings.TrimSpace(effort))
	if effort == "" {
		return false
	}
	caps := c.ReasoningCaps()
	if caps.Kind != ReasoningKindEffort {
		return false
	}
	if effort == "off" || effort == "none" || effort == "disabled" {
		return caps.CanDisable
	}
	if len(caps.Efforts) == 0 {
		return true
	}
	return slices.Contains(caps.Efforts, effort)
}

func (c Capabilities) SupportsReasoningControl(value string) bool {
	if c.SupportsThinking() {
		value = strings.ToLower(strings.TrimSpace(value))
		if value == "off" || value == "none" || value == "disabled" {
			return c.Reasoning.CanDisable
		}
		return c.ThinkingBudgetForEffort(value) > 0
	}
	return c.SupportsReasoningEffort(value) || c.SupportsReasoningToggle(value)
}

func (c Capabilities) SupportsReasoningToggle(value string) bool {
	value = strings.ToLower(strings.TrimSpace(value))
	if value == "" {
		return false
	}
	caps := c.ReasoningCaps()
	if caps.Kind != ReasoningKindBoolean {
		return false
	}
	if value == "off" || value == "none" || value == "disabled" {
		return caps.CanDisable
	}
	return true
}

func (c Capabilities) SupportsThinking() bool {
	return c.ReasoningCaps().Kind == ReasoningKindBudget
}

func (c Capabilities) SupportsThinkingBudget(tokens int) bool {
	if tokens <= 0 {
		return false
	}
	caps := c.ReasoningCaps()
	if caps.Kind != ReasoningKindBudget {
		return false
	}
	if caps.BudgetMinTokens > 0 && tokens < caps.BudgetMinTokens {
		return false
	}
	if caps.BudgetMaxTokens > 0 && tokens > caps.BudgetMaxTokens {
		return false
	}
	return true
}

// ThinkingBudgetForEffort maps Ion's user-facing reasoning levels to the
// token budget used by providers whose native control is a thinking budget.
// An explicit provider-native budget remains authoritative; this mapping only
// adapts the shared effort control at the provider boundary.
func (c Capabilities) ThinkingBudgetForEffort(effort string) int {
	if !c.SupportsThinking() {
		return 0
	}

	effort = strings.ToLower(strings.TrimSpace(effort))
	if effort == "" || effort == "auto" || effort == "off" || effort == "none" || effort == "disabled" {
		return 0
	}

	min := c.Reasoning.BudgetMinTokens
	if min <= 0 {
		min = defaultThinkingBudgetMinTokens
	}
	defaultBudget := c.Reasoning.BudgetDefaultTokens
	if defaultBudget < min {
		defaultBudget = defaultThinkingBudgetTokens
	}
	max := c.Reasoning.BudgetMaxTokens
	if max <= 0 {
		max = defaultThinkingBudgetMaxTokens
	}

	var budget int
	switch effort {
	case "minimal":
		budget = min
	case "low":
		budget = defaultBudget / 2
	case "medium":
		budget = defaultBudget
	case "high":
		budget = defaultBudget * 2
	case "xhigh", "max":
		budget = defaultBudget * 4
	default:
		return 0
	}

	if budget < min {
		budget = min
	}
	if budget > max {
		budget = max
	}
	return budget
}
