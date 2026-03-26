package config

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/pelletier/go-toml/v2"
)

const (
	DefaultProvider = "openrouter"
	DefaultModel    = "openai/gpt-5.4"
)

type Config struct {
	Provider     string `toml:"provider,omitempty"`
	Model        string `toml:"model,omitempty"`
	ContextLimit int    `toml:"context_limit,omitempty"`
}

func DefaultConfigPath() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, ".config", "ion", "config.toml"), nil
}

func LegacyConfigPath() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, ".ion", "config.toml"), nil
}

func Load() (*Config, error) {
	path, err := DefaultConfigPath()
	if err != nil {
		return nil, err
	}

	cfg := &Config{}

	legacy, err := LegacyConfigPath()
	if err != nil {
		return nil, err
	}
	if err := loadFirstExisting(cfg, path, legacy); err != nil {
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
	path, err := DefaultConfigPath()
	if err != nil {
		return err
	}

	if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
		return err
	}

	data, err := toml.Marshal(cfg)
	if err != nil {
		return err
	}

	return os.WriteFile(path, data, 0644)
}

func loadFirstExisting(dst any, paths ...string) error {
	for _, path := range paths {
		data, err := os.ReadFile(path)
		if err != nil {
			if os.IsNotExist(err) {
				continue
			}
			return err
		}
		if err := toml.Unmarshal(data, dst); err != nil {
			return fmt.Errorf("failed to parse config: %w", err)
		}
		return nil
	}
	return nil
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
