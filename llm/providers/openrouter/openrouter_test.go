package openrouter

import (
	"encoding/json"
	"testing"

	"github.com/nijaru/ion/llm"
)

func TestNewProviderDefaults(t *testing.T) {
	p := NewProvider(llm.ProviderConfig{})

	if got, want := p.ID(), "openrouter"; got != want {
		t.Fatalf("ID = %q, want %q", got, want)
	}
	if got, want := p.Config.APIEndpoint, "https://openrouter.ai/api/v1"; got != want {
		t.Fatalf("endpoint = %q, want %q", got, want)
	}
}

func TestNewProviderRespectsConfig(t *testing.T) {
	p := NewProvider(llm.ProviderConfig{
		ID:          "openrouter-custom",
		APIEndpoint: "https://example.test/openrouter",
	})

	if got, want := p.ID(), "openrouter-custom"; got != want {
		t.Fatalf("ID = %q, want %q", got, want)
	}
	if got, want := p.Config.APIEndpoint, "https://example.test/openrouter"; got != want {
		t.Fatalf("endpoint = %q, want %q", got, want)
	}
}

func TestBuildRequestJSON_NestedReasoningFormat(t *testing.T) {
	// Register a reasoning model.
	llm.ClearRegistry()
	llm.RegisterModel(llm.ModelDef{
		Pattern: "*mimo*",
		Preset:  llm.PresetReasoning,
	})

	p := NewProvider(llm.ProviderConfig{
		APIKey: "test-key",
		Models: []llm.Model{{
			ID: "xiaomi/mimo-v2.5-pro",
			Capabilities: &llm.Capabilities{
				Streaming:   true,
				Tools:       true,
				Temperature: false,
				SystemRole:  llm.RoleSystem,
				Reasoning: llm.ReasoningCapabilities{
					Kind:       llm.ReasoningKindEffort,
					Efforts:    []string{"minimal", "low", "medium", "high"},
					CanDisable: true,
				},
			},
		}},
	})

	req := &llm.Request{
		Model:           "xiaomi/mimo-v2.5-pro",
		Messages:        []llm.Message{{Role: llm.RoleUser, Content: "hello"}},
		ReasoningEffort: "medium",
	}

	body, err := p.buildRequestJSON(req)
	if err != nil {
		t.Fatalf("buildRequestJSON: %v", err)
	}

	// Parse the raw JSON to verify the reasoning object is nested, not top-level.
	var raw map[string]any
	if err := json.Unmarshal(body, &raw); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}

	// Top-level reasoning_effort must NOT be present.
	if _, ok := raw["reasoning_effort"]; ok {
		t.Fatal("reasoning_effort should not be a top-level field")
	}

	// reasoning must be a nested object with effort.
	reasoningRaw, ok := raw["reasoning"]
	if !ok {
		t.Fatal("reasoning object missing from request")
	}
	reasoning, ok := reasoningRaw.(map[string]any)
	if !ok {
		t.Fatalf("reasoning is %T, want object", reasoningRaw)
	}
	if got, want := reasoning["effort"], "medium"; got != want {
		t.Fatalf("reasoning.effort = %v, want %v", got, want)
	}
}

func TestBuildRequestJSON_NoReasoningWhenNotSpecified(t *testing.T) {
	p := NewProvider(llm.ProviderConfig{APIKey: "test-key"})

	req := &llm.Request{
		Model:    "gpt-4",
		Messages: []llm.Message{{Role: llm.RoleUser, Content: "hello"}},
	}

	body, err := p.buildRequestJSON(req)
	if err != nil {
		t.Fatalf("buildRequestJSON: %v", err)
	}

	var raw map[string]any
	if err := json.Unmarshal(body, &raw); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}

	if _, ok := raw["reasoning"]; ok {
		t.Fatal("reasoning should not be present for non-reasoning models")
	}
	if _, ok := raw["reasoning_effort"]; ok {
		t.Fatal("reasoning_effort should not be a top-level field")
	}
}

func TestBuildRequestJSON_ReasoningOffForReasoningModel(t *testing.T) {
	llm.ClearRegistry()
	llm.RegisterModel(llm.ModelDef{
		Pattern: "*mimo*",
		Preset:  llm.PresetReasoning,
	})

	p := NewProvider(llm.ProviderConfig{
		APIKey: "test-key",
		Models: []llm.Model{{
			ID: "xiaomi/mimo-v2.5-pro",
			Capabilities: &llm.Capabilities{
				Streaming:   true,
				Tools:       true,
				Temperature: false,
				SystemRole:  llm.RoleSystem,
				Reasoning: llm.ReasoningCapabilities{
					Kind:       llm.ReasoningKindEffort,
					Efforts:    []string{"minimal", "low", "medium", "high"},
					CanDisable: true,
				},
			},
		}},
	})

	// No reasoning effort specified for a reasoning model: should default to "none"
	// to avoid unwanted reasoning charges.
	req := &llm.Request{
		Model:    "xiaomi/mimo-v2.5-pro",
		Messages: []llm.Message{{Role: llm.RoleUser, Content: "hello"}},
	}

	body, err := p.buildRequestJSON(req)
	if err != nil {
		t.Fatalf("buildRequestJSON: %v", err)
	}

	var raw map[string]any
	if err := json.Unmarshal(body, &raw); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}

	reasoningRaw, ok := raw["reasoning"]
	if !ok {
		t.Fatal("reasoning object missing from request for reasoning model")
	}
	reasoning, ok := reasoningRaw.(map[string]any)
	if !ok {
		t.Fatalf("reasoning is %T, want object", reasoningRaw)
	}
	if got, want := reasoning["effort"], "none"; got != want {
		t.Fatalf("reasoning.effort = %v, want %v", got, want)
	}
}

func TestIsReasoningOff(t *testing.T) {
	tests := []struct {
		input string
		want  bool
	}{
		{"", true},
		{"off", true},
		{"none", true},
		{"disabled", true},
		{"OFF", true},
		{"None", true},
		{"low", false},
		{"medium", false},
		{"high", false},
	}
	for _, tt := range tests {
		if got := IsReasoningOff(tt.input); got != tt.want {
			t.Errorf("IsReasoningOff(%q) = %v, want %v", tt.input, got, tt.want)
		}
	}
}

func TestBuildRequestJSON_StripsTopLevelReasoningEffort(t *testing.T) {
	llm.ClearRegistry()
	llm.RegisterModel(llm.ModelDef{
		Pattern: "*mimo*",
		Preset:  llm.PresetReasoning,
	})

	p := NewProvider(llm.ProviderConfig{
		APIKey: "test-key",
		Models: []llm.Model{{
			ID: "xiaomi/mimo-v2.5-pro",
			Capabilities: &llm.Capabilities{
				Streaming:   true,
				Tools:       true,
				Temperature: false,
				SystemRole:  llm.RoleSystem,
				Reasoning: llm.ReasoningCapabilities{
					Kind:       llm.ReasoningKindEffort,
					Efforts:    []string{"minimal", "low", "medium", "high"},
					CanDisable: true,
				},
			},
		}},
	})

	// Build the request - the provider should strip reasoning_effort and use nested reasoning.
	req := &llm.Request{
		Model:           "xiaomi/mimo-v2.5-pro",
		Messages:        []llm.Message{{Role: llm.RoleUser, Content: "hello"}},
		ReasoningEffort: "high",
	}

	body, err := p.buildRequestJSON(req)
	if err != nil {
		t.Fatalf("buildRequestJSON: %v", err)
	}

	var raw map[string]any
	if err := json.Unmarshal(body, &raw); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}

	if _, ok := raw["reasoning_effort"]; ok {
		t.Fatal("reasoning_effort should not be a top-level field")
	}

	reasoningRaw, ok := raw["reasoning"]
	if !ok {
		t.Fatal("reasoning object missing")
	}
	reasoning, ok := reasoningRaw.(map[string]any)
	if !ok {
		t.Fatalf("reasoning is %T, want object", reasoningRaw)
	}
	if got, want := reasoning["effort"], "high"; got != want {
		t.Fatalf("reasoning.effort = %v, want %v", got, want)
	}
}

func TestOpenRouterProviderUsesBaseCapabilities(t *testing.T) {
	// Verify that the OpenRouter provider correctly delegates capabilities
	// to the underlying Base provider.
	llm.ClearRegistry()
	llm.RegisterModel(llm.ModelDef{
		Pattern: "*mimo*",
		Preset:  llm.PresetReasoning,
	})

	p := NewProvider(llm.ProviderConfig{
		APIKey: "test-key",
		Models: []llm.Model{{
			ID: "xiaomi/mimo-v2.5-pro",
			Capabilities: &llm.Capabilities{
				Streaming:   true,
				Tools:       true,
				Temperature: false,
				SystemRole:  llm.RoleSystem,
				Reasoning: llm.ReasoningCapabilities{
					Kind:       llm.ReasoningKindEffort,
					Efforts:    []string{"minimal", "low", "medium", "high"},
					CanDisable: true,
				},
			},
		}},
	})

	caps := p.Capabilities("xiaomi/mimo-v2.5-pro")
	if caps.Temperature {
		t.Fatal("mimo should not have temperature enabled")
	}
	if !caps.SupportsReasoningEffort("high") {
		t.Fatal("mimo should support reasoning effort high")
	}
}
