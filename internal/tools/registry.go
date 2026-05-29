package tools

import (
	"fmt"

	"github.com/nijaru/ion/internal/tool"
)

type CodingToolsConfig struct {
	Workdir     string
	Environment EnvironmentPolicy
	SkillDirs   []string
}

func RegisterCodingTools(registry *tool.Registry, cfg CodingToolsConfig) error {
	if registry == nil {
		return fmt.Errorf("tool registry is nil")
	}
	fileTool := NewFileTool(cfg.Workdir)
	searchTool := NewSearchTool(cfg.Workdir)
	registry.Register(NewBashWithEnvironment(cfg.Workdir, cfg.Environment))
	registry.Register(&Read{FileTool: *fileTool})
	registry.Register(&Write{FileTool: *fileTool})
	registry.Register(&Edit{FileTool: *fileTool})
	registry.Register(&List{FileTool: *fileTool})
	registry.Register(&Grep{SearchTool: *searchTool})
	registry.Register(&Find{SearchTool: *searchTool})
	if len(cfg.SkillDirs) > 0 {
		registry.Register(NewReadSkill(cfg.SkillDirs))
	}
	return nil
}
