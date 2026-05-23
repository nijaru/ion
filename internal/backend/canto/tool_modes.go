package canto

import (
	"context"
	"slices"

	cantofw "github.com/nijaru/canto"
	"github.com/nijaru/canto/tool"
	"github.com/nijaru/ion/internal/config"
)

var readToolNames = map[string]struct{}{
	"glob":       {},
	"grep":       {},
	"list":       {},
	"read":       {},
	"read_skill": {},
}

var codingToolNames = map[string]struct{}{
	"bash":       {},
	"edit":       {},
	"glob":       {},
	"grep":       {},
	"list":       {},
	"read":       {},
	"read_skill": {},
	"subagent":   {},
	"write":      {},
}

func activeToolNamesForMode(mode string, registered []string) []string {
	switch config.NormalizeToolMode(mode) {
	case "all":
		return append([]string(nil), registered...)
	case "read":
		return filterToolNames(registered, readToolNames)
	default:
		return filterToolNames(registered, codingToolNames)
	}
}

func filterToolNames(registered []string, allowed map[string]struct{}) []string {
	names := make([]string, 0, len(registered))
	for _, name := range registered {
		if _, ok := allowed[name]; ok {
			names = append(names, name)
		}
	}
	return names
}

func syncCantoActiveTools(
	ctx context.Context,
	session *cantofw.Session,
	registry *tool.Registry,
	cfg *config.Config,
) error {
	if session == nil || registry == nil {
		return nil
	}
	mode := "coding"
	if cfg != nil {
		mode = cfg.ActiveToolMode()
	}
	names := activeToolNamesForMode(mode, registry.Names())
	settings, err := session.EffectiveSettings(ctx)
	if err != nil {
		return err
	}
	if !settings.HasTools && mode == "coding" {
		return nil
	}
	if settings.HasTools && slices.Equal(settings.ActiveTools, names) {
		return nil
	}
	return session.SetActiveTools(ctx, names...)
}
