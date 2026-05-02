package main

import (
	"context"
	"errors"
	"fmt"
	"strings"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/backend/canto"
	"github.com/nijaru/ion/internal/backend/registry"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/providers"
	"github.com/nijaru/ion/internal/storage"
)

var (
	errNoProviderConfigured = errors.New(
		"No provider configured. Use /provider. Set ION_PROVIDER or --provider for scripts.",
	)
	errNoModelConfigured = errors.New(
		"No model configured. Use /model. Set ION_MODEL or --model for scripts.",
	)
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

func applyCLIConfigOverrides(
	cfg *config.Config,
	providerOverride, modelOverride, thinkingOverride string,
) {
	if cfg == nil {
		return
	}
	if strings.TrimSpace(providerOverride) != "" {
		provider := providers.ResolveID(providerOverride)
		if provider != providers.ResolveID(cfg.Provider) {
			if strings.TrimSpace(modelOverride) == "" {
				cfg.Model = ""
			}
			clearProviderScopedPresets(cfg)
		}
		cfg.Provider = provider
	}
	if model := strings.TrimSpace(modelOverride); model != "" {
		if cfg.Provider == "" {
			if provider, rest, ok := strings.Cut(model, "/"); ok {
				resolved := providers.ResolveID(provider)
				if _, exists := providers.Lookup(resolved); exists {
					cfg.Provider = resolved
					cfg.Model = strings.TrimSpace(rest)
					model = ""
				}
			}
		}
		if model != "" {
			cfg.Model = model
		}
	}
	if strings.TrimSpace(thinkingOverride) != "" {
		cfg.ReasoningEffort = thinkingOverride
	}
}

func clearProviderScopedPresets(cfg *config.Config) {
	cfg.FastModel = ""
	cfg.FastReasoningEffort = ""
	cfg.SummaryModel = ""
	cfg.SummaryReasoningEffort = ""
}

func startupRuntimeConfig(
	ctx context.Context,
	cfg *config.Config,
	sessionID string,
	explicitRuntimeOverride bool,
) (*config.Config, string, error) {
	preset := "primary"
	if !explicitRuntimeOverride && strings.TrimSpace(sessionID) == "" {
		if state, err := config.LoadState(); err == nil && state.ActivePreset != nil {
			preset = config.NormalizeActivePreset(*state.ActivePreset)
		}
	}
	if preset == "" {
		preset = "primary"
	}

	resolved, err := registry.ResolveRuntimeConfig(ctx, cfg, registry.Preset(preset))
	if err == nil {
		return resolved, preset, nil
	}
	if preset != "fast" {
		return nil, preset, err
	}

	resolved, err = registry.ResolveRuntimeConfig(ctx, cfg, registry.PresetPrimary)
	if err != nil {
		return nil, "primary", err
	}
	return resolved, "primary", nil
}

func firstNonEmpty(values ...string) string {
	for _, value := range values {
		if strings.TrimSpace(value) != "" {
			return value
		}
	}
	return ""
}

func applySessionConfigFromMetadata(
	ctx context.Context,
	store storage.Store,
	sessionID string,
	cfg *config.Config,
) error {
	if store == nil || cfg == nil || strings.TrimSpace(sessionID) == "" {
		return nil
	}
	sess, err := store.ResumeSession(ctx, sessionID)
	if err != nil {
		return fmt.Errorf("failed to inspect session %s metadata: %w", sessionID, err)
	}
	defer func() {
		_ = sess.Close()
	}()
	provider, model := splitSessionModelName(sess.Meta().Model)
	if provider == "" {
		return nil
	}
	cfg.Provider = provider
	cfg.Model = model
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
		return nil, fmt.Errorf(
			"ACP providers are deferred until the advanced integration phase",
		)
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

func splitSessionModelName(value string) (string, string) {
	value = strings.TrimSpace(value)
	if value == "" {
		return "", ""
	}
	provider, model, ok := strings.Cut(value, "/")
	if !ok {
		return strings.TrimSpace(value), ""
	}
	return strings.TrimSpace(provider), strings.TrimSpace(model)
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
