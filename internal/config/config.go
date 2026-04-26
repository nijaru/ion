package config

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/pelletier/go-toml/v2"
)

const (
	DefaultSessionRetentionDays = 90
	defaultModelCacheTTLSeconds = 3600
	DefaultReasoningEffort      = "auto"
)

type Config struct {
	Provider               string            `toml:"provider,omitempty"`
	Model                  string            `toml:"model,omitempty"`
	ReasoningEffort        string            `toml:"reasoning_effort,omitempty"`
	FastModel              string            `toml:"fast_model,omitempty"`
	FastReasoningEffort    string            `toml:"fast_reasoning_effort,omitempty"`
	SummaryModel           string            `toml:"summary_model,omitempty"`
	SummaryReasoningEffort string            `toml:"summary_reasoning_effort,omitempty"`
	DefaultMode            string            `toml:"default_mode,omitempty"`
	Endpoint               string            `toml:"endpoint,omitempty"`
	AuthEnvVar             string            `toml:"auth_env_var,omitempty"`
	ExtraHeaders           map[string]string `toml:"extra_headers,omitempty"`
	ContextLimit           int               `toml:"context_limit,omitempty"`
	SessionRetentionDays   int               `toml:"session_retention_days,omitempty"`
	ToolVerbosity          string            `toml:"tool_verbosity,omitempty"`
	ThinkingVerbosity      string            `toml:"thinking_verbosity,omitempty"`
}

func DefaultConfigPath() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, ".ion", "config.toml"), nil
}

func Load() (*Config, error) {
	cfg := defaultConfig()

	path, err := DefaultConfigPath()
	if err != nil {
		return nil, err
	}
	if data, err := os.ReadFile(path); err != nil {
		if !os.IsNotExist(err) {
			return nil, err
		}
	} else if err := toml.Unmarshal(data, cfg); err != nil {
		return nil, fmt.Errorf("failed to parse config: %w", err)
	}

	if override := os.Getenv("ION_MODEL"); override != "" {
		if provider, model, ok := splitProviderModel(override); ok {
			cfg.Provider = provider
			cfg.Model = model
		} else {
			cfg.Model = override
		}
	}

	if override := os.Getenv("ION_PROVIDER"); override != "" {
		cfg.Provider = override
	}
	if override := os.Getenv("ION_REASONING_EFFORT"); override != "" {
		cfg.ReasoningEffort = override
	}

	cfg.Provider = strings.ToLower(strings.TrimSpace(cfg.Provider))
	cfg.Model = strings.TrimSpace(cfg.Model)
	cfg.ReasoningEffort = normalizeReasoningEffort(cfg.ReasoningEffort)
	cfg.FastModel = strings.TrimSpace(cfg.FastModel)
	cfg.FastReasoningEffort = normalizeOptionalReasoningEffort(cfg.FastReasoningEffort)
	cfg.SummaryModel = strings.TrimSpace(cfg.SummaryModel)
	cfg.SummaryReasoningEffort = normalizeOptionalReasoningEffort(cfg.SummaryReasoningEffort)
	cfg.Endpoint = strings.TrimSpace(cfg.Endpoint)
	cfg.AuthEnvVar = strings.TrimSpace(cfg.AuthEnvVar)
	cfg.ToolVerbosity = normalizeVerbosity(cfg.ToolVerbosity)
	cfg.ThinkingVerbosity = normalizeVerbosity(cfg.ThinkingVerbosity)
	if cfg.ContextLimit < 0 {
		cfg.ContextLimit = 0
	}
	if cfg.SessionRetentionDays <= 0 {
		cfg.SessionRetentionDays = DefaultSessionRetentionDays
	}

	return cfg, nil
}

func Save(cfg *Config) error {
	path, err := DefaultConfigPath()
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}

	out := *cfg
	out.Provider = strings.ToLower(strings.TrimSpace(out.Provider))
	out.Model = strings.TrimSpace(out.Model)
	out.ReasoningEffort = normalizeReasoningEffort(out.ReasoningEffort)
	if out.ReasoningEffort == DefaultReasoningEffort {
		out.ReasoningEffort = ""
	}
	out.FastModel = strings.TrimSpace(out.FastModel)
	out.FastReasoningEffort = normalizeOptionalReasoningEffort(out.FastReasoningEffort)
	out.SummaryModel = strings.TrimSpace(out.SummaryModel)
	out.SummaryReasoningEffort = normalizeOptionalReasoningEffort(out.SummaryReasoningEffort)
	out.Endpoint = strings.TrimSpace(out.Endpoint)
	out.AuthEnvVar = strings.TrimSpace(out.AuthEnvVar)
	if out.ContextLimit < 0 {
		out.ContextLimit = 0
	}
	if out.SessionRetentionDays <= 0 {
		out.SessionRetentionDays = DefaultSessionRetentionDays
	}
	out.ToolVerbosity = normalizeVerbosity(out.ToolVerbosity)
	out.ThinkingVerbosity = normalizeVerbosity(out.ThinkingVerbosity)

	data, err := toml.Marshal(&out)
	if err != nil {
		return err
	}
	return os.WriteFile(path, data, 0o644)
}

func splitProviderModel(value string) (string, string, bool) {
	left, right, ok := strings.Cut(value, " ")
	if !ok {
		return "", "", false
	}

	provider := strings.ToLower(strings.TrimSpace(left))
	model := strings.TrimSpace(right)
	if provider == "" || model == "" {
		return "", "", false
	}

	return provider, model, true
}

func DefaultDataDir() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, ".ion", "data"), nil
}

func DefaultModelCacheTTLSeconds() int {
	return defaultModelCacheTTLSeconds
}

func defaultConfig() *Config {
	return &Config{
		SessionRetentionDays: DefaultSessionRetentionDays,
	}
}

func normalizeReasoningEffort(value string) string {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "", DefaultReasoningEffort:
		return DefaultReasoningEffort
	case "low":
		return "low"
	case "medium", "med":
		return "medium"
	case "high":
		return "high"
	default:
		return DefaultReasoningEffort
	}
}

func normalizeOptionalReasoningEffort(value string) string {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "", DefaultReasoningEffort:
		return ""
	case "low":
		return "low"
	case "medium", "med":
		return "medium"
	case "high":
		return "high"
	default:
		return ""
	}
}

func normalizeVerbosity(value string) string {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "full":
		return "full"
	case "collapsed":
		return "collapsed"
	case "hidden":
		return "hidden"
	default:
		return ""
	}
}

func ResolveDefaultMode(value string) string {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "read", "r":
		return "read"
	case "edit", "e", "write", "w":
		return "edit"
	case "yolo", "y":
		return "yolo"
	default:
		return "edit"
	}
}
