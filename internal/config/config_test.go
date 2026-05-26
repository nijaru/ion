package config

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestLoadReadsConfigFile(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	configDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(configDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}

	path := filepath.Join(configDir, "config.toml")
	if err := os.WriteFile(path, []byte(
		"provider = \"openai\"\nmodel = \"gpt-4o\"\nreasoning_effort = \"med\"\nfast_model = \"gpt-4.1-mini\"\nfast_reasoning_effort = \"low\"\nsummary_model = \"gpt-4o-mini\"\nsummary_reasoning_effort = \"low\"\nendpoint = \"https://example.com/v1\"\nauth_env_var = \"OPENAI_PROXY_KEY\"\ncontext_limit = 128000\nmax_session_cost = 1.25\nmax_turn_cost = 0.10\nretry_until_cancelled = false\ntelemetry_otlp_endpoint = \" localhost:4317 \"\ntelemetry_otlp_insecure = true\nsubagents_path = \" /tmp/ion-agents \"\nsession_retention_days = 14\ntool_env = \"inherit_without_provider_keys\"\nskill_tools = \"readonly\"\nsubagent_tools = \"enabled\"\n[extra_headers]\n\" X-Test \" = \" value \"\n\"Drop\" = \" \"\n[telemetry_otlp_headers]\n\"x-api-key\" = \" secret \"\n",
	), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}

	cfg, err := Load()
	if err != nil {
		t.Fatalf("load config: %v", err)
	}

	if cfg.Provider != "openai" {
		t.Fatalf("provider = %q, want %q", cfg.Provider, "openai")
	}
	if cfg.Model != "gpt-4o" {
		t.Fatalf("model = %q, want %q", cfg.Model, "gpt-4o")
	}
	if cfg.ReasoningEffort != "medium" {
		t.Fatalf("reasoning_effort = %q, want %q", cfg.ReasoningEffort, "medium")
	}
	if cfg.FastModel != "gpt-4.1-mini" {
		t.Fatalf("fast_model = %q, want %q", cfg.FastModel, "gpt-4.1-mini")
	}
	if cfg.FastReasoningEffort != "low" {
		t.Fatalf("fast_reasoning_effort = %q, want %q", cfg.FastReasoningEffort, "low")
	}
	if cfg.SummaryModel != "gpt-4o-mini" {
		t.Fatalf("summary_model = %q, want %q", cfg.SummaryModel, "gpt-4o-mini")
	}
	if cfg.SummaryReasoningEffort != "low" {
		t.Fatalf("summary_reasoning_effort = %q, want %q", cfg.SummaryReasoningEffort, "low")
	}
	if cfg.Endpoint != "https://example.com/v1" {
		t.Fatalf("endpoint = %q, want %q", cfg.Endpoint, "https://example.com/v1")
	}
	if cfg.AuthEnvVar != "OPENAI_PROXY_KEY" {
		t.Fatalf("auth_env_var = %q, want %q", cfg.AuthEnvVar, "OPENAI_PROXY_KEY")
	}
	if got := cfg.ExtraHeaders["X-Test"]; got != "value" {
		t.Fatalf("extra_headers[X-Test] = %q, want %q", got, "value")
	}
	if _, ok := cfg.ExtraHeaders["Drop"]; ok {
		t.Fatalf("extra_headers kept blank value: %#v", cfg.ExtraHeaders)
	}
	if cfg.ContextLimit != 128000 {
		t.Fatalf("context_limit = %d, want %d", cfg.ContextLimit, 128000)
	}
	if cfg.MaxSessionCost != 1.25 {
		t.Fatalf("max_session_cost = %f, want %f", cfg.MaxSessionCost, 1.25)
	}
	if cfg.MaxTurnCost != 0.10 {
		t.Fatalf("max_turn_cost = %f, want %f", cfg.MaxTurnCost, 0.10)
	}
	if cfg.RetryUntilCancelled == nil || *cfg.RetryUntilCancelled {
		t.Fatal("retry_until_cancelled = true or nil, want false")
	}
	if cfg.SessionRetentionDays != 14 {
		t.Fatalf("session_retention_days = %d, want %d", cfg.SessionRetentionDays, 14)
	}
	if cfg.TelemetryOTLPEndpoint != "localhost:4317" {
		t.Fatalf("telemetry_otlp_endpoint = %q, want localhost:4317", cfg.TelemetryOTLPEndpoint)
	}
	if !cfg.TelemetryOTLPInsecure {
		t.Fatal("telemetry_otlp_insecure = false, want true")
	}
	if got := cfg.TelemetryOTLPHeaders["x-api-key"]; got != "secret" {
		t.Fatalf("telemetry header = %q, want secret", got)
	}
	if cfg.SubagentsPath != "/tmp/ion-agents" {
		t.Fatalf("subagents_path = %q, want /tmp/ion-agents", cfg.SubagentsPath)
	}
	if cfg.SkillTools != "read" {
		t.Fatalf("skill_tools = %q, want read", cfg.SkillTools)
	}
	if cfg.SubagentTools != "on" {
		t.Fatalf("subagent_tools = %q, want on", cfg.SubagentTools)
	}
	if cfg.ToolEnvMode() != "inherit_without_provider_keys" {
		t.Fatalf("tool_env = %q, want inherit_without_provider_keys", cfg.ToolEnvMode())
	}
}

func TestLoadUsesDefaultsWhenConfigMissing(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	cfg, err := Load()
	if err != nil {
		t.Fatalf("load config: %v", err)
	}

	if cfg.Provider != "" {
		t.Fatalf("provider = %q, want empty", cfg.Provider)
	}
	if cfg.Model != "" {
		t.Fatalf("model = %q, want empty", cfg.Model)
	}
	if cfg.ContextLimit != 0 {
		t.Fatalf("context_limit = %d, want %d", cfg.ContextLimit, 0)
	}
	if cfg.ReasoningEffort != DefaultReasoningEffort {
		t.Fatalf("reasoning_effort = %q, want %q", cfg.ReasoningEffort, DefaultReasoningEffort)
	}
	if cfg.SessionRetentionDays != DefaultSessionRetentionDays {
		t.Fatalf(
			"session_retention_days = %d, want %d",
			cfg.SessionRetentionDays,
			DefaultSessionRetentionDays,
		)
	}
	if !cfg.RetryUntilCancelledEnabled() {
		t.Fatal("retry_until_cancelled = false, want true")
	}
	if cfg.ToolEnvMode() != "inherit" {
		t.Fatalf("tool_env = %q, want inherit", cfg.ToolEnvMode())
	}
}

func TestLoadAppliesMutableState(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	configDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(configDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	configPath := filepath.Join(configDir, "config.toml")
	if err := os.WriteFile(configPath, []byte(
		"provider = \"openrouter\"\nmodel = \"default/model\"\nendpoint = \"https://example.com/v1\"\n",
	), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}
	statePath := filepath.Join(configDir, "state.toml")
	if err := os.WriteFile(statePath, []byte(
		"provider = \"local-api\"\nmodel = \"qwen3.6:27b\"\nreasoning_effort = \"high\"\n",
	), 0o644); err != nil {
		t.Fatalf("write state: %v", err)
	}

	cfg, err := Load()
	if err != nil {
		t.Fatalf("load config: %v", err)
	}
	if cfg.Provider != "openai-compatible" {
		t.Fatalf("provider = %q, want openai-compatible", cfg.Provider)
	}
	if cfg.Model != "qwen3.6:27b" {
		t.Fatalf("model = %q, want qwen3.6:27b", cfg.Model)
	}
	if cfg.ReasoningEffort != "high" {
		t.Fatalf("reasoning_effort = %q, want high", cfg.ReasoningEffort)
	}
	if cfg.Endpoint != "https://example.com/v1" {
		t.Fatalf("endpoint = %q, want stable config endpoint", cfg.Endpoint)
	}
}

func TestLoadStateProviderOverrideClearsProviderScopedPresets(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	configDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(configDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	configPath := filepath.Join(configDir, "config.toml")
	if err := os.WriteFile(configPath, []byte(
		"provider = \"local-api\"\n"+
			"model = \"qwen3.6:27b\"\n"+
			"fast_model = \"qwen3.6:27b-fast\"\n"+
			"fast_reasoning_effort = \"low\"\n"+
			"summary_model = \"qwen3.6:27b-summary\"\n"+
			"summary_reasoning_effort = \"minimal\"\n",
	), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}
	statePath := filepath.Join(configDir, "state.toml")
	if err := os.WriteFile(statePath, []byte(
		"provider = \"openrouter\"\nmodel = \"openai/gpt-5.4\"\n",
	), 0o644); err != nil {
		t.Fatalf("write state: %v", err)
	}

	cfg, err := Load()
	if err != nil {
		t.Fatalf("load config: %v", err)
	}
	if cfg.Provider != "openrouter" || cfg.Model != "openai/gpt-5.4" {
		t.Fatalf(
			"cfg provider/model = %s/%s, want openrouter/openai/gpt-5.4",
			cfg.Provider,
			cfg.Model,
		)
	}
	if cfg.FastModel != "" ||
		cfg.FastReasoningEffort != "" ||
		cfg.SummaryModel != "" ||
		cfg.SummaryReasoningEffort != "" {
		t.Fatalf("provider-scoped presets were not cleared: %#v", cfg)
	}
}

func TestLoadProviderEnvOverrideClearsStaleModel(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("ION_PROVIDER", "local-api")

	configDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(configDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(
		filepath.Join(configDir, "config.toml"),
		[]byte(
			"provider = \"openrouter\"\n"+
				"model = \"openai/gpt-5.4\"\n"+
				"fast_model = \"google/gemini-2.0-flash-lite-001\"\n"+
				"fast_reasoning_effort = \"low\"\n",
		),
		0o644,
	); err != nil {
		t.Fatalf("write config: %v", err)
	}
	if err := os.WriteFile(
		filepath.Join(configDir, "state.toml"),
		[]byte("provider = \"openrouter\"\nmodel = \"openai/gpt-5.4\"\n"),
		0o644,
	); err != nil {
		t.Fatalf("write state: %v", err)
	}

	cfg, err := Load()
	if err != nil {
		t.Fatalf("load config: %v", err)
	}
	if cfg.Provider != "openai-compatible" || cfg.Model != "" {
		t.Fatalf("cfg = %#v, want provider override with no stale model", cfg)
	}
	if cfg.FastModel != "" || cfg.FastReasoningEffort != "" {
		t.Fatalf("provider-scoped fast preset was not cleared: %#v", cfg)
	}
}

func TestLoadProviderEnvOverrideKeepsExplicitModel(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("ION_PROVIDER", "local-api")
	t.Setenv("ION_MODEL", "qwen3.6:27b")

	configDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(configDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(
		filepath.Join(configDir, "state.toml"),
		[]byte("provider = \"openrouter\"\nmodel = \"openai/gpt-5.4\"\n"),
		0o644,
	); err != nil {
		t.Fatalf("write state: %v", err)
	}

	cfg, err := Load()
	if err != nil {
		t.Fatalf("load config: %v", err)
	}
	if cfg.Provider != "openai-compatible" || cfg.Model != "qwen3.6:27b" {
		t.Fatalf("cfg = %#v, want explicit provider/model override", cfg)
	}
}

func TestLoadStateCanClearConfiguredModel(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	configDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(configDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(
		filepath.Join(configDir, "config.toml"),
		[]byte("provider = \"openrouter\"\nmodel = \"vendor/model-b\"\n"),
		0o644,
	); err != nil {
		t.Fatalf("write config: %v", err)
	}
	if err := os.WriteFile(
		filepath.Join(configDir, "state.toml"),
		[]byte("provider = \"local-api\"\nmodel = \"\"\n"),
		0o644,
	); err != nil {
		t.Fatalf("write state: %v", err)
	}

	cfg, err := Load()
	if err != nil {
		t.Fatalf("load config: %v", err)
	}
	if cfg.Provider != "openai-compatible" {
		t.Fatalf("provider = %q, want openai-compatible", cfg.Provider)
	}
	if cfg.Model != "" {
		t.Fatalf("model = %q, want empty", cfg.Model)
	}
}

func TestLoadAppliesEnvOverrides(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("ION_MODEL", "openrouter openai/gpt-5.4")
	t.Setenv("ION_PROVIDER", "anthropic")
	t.Setenv("ION_REASONING_EFFORT", "high")

	cfg, err := Load()
	if err != nil {
		t.Fatalf("load config: %v", err)
	}

	if cfg.Provider != "anthropic" {
		t.Fatalf("provider = %q, want %q", cfg.Provider, "anthropic")
	}
	if cfg.Model != "openai/gpt-5.4" {
		t.Fatalf("model = %q, want %q", cfg.Model, "openai/gpt-5.4")
	}
	if cfg.ReasoningEffort != "high" {
		t.Fatalf("reasoning_effort = %q, want %q", cfg.ReasoningEffort, "high")
	}
}

func TestLoadExpandsUserPaths(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	configDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(configDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	path := filepath.Join(configDir, "config.toml")
	if err := os.WriteFile(path, []byte("subagents_path = \"~/.ion/agents\""), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}

	cfg, err := Load()
	if err != nil {
		t.Fatalf("load config: %v", err)
	}
	want := filepath.Join(home, ".ion", "agents")
	if cfg.SubagentsPath != want {
		t.Fatalf("subagents_path = %q, want %q", cfg.SubagentsPath, want)
	}
}

func TestSaveWritesStatePath(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	retryUntilCancelled := false

	cfg := &Config{
		Provider:               "openai",
		Model:                  "gpt-4o",
		ReasoningEffort:        "low",
		FastModel:              "gpt-4.1-mini",
		FastReasoningEffort:    "low",
		SummaryModel:           "gpt-4o-mini",
		SummaryReasoningEffort: "low",
		Endpoint:               "https://example.com/v1",
		AuthEnvVar:             "OPENAI_PROXY_KEY",
		ExtraHeaders:           map[string]string{" X-Test ": " value ", "Drop": " "},
		ContextLimit:           128000,
		MaxSessionCost:         1.25,
		MaxTurnCost:            0.10,
		RetryUntilCancelled:    &retryUntilCancelled,
		TelemetryOTLPEndpoint:  "localhost:4317",
		TelemetryOTLPInsecure:  true,
		TelemetryOTLPHeaders:   map[string]string{"x-api-key": "secret"},
		SubagentsPath:          "/tmp/ion-agents",
		SessionRetentionDays:   14,
		ToolEnv:                "inherit_without_provider_keys",
		SubagentTools:          "enabled",
	}
	if err := Save(cfg); err != nil {
		t.Fatalf("save config: %v", err)
	}

	path := filepath.Join(home, ".ion", "config.toml")
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read saved config: %v", err)
	}
	got := string(data)
	for _, want := range []string{
		`provider =`,
		`openai`,
		`model =`,
		`gpt-4o`,
		`fast_model =`,
		`gpt-4.1-mini`,
		`fast_reasoning_effort =`,
		`summary_model =`,
		`gpt-4o-mini`,
		`reasoning_effort = 'low'`,
		`endpoint = 'https://example.com/v1'`,
		`auth_env_var = 'OPENAI_PROXY_KEY'`,
		`[extra_headers]`,
		`X-Test = 'value'`,
		`context_limit = 128000`,
		`max_session_cost = 1.25`,
		`max_turn_cost = 0.1`,
		`retry_until_cancelled = false`,
		`telemetry_otlp_endpoint = 'localhost:4317'`,
		`telemetry_otlp_insecure = true`,
		`subagents_path = '/tmp/ion-agents'`,
		`subagent_tools = 'on'`,
		`[telemetry_otlp_headers]`,
		`x-api-key = 'secret'`,
		`session_retention_days = 14`,
		`tool_env = 'inherit_without_provider_keys'`,
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("saved config missing %q:\n%s", want, got)
		}
	}
	if strings.Contains(got, "Drop") {
		t.Fatalf("saved config kept blank extra header:\n%s", got)
	}
}

func TestSaveStateWritesOnlyMutableFields(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	cfg := &Config{
		Provider:             "local-api",
		Model:                "qwen3.6:27b",
		ReasoningEffort:      "auto",
		Endpoint:             "http://fedora:8080/v1",
		ToolVerbosity:        "collapsed",
		MaxSessionCost:       1.25,
		SessionRetentionDays: 14,
	}
	if err := SaveState(cfg); err != nil {
		t.Fatalf("save state: %v", err)
	}
	data, err := os.ReadFile(filepath.Join(home, ".ion", "state.toml"))
	if err != nil {
		t.Fatalf("read state: %v", err)
	}
	got := string(data)
	for _, want := range []string{
		`provider = 'openai-compatible'`,
		`model = 'qwen3.6:27b'`,
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("saved state missing %q:\n%s", want, got)
		}
	}
	for _, notWant := range []string{
		`endpoint`,
		`tool_verbosity`,
		`max_session_cost`,
		`session_retention_days`,
		`reasoning_effort`,
	} {
		if strings.Contains(got, notWant) {
			t.Fatalf("saved state should not include %q:\n%s", notWant, got)
		}
	}
}

func TestSaveStatePreservesActivePreset(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	if err := SaveActivePreset("fast"); err != nil {
		t.Fatalf("save active preset: %v", err)
	}
	if err := SaveState(&Config{Provider: "openai", Model: "gpt-4o"}); err != nil {
		t.Fatalf("save state: %v", err)
	}
	state, err := LoadState()
	if err != nil {
		t.Fatalf("load state: %v", err)
	}
	if state.ActivePreset == nil || *state.ActivePreset != "fast" {
		t.Fatalf("active_preset = %#v, want fast", state.ActivePreset)
	}
}

func TestSaveActivePresetUpdatesState(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	if err := SaveState(&Config{Provider: "openai", Model: "gpt-4o"}); err != nil {
		t.Fatalf("save state: %v", err)
	}
	if err := SaveActivePreset("fast"); err != nil {
		t.Fatalf("save active preset: %v", err)
	}
	state, err := LoadState()
	if err != nil {
		t.Fatalf("load state: %v", err)
	}
	if state.Provider == nil || *state.Provider != "openai" {
		t.Fatalf("provider = %#v, want openai", state.Provider)
	}
	if state.Model == nil || *state.Model != "gpt-4o" {
		t.Fatalf("model = %#v, want gpt-4o", state.Model)
	}
	if state.ActivePreset == nil || *state.ActivePreset != "fast" {
		t.Fatalf("active_preset = %#v, want fast", state.ActivePreset)
	}
}

func TestSaveReasoningStatePreservesSelectedModels(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	if err := SaveState(&Config{
		Provider:            "openrouter",
		Model:               "model-a",
		FastModel:           "model-b",
		FastReasoningEffort: "low",
	}); err != nil {
		t.Fatalf("save state: %v", err)
	}
	if err := SaveReasoningState("primary", "high"); err != nil {
		t.Fatalf("save reasoning: %v", err)
	}
	state, err := LoadState()
	if err != nil {
		t.Fatalf("load state: %v", err)
	}
	if state.Provider == nil || *state.Provider != "openrouter" {
		t.Fatalf("provider = %#v, want openrouter", state.Provider)
	}
	if state.Model == nil || *state.Model != "model-a" {
		t.Fatalf("model = %#v, want model-a", state.Model)
	}
	if state.FastModel == nil || *state.FastModel != "model-b" {
		t.Fatalf("fast_model = %#v, want model-b", state.FastModel)
	}
	if state.FastReasoningEffort == nil || *state.FastReasoningEffort != "low" {
		t.Fatalf("fast reasoning = %#v, want low", state.FastReasoningEffort)
	}
	if state.ReasoningEffort == nil || *state.ReasoningEffort != "high" {
		t.Fatalf("reasoning = %#v, want high", state.ReasoningEffort)
	}
}

func TestSaveReasoningStateDoesNotFreezeConfiguredModel(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	if err := SaveReasoningState("primary", "high"); err != nil {
		t.Fatalf("save reasoning: %v", err)
	}
	data, err := os.ReadFile(filepath.Join(home, ".ion", "state.toml"))
	if err != nil {
		t.Fatalf("read state: %v", err)
	}
	got := string(data)
	if strings.Contains(got, "provider") || strings.Contains(got, "model") {
		t.Fatalf("reasoning state should not freeze provider/model:\n%s", got)
	}
	if !strings.Contains(got, "reasoning_effort = 'high'") {
		t.Fatalf("state missing reasoning effort:\n%s", got)
	}
}

func TestSaveRuntimeStateCombinesSelectionReasoningAndActivePreset(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	if err := SaveState(&Config{
		Provider:            "openrouter",
		Model:               "model-a",
		FastModel:           "model-b",
		FastReasoningEffort: "low",
	}); err != nil {
		t.Fatalf("seed state: %v", err)
	}
	if err := SaveRuntimeState(RuntimeStateUpdate{
		Config: &Config{
			Provider:        "openai",
			Model:           "gpt-5.5",
			ReasoningEffort: "medium",
			FastModel:       "gpt-5.5-mini",
		},
		PersistConfig:       true,
		ActivePreset:        "fast",
		PersistActivePreset: true,
		ReasoningPreset:     "fast",
		ReasoningEffort:     "high",
		PersistReasoning:    true,
	}); err != nil {
		t.Fatalf("save runtime state: %v", err)
	}

	state, err := LoadState()
	if err != nil {
		t.Fatalf("load state: %v", err)
	}
	if state.Provider == nil || *state.Provider != "openai" {
		t.Fatalf("provider = %#v, want openai", state.Provider)
	}
	if state.Model == nil || *state.Model != "gpt-5.5" {
		t.Fatalf("model = %#v, want gpt-5.5", state.Model)
	}
	if state.ReasoningEffort == nil || *state.ReasoningEffort != "medium" {
		t.Fatalf("reasoning = %#v, want medium", state.ReasoningEffort)
	}
	if state.FastModel == nil || *state.FastModel != "gpt-5.5-mini" {
		t.Fatalf("fast model = %#v, want gpt-5.5-mini", state.FastModel)
	}
	if state.FastReasoningEffort == nil || *state.FastReasoningEffort != "high" {
		t.Fatalf("fast reasoning = %#v, want high", state.FastReasoningEffort)
	}
	if state.ActivePreset == nil || *state.ActivePreset != "fast" {
		t.Fatalf("active preset = %#v, want fast", state.ActivePreset)
	}
}

func TestSaveUsesAtomicReplace(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	if err := Save(&Config{Provider: "openai", Model: "gpt-4o"}); err != nil {
		t.Fatalf("first save config: %v", err)
	}
	if err := Save(&Config{Provider: "anthropic", Model: "claude-sonnet-4-5"}); err != nil {
		t.Fatalf("second save config: %v", err)
	}
	path := filepath.Join(home, ".ion", "config.toml")
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read config: %v", err)
	}
	got := string(data)
	if strings.Contains(got, "gpt-4o") {
		t.Fatalf("config kept stale model after replace:\n%s", got)
	}
	if !strings.Contains(got, "claude-sonnet-4-5") {
		t.Fatalf("config missing replacement model:\n%s", got)
	}
	if matches, err := filepath.Glob(filepath.Join(home, ".ion", ".config.toml.tmp-*")); err != nil {
		t.Fatalf("glob temp config: %v", err)
	} else if len(matches) != 0 {
		t.Fatalf("temporary config files left behind: %v", matches)
	}
}

func TestSaveStateUsesAtomicReplace(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	if err := SaveState(&Config{Provider: "openai", Model: "gpt-4o"}); err != nil {
		t.Fatalf("first save state: %v", err)
	}
	if err := SaveState(&Config{Provider: "local-api", Model: "qwen3.6:27b"}); err != nil {
		t.Fatalf("second save state: %v", err)
	}
	path := filepath.Join(home, ".ion", "state.toml")
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read state: %v", err)
	}
	got := string(data)
	if strings.Contains(got, "gpt-4o") {
		t.Fatalf("state kept stale model after replace:\n%s", got)
	}
	if !strings.Contains(got, "qwen3.6:27b") {
		t.Fatalf("state missing replacement model:\n%s", got)
	}
	if matches, err := filepath.Glob(filepath.Join(home, ".ion", ".state.toml.tmp-*")); err != nil {
		t.Fatalf("glob temp state: %v", err)
	} else if len(matches) != 0 {
		t.Fatalf("temporary state files left behind: %v", matches)
	}
}

func TestLoadClampsNegativeCostLimits(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	configDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(configDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	path := filepath.Join(configDir, "config.toml")
	if err := os.WriteFile(path, []byte("max_session_cost = -1\nmax_turn_cost = -0.5\n"), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}

	cfg, err := Load()
	if err != nil {
		t.Fatalf("load config: %v", err)
	}
	if cfg.MaxSessionCost != 0 {
		t.Fatalf("max_session_cost = %f, want 0", cfg.MaxSessionCost)
	}
	if cfg.MaxTurnCost != 0 {
		t.Fatalf("max_turn_cost = %f, want 0", cfg.MaxTurnCost)
	}
}

func TestDefaultDataDirUsesIonDataDir(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	got, err := DefaultDataDir()
	if err != nil {
		t.Fatalf("default data dir: %v", err)
	}
	want := filepath.Join(home, ".ion", "data")
	if got != want {
		t.Fatalf("data dir = %q, want %q", got, want)
	}
}

func TestDefaultSkillsDirUsesIonSkillsDir(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	got, err := DefaultSkillsDir()
	if err != nil {
		t.Fatalf("default skills dir: %v", err)
	}
	want := filepath.Join(home, ".ion", "skills")
	if got != want {
		t.Fatalf("skills dir = %q, want %q", got, want)
	}
}

func TestDefaultModelCacheTTLSeconds(t *testing.T) {
	if got := DefaultModelCacheTTLSeconds(); got != 3600 {
		t.Fatalf("ttl = %d, want %d", got, 3600)
	}
}

func TestNormalizeReasoningEffort(t *testing.T) {
	for input, want := range map[string]string{
		"":       DefaultReasoningEffort,
		"auto":   DefaultReasoningEffort,
		"none":   "off",
		"MIN":    "minimal",
		"med":    "medium",
		"medium": "medium",
		"LOW":    "low",
		"xhigh":  "xhigh",
		"max":    "max",
		"weird":  DefaultReasoningEffort,
	} {
		if got := normalizeReasoningEffort(input); got != want {
			t.Fatalf("normalizeReasoningEffort(%q) = %q, want %q", input, got, want)
		}
	}
}

func TestNormalizeVerbosity(t *testing.T) {
	for input, want := range map[string]string{
		"":            "",
		" FULL ":      "full",
		"collapsed":   "collapsed",
		"HIDDEN":      "hidden",
		"unsupported": "",
	} {
		if got := normalizeVerbosity(input); got != want {
			t.Fatalf("normalizeVerbosity(%q) = %q, want %q", input, got, want)
		}
	}
}

func TestNormalizeReadOutput(t *testing.T) {
	for input, want := range map[string]string{
		"":            "",
		"show":        "full",
		" FULL ":      "full",
		"single":      "summary",
		"summary":     "summary",
		"combined":    "summary",
		"collapsed":   "summary",
		"hidden":      "hidden",
		"unsupported": "",
	} {
		if got := normalizeReadOutput(input); got != want {
			t.Fatalf("normalizeReadOutput(%q) = %q, want %q", input, got, want)
		}
	}
}

func TestNormalizeWriteOutput(t *testing.T) {
	for input, want := range map[string]string{
		"":            "",
		"show":        "diff",
		"diff":        "diff",
		" FULL ":      "diff",
		"single":      "summary",
		"summary":     "summary",
		"call":        "summary",
		"hidden":      "hidden",
		"unsupported": "",
	} {
		if got := normalizeWriteOutput(input); got != want {
			t.Fatalf("normalizeWriteOutput(%q) = %q, want %q", input, got, want)
		}
	}
}

func TestNormalizeBashOutput(t *testing.T) {
	for input, want := range map[string]string{
		"":            "",
		"show":        "full",
		"verbose":     "full",
		" FULL ":      "full",
		"truncated":   "summary",
		"summary":     "summary",
		"collapsed":   "summary",
		"command":     "hidden",
		"call":        "hidden",
		"hidden":      "hidden",
		"unsupported": "",
	} {
		if got := normalizeBashOutput(input); got != want {
			t.Fatalf("normalizeBashOutput(%q) = %q, want %q", input, got, want)
		}
	}
}

func TestNormalizeBusyInput(t *testing.T) {
	for input, want := range map[string]string{
		"":          "",
		"queue":     "queue",
		"queued":    "queue",
		"follow-up": "queue",
		" STEER ":   "",
		"steering":  "",
		"unknown":   "",
	} {
		if got := normalizeBusyInput(input); got != want {
			t.Fatalf("normalizeBusyInput(%q) = %q, want %q", input, got, want)
		}
	}
}

func TestNormalizeSkillTools(t *testing.T) {
	for input, want := range map[string]string{
		"":          "off",
		"off":       "off",
		"disabled":  "off",
		"read":      "read",
		"READ_ONLY": "read",
		"manage":    "manage",
		"full":      "manage",
		"unknown":   "off",
	} {
		if got := normalizeSkillTools(input); got != want {
			t.Fatalf("normalizeSkillTools(%q) = %q, want %q", input, got, want)
		}
	}
}

func TestSkillToolModeDefaultsOff(t *testing.T) {
	if got := (&Config{}).SkillToolMode(); got != "off" {
		t.Fatalf("skill tool mode = %q, want off", got)
	}
	if got := (&Config{SkillTools: "read"}).SkillToolMode(); got != "read" {
		t.Fatalf("skill tool mode = %q, want read", got)
	}
}

func TestNormalizeSubagentTools(t *testing.T) {
	for input, want := range map[string]string{
		"":          "off",
		"off":       "off",
		"disabled":  "off",
		"enabled":   "on",
		"TRUE":      "on",
		"subagents": "on",
		"delegate":  "on",
		"unknown":   "off",
	} {
		if got := normalizeSubagentTools(input); got != want {
			t.Fatalf("normalizeSubagentTools(%q) = %q, want %q", input, got, want)
		}
	}
}

func TestNormalizeToolMode(t *testing.T) {
	for input, want := range map[string]string{
		"":          "coding",
		"coding":    "coding",
		"read":      "read",
		"readonly":  "read",
		"read-only": "read",
		"all":       "all",
		"full":      "all",
		"weird":     "coding",
	} {
		if got := normalizeToolMode(input); got != want {
			t.Fatalf("normalizeToolMode(%q) = %q, want %q", input, got, want)
		}
	}
	if got := (&Config{}).ActiveToolMode(); got != "coding" {
		t.Fatalf("active tool mode = %q, want coding", got)
	}
	if got := (&Config{ToolMode: "read-only"}).ActiveToolMode(); got != "read" {
		t.Fatalf("active tool mode = %q, want read", got)
	}
}

func TestSubagentToolModeDefaultsOff(t *testing.T) {
	if got := (&Config{}).SubagentToolMode(); got != "off" {
		t.Fatalf("subagent tool mode = %q, want off", got)
	}
	if got := (&Config{SubagentTools: "enabled"}).SubagentToolMode(); got != "on" {
		t.Fatalf("subagent tool mode = %q, want on", got)
	}
}

func TestBusyInputModeDefaultsToSteer(t *testing.T) {
	if got := (&Config{}).BusyInputMode(); got != "steer" {
		t.Fatalf("busy input mode = %q, want steer", got)
	}
	if got := (&Config{BusyInput: "steer"}).BusyInputMode(); got != "steer" {
		t.Fatalf("busy input mode = %q, want steer", got)
	}
	if got := (&Config{BusyInput: "queue"}).BusyInputMode(); got != "queue" {
		t.Fatalf("busy input mode = %q, want queue", got)
	}
}

func TestLoadParsesModelCapabilities(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	configDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(configDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}

	path := filepath.Join(configDir, "config.toml")
	if err := os.WriteFile(path, []byte(`
[[model_capabilities]]
pattern = "*mimo*"
temperature = false
reasoning_kind = "effort"
system_role = "developer"

[[model_capabilities]]
pattern = "custom-thinking"
temperature = true
reasoning_kind = "budget"
system_role = "system"
`), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}

	cfg, err := Load()
	if err != nil {
		t.Fatalf("load config: %v", err)
	}

	if len(cfg.ModelCapabilities) != 2 {
		t.Fatalf("len(ModelCapabilities) = %d, want 2", len(cfg.ModelCapabilities))
	}

	cap0 := cfg.ModelCapabilities[0]
	if cap0.Pattern != "*mimo*" {
		t.Fatalf("cap0 pattern = %q, want *mimo*", cap0.Pattern)
	}
	if cap0.Temperature == nil || *cap0.Temperature {
		t.Fatal("cap0 temperature should be false")
	}
	if cap0.ReasoningKind != "effort" {
		t.Fatalf("cap0 reasoning_kind = %q, want effort", cap0.ReasoningKind)
	}
	if cap0.SystemRole != "developer" {
		t.Fatalf("cap0 system_role = %q, want developer", cap0.SystemRole)
	}

	cap1 := cfg.ModelCapabilities[1]
	if cap1.Pattern != "custom-thinking" {
		t.Fatalf("cap1 pattern = %q, want custom-thinking", cap1.Pattern)
	}
	if cap1.Temperature == nil || !*cap1.Temperature {
		t.Fatal("cap1 temperature should be true")
	}
	if cap1.ReasoningKind != "budget" {
		t.Fatalf("cap1 reasoning_kind = %q, want budget", cap1.ReasoningKind)
	}
	if cap1.SystemRole != "system" {
		t.Fatalf("cap1 system_role = %q, want system", cap1.SystemRole)
	}
}
