package main

import (
	"fmt"
	"strings"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/backend/acp"
	"github.com/nijaru/ion/internal/backend/canto"
	"github.com/nijaru/ion/internal/config"
)

var acpProviders = map[string]string{
	"claude-pro":      "claude --acp",
	"gemini-advanced": "gemini --acp",
	"gh-copilot":      "gh copilot --acp",
	"chatgpt":         "codex --acp",
	"codex":           "codex --acp",
}

var cantoProviders = map[string]struct{}{
	"anthropic":  {},
	"openai":     {},
	"openrouter": {},
	"gemini":     {},
	"ollama":     {},
}

func resolveStartupConfig(cfg *config.Config) error {
	cfg.Provider = strings.ToLower(strings.TrimSpace(cfg.Provider))
	cfg.Model = strings.TrimSpace(cfg.Model)

	switch {
	case cfg.Provider == "" && cfg.Model == "":
		cfg.Provider = config.DefaultProvider
		cfg.Model = config.DefaultModel
	case cfg.Provider == "":
		cfg.Provider = config.DefaultProvider
	}

	if isACPProvider(cfg.Provider) {
		return nil
	}

	if cfg.Provider == config.DefaultProvider && cfg.Model == "" {
		cfg.Model = config.DefaultModel
		return nil
	}

	if cfg.Model == "" {
		return fmt.Errorf("no model configured: set model in ~/.ion/state.toml or ION_MODEL")
	}

	return nil
}

func backendForProvider(provider string) (backend.Backend, error) {
	provider = strings.ToLower(strings.TrimSpace(provider))
	if provider == "" {
		return nil, fmt.Errorf("no provider configured")
	}

	if _, ok := acpProviders[provider]; ok {
		return acp.New(), nil
	}
	if _, ok := cantoProviders[provider]; ok {
		return canto.New(), nil
	}

	return nil, fmt.Errorf("unsupported provider %q", provider)
}

func isACPProvider(provider string) bool {
	provider = strings.ToLower(strings.TrimSpace(provider))
	_, ok := acpProviders[provider]
	return ok
}

func defaultACPCommand(provider string) (string, bool) {
	provider = strings.ToLower(strings.TrimSpace(provider))
	command, ok := acpProviders[provider]
	if !ok || command == "" {
		return "", false
	}
	return command, true
}

func sessionModelName(provider, model string) string {
	provider = strings.TrimSpace(provider)
	model = strings.TrimSpace(model)

	switch {
	case provider == "":
		return model
	case model == "":
		return provider
	default:
		return provider + "/" + model
	}
}
