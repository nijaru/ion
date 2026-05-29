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

// DefaultCapabilities returns full capabilities — suitable for most chat models.
func DefaultCapabilities() Capabilities {
	return Capabilities{
		Streaming:   true,
		Tools:       true,
		Temperature: false, // Match Pi: temperature is opt-in, not default
		SystemRole:  RoleSystem,
	}
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
