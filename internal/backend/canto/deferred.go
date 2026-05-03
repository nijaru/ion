package canto

import (
	"fmt"
	"strings"

	"github.com/nijaru/canto/tool"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/subagents"
)

func loadSubagentPersonas(cfg *config.Config) ([]subagents.Persona, error) {
	path := ""
	if cfg != nil {
		path = strings.TrimSpace(cfg.SubagentsPath)
	}
	if path == "" {
		defaultPath, err := config.DefaultSubagentsDir()
		if err != nil {
			return nil, err
		}
		path = defaultPath
	}
	custom, err := subagents.LoadDir(path)
	if err != nil {
		return nil, fmt.Errorf("load subagent personas: %w", err)
	}
	return subagents.Merge(subagents.Builtins(), custom), nil
}

func validateSubagentPersonaTools(personas []subagents.Persona, registry *tool.Registry) error {
	for _, persona := range personas {
		for _, toolName := range persona.Tools {
			if _, ok := registry.Get(toolName); !ok {
				return fmt.Errorf(
					"subagent persona %s references unknown tool %q",
					persona.Name,
					toolName,
				)
			}
		}
	}
	return nil
}
