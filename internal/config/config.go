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
)

type Config struct {
	Provider             string `toml:"provider,omitempty"`
	Model                string `toml:"model,omitempty"`
	ContextLimit         int    `toml:"context_limit,omitempty"`
	SessionRetentionDays int    `toml:"session_retention_days,omitempty"`
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

	cfg.Provider = strings.ToLower(strings.TrimSpace(cfg.Provider))
	cfg.Model = strings.TrimSpace(cfg.Model)
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
	if out.ContextLimit < 0 {
		out.ContextLimit = 0
	}
	if out.SessionRetentionDays <= 0 {
		out.SessionRetentionDays = DefaultSessionRetentionDays
	}

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
