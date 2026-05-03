package canto

import (
	"github.com/nijaru/ion/internal/backend/canto/tools"
	"github.com/nijaru/ion/internal/providers"
)

func (b *Backend) executorEnvironmentPolicy() tools.EnvironmentPolicy {
	return tools.NewEnvironmentPolicy(
		b.cfg.ToolEnvMode(),
		providers.CredentialEnvVars(b.cfg),
	)
}
