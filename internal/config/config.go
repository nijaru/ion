package config

import (
	"os"
	"strings"
)

const (
	DefaultProvider = "openrouter"
	DefaultModel    = "openai/gpt-5.4"
)

type Config = State

func DefaultConfigPath() (string, error) {
	return DefaultStatePath()
}

func Load() (*Config, error) {
	cfg, err := LoadState()
	if err != nil {
		return nil, err
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

	return cfg, nil
}

func Save(cfg *Config) error {
	return SaveState(cfg)
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
