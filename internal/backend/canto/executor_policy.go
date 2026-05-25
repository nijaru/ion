package canto

import (
	"github.com/nijaru/ion/internal/providers"
	"github.com/nijaru/ion/internal/tools"
)

func (b *Backend) executorEnvironmentPolicy() tools.EnvironmentPolicy {
	cfg := b.configSnapshot()
	return tools.NewEnvironmentPolicy(
		cfg.ToolEnvMode(),
		providers.CredentialEnvVars(cfg),
	)
}
