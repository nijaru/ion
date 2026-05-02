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
		"provider = \"openai\"\nmodel = \"gpt-4o\"\nreasoning_effort = \"med\"\nfast_model = \"gpt-4.1-mini\"\nfast_reasoning_effort = \"low\"\nsummary_model = \"gpt-4o-mini\"\nsummary_reasoning_effort = \"low\"\nendpoint = \"https://example.com/v1\"\nauth_env_var = \"OPENAI_PROXY_KEY\"\ncontext_limit = 128000\nmax_session_cost = 1.25\nmax_turn_cost = 0.10\nretry_until_cancelled = false\nworkspace_trust = \"off\"\ntelemetry_otlp_endpoint = \" localhost:4317 \"\ntelemetry_otlp_insecure = true\npolicy_path = \" /tmp/ion-policy.yaml \"\nsubagents_path = \" /tmp/ion-agents \"\nsession_retention_days = 14\n[extra_headers]\n\"X-Test\" = \"value\"\n[telemetry_otlp_headers]\n\"x-api-key\" = \" secret \"\n",
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
	if cfg.WorkspaceTrust != "off" {
		t.Fatalf("workspace_trust = %q, want off", cfg.WorkspaceTrust)
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
	if cfg.PolicyPath != "/tmp/ion-policy.yaml" {
		t.Fatalf("policy_path = %q, want /tmp/ion-policy.yaml", cfg.PolicyPath)
	}
	if cfg.SubagentsPath != "/tmp/ion-agents" {
		t.Fatalf("subagents_path = %q, want /tmp/ion-agents", cfg.SubagentsPath)
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
	if cfg.WorkspaceTrust != "prompt" {
		t.Fatalf("workspace_trust = %q, want prompt", cfg.WorkspaceTrust)
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
	if cfg.Provider != "local-api" {
		t.Fatalf("provider = %q, want local-api", cfg.Provider)
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

func TestLoadProviderEnvOverrideClearsStaleModel(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("ION_PROVIDER", "local-api")

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
	if cfg.Provider != "local-api" || cfg.Model != "" {
		t.Fatalf("cfg = %#v, want provider override with no stale model", cfg)
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
	if cfg.Provider != "local-api" || cfg.Model != "qwen3.6:27b" {
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
	if cfg.Provider != "local-api" {
		t.Fatalf("provider = %q, want local-api", cfg.Provider)
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
	if err := os.WriteFile(path, []byte("policy_path = \"~/.ion/work-policy.yaml\"\nsubagents_path = \"~/.ion/agents\""), 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}

	cfg, err := Load()
	if err != nil {
		t.Fatalf("load config: %v", err)
	}
	want := filepath.Join(home, ".ion", "work-policy.yaml")
	if cfg.PolicyPath != want {
		t.Fatalf("policy_path = %q, want %q", cfg.PolicyPath, want)
	}
	wantAgents := filepath.Join(home, ".ion", "agents")
	if cfg.SubagentsPath != wantAgents {
		t.Fatalf("subagents_path = %q, want %q", cfg.SubagentsPath, wantAgents)
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
		ExtraHeaders:           map[string]string{"X-Test": "value"},
		ContextLimit:           128000,
		MaxSessionCost:         1.25,
		MaxTurnCost:            0.10,
		RetryUntilCancelled:    &retryUntilCancelled,
		WorkspaceTrust:         "strict",
		TelemetryOTLPEndpoint:  "localhost:4317",
		TelemetryOTLPInsecure:  true,
		TelemetryOTLPHeaders:   map[string]string{"x-api-key": "secret"},
		PolicyPath:             "/tmp/ion-policy.yaml",
		SubagentsPath:          "/tmp/ion-agents",
		SessionRetentionDays:   14,
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
		`workspace_trust = 'strict'`,
		`telemetry_otlp_endpoint = 'localhost:4317'`,
		`telemetry_otlp_insecure = true`,
		`policy_path = '/tmp/ion-policy.yaml'`,
		`subagents_path = '/tmp/ion-agents'`,
		`[telemetry_otlp_headers]`,
		`x-api-key = 'secret'`,
		`session_retention_days = 14`,
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("saved config missing %q:\n%s", want, got)
		}
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
		PolicyPath:           "/tmp/policy.yaml",
		WorkspaceTrust:       "strict",
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
		`provider = 'local-api'`,
		`model = 'qwen3.6:27b'`,
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("saved state missing %q:\n%s", want, got)
		}
	}
	for _, notWant := range []string{
		`endpoint`,
		`policy_path`,
		`workspace_trust`,
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
		"queue":     "",
		"queued":    "",
		"follow-up": "",
		" STEER ":   "steer",
		"steering":  "steer",
		"unknown":   "",
	} {
		if got := normalizeBusyInput(input); got != want {
			t.Fatalf("normalizeBusyInput(%q) = %q, want %q", input, got, want)
		}
	}
}

func TestBusyInputModeDefaultsToQueue(t *testing.T) {
	if got := (&Config{}).BusyInputMode(); got != "queue" {
		t.Fatalf("busy input mode = %q, want queue", got)
	}
	if got := (&Config{BusyInput: "steer"}).BusyInputMode(); got != "steer" {
		t.Fatalf("busy input mode = %q, want steer", got)
	}
}
