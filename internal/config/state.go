package config

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/pelletier/go-toml/v2"
)

type State struct {
	Provider             string `toml:"provider,omitempty"`
	Model                string `toml:"model,omitempty"`
	ContextLimit         int    `toml:"context_limit,omitempty"`
	DataDir              string `toml:"data_dir,omitempty"`
	ModelCacheTTLSeconds int    `toml:"model_cache_ttl_secs,omitempty"`
	SessionRetentionDays int    `toml:"session_retention_days,omitempty"`
}

func DefaultStatePath() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, ".ion", "state.toml"), nil
}

func LoadState() (*State, error) {
	state := defaultState()

	path, err := DefaultStatePath()
	if err != nil {
		return nil, err
	}
	if data, err := os.ReadFile(path); err != nil {
		if !os.IsNotExist(err) {
			return nil, err
		}
	} else if err := toml.Unmarshal(data, state); err != nil {
		return nil, fmt.Errorf("failed to parse state: %w", err)
	}

	state.Provider = strings.ToLower(strings.TrimSpace(state.Provider))
	state.Model = strings.TrimSpace(state.Model)

	if state.DataDir == "" {
		state.DataDir = defaultState().DataDir
	}
	if state.ContextLimit < 0 {
		state.ContextLimit = 0
	}
	if state.ModelCacheTTLSeconds <= 0 {
		state.ModelCacheTTLSeconds = defaultState().ModelCacheTTLSeconds
	}
	if state.SessionRetentionDays <= 0 {
		state.SessionRetentionDays = defaultState().SessionRetentionDays
	}

	return state, nil
}

func SaveState(state *State) error {
	path, err := DefaultStatePath()
	if err != nil {
		return err
	}

	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}

	data, err := toml.Marshal(state)
	if err != nil {
		return err
	}

	return os.WriteFile(path, data, 0o644)
}

func defaultState() *State {
	home, _ := os.UserHomeDir()
	return &State{
		DataDir:              filepath.Join(home, ".ion", "data"),
		ContextLimit:         0,
		ModelCacheTTLSeconds: 3600,
		SessionRetentionDays: 90,
	}
}
