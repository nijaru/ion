package config

import (
	"testing"

	"github.com/pelletier/go-toml/v2"
)

func TestProvidersTOML(t *testing.T) {
	input := `
provider = "openrouter"
model = "deepseek/deepseek-v4-pro"

[providers.openrouter.model_routing]

[providers.openrouter.model_routing."deepseek/deepseek-v4-pro"]
only = ["deepseek"]
allow_fallbacks = true
`
	var cfg Config
	if err := toml.Unmarshal([]byte(input), &cfg); err != nil {
		t.Fatalf("parse: %v", err)
	}
	ps, ok := cfg.Providers["openrouter"]
	if !ok {
		t.Fatal("openrouter not found in providers")
	}
	mr, ok := ps.ModelRouting["deepseek/deepseek-v4-pro"]
	if !ok {
		t.Fatal("deepseek routing not found")
	}
	if len(mr.Only) != 1 || mr.Only[0] != "deepseek" {
		t.Errorf("Only = %v, want [deepseek]", mr.Only)
	}
	if !mr.AllowFallbacks {
		t.Error("AllowFallbacks = false, want true")
	}
}
