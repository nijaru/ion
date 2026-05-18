package canto

import (
	"github.com/nijaru/ion/internal/backend/canto/tools"
	"github.com/nijaru/ion/internal/providers"
)

func (b *Backend) executorEnvironmentPolicy() tools.EnvironmentPolicy {
	cfg := b.configSnapshot()
	return tools.NewEnvironmentPolicy(
		cfg.ToolEnvMode(),
		providers.CredentialEnvVars(cfg),
	)
}
