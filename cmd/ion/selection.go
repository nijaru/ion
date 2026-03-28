package main

import (
	"errors"
	"fmt"
	"strings"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/backend/acp"
	"github.com/nijaru/ion/internal/backend/canto"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/providers"
)

var (
	errNoProviderConfigured = errors.New("No provider configured. Use /provider or Ctrl+P. Set ION_PROVIDER for scripts.")
	errNoModelConfigured    = errors.New("No model configured. Use /model or Ctrl+M. Set ION_MODEL for scripts.")
)

func resolveStartupConfig(cfg *config.Config) error {
	cfg.Provider = providers.ResolveID(cfg.Provider)
	cfg.Model = strings.TrimSpace(cfg.Model)
	cfg.Endpoint = strings.TrimSpace(cfg.Endpoint)
	cfg.AuthEnvVar = strings.TrimSpace(cfg.AuthEnvVar)

	if cfg.Provider == "" {
		return errNoProviderConfigured
	}
	def, ok := providers.Lookup(cfg.Provider)
	if !ok {
		return fmt.Errorf("unsupported provider %q", cfg.Provider)
	}
	if providers.RequiresEndpoint(cfg) && providers.ResolvedEndpoint(cfg) == "" {
		return fmt.Errorf("%s requires endpoint configuration", def.DisplayName)
	}

	if cfg.Model == "" {
		return errNoModelConfigured
	}

	return nil
}

func backendForProvider(provider string) (backend.Backend, error) {
	provider = providers.ResolveID(provider)
	if provider == "" {
		return nil, fmt.Errorf("no provider configured")
	}

	def, ok := providers.Lookup(provider)
	if !ok {
		return nil, fmt.Errorf("unsupported provider %q", provider)
	}
	if def.Runtime == providers.RuntimeACP {
		return acp.New(), nil
	}
	if def.Runtime == providers.RuntimeNative {
		return canto.New(), nil
	}

	return nil, fmt.Errorf("unsupported provider %q", provider)
}

func isACPProvider(provider string) bool {
	return providers.IsACP(provider)
}

func defaultACPCommand(provider string) (string, bool) {
	return providers.DefaultACPCommand(provider)
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
