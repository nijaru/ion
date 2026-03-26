package config

import (
	"os"
	"path/filepath"

	"github.com/pelletier/go-toml/v2"
)

type State struct {
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
	legacy, err := LegacyConfigPath()
	if err != nil {
		return nil, err
	}

	if err := loadFirstExisting(state, path, legacy); err != nil {
		return nil, err
	}

	if state.DataDir == "" {
		state.DataDir = defaultState().DataDir
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
		DataDir:              filepath.Join(home, ".ion"),
		ModelCacheTTLSeconds: 3600,
		SessionRetentionDays: 90,
	}
}
