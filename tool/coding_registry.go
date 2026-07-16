package tool

import (
	"fmt"
)

type CodingToolsConfig struct {
	Workdir     string
	Environment EnvironmentPolicy
	SkillDirs   []string
	Jobs        *JobManager
}

func RegisterCodingTools(registry *Registry, cfg CodingToolsConfig) error {
	if registry == nil {
		return fmt.Errorf("tool registry is nil")
	}
	fileTool := NewFileTool(cfg.Workdir)
	searchTool := newFileSearchBase(cfg.Workdir)
	registry.Register(NewBashWithEnvironmentAndJobs(cfg.Workdir, cfg.Environment, cfg.Jobs))
	registry.Register(&Read{FileTool: *fileTool})
	registry.Register(&Write{FileTool: *fileTool})
	registry.Register(&Edit{FileTool: *fileTool})
	registry.Register(&List{FileTool: *fileTool})
	registry.Register(&Grep{fileSearchBase: *searchTool})
	registry.Register(&Find{fileSearchBase: *searchTool})
	registry.Register(NewSearchTool(registry))
	if len(cfg.SkillDirs) > 0 {
		registry.Register(NewReadSkill(cfg.SkillDirs))
	}
	return nil
}
