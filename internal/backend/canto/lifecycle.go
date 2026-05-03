package canto

import (
	"context"
	"fmt"
	"os"
	"time"

	"github.com/nijaru/canto/agent"
	"github.com/nijaru/canto/prompt"
	"github.com/nijaru/canto/runtime"
	"github.com/nijaru/canto/session"
	"github.com/nijaru/canto/tool"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/backend/canto/tools"
	ionconfig "github.com/nijaru/ion/internal/config"
	ionsession "github.com/nijaru/ion/internal/session"
)

func (b *Backend) Open(ctx context.Context) error {
	b.mu.Lock()
	defer b.mu.Unlock()

	providerName := b.Provider()
	modelName := b.Model()

	if providerName == "" {
		return fmt.Errorf(
			"No provider configured. Use /provider. Set ION_PROVIDER for scripts.",
		)
	}
	if modelName == "" {
		return fmt.Errorf("No model configured. Use /model. Set ION_MODEL for scripts.")
	}

	if b.ionStore != nil {
		if cs, ok := b.ionStore.(interface{ Canto() *session.SQLiteStore }); ok {
			b.store = cs.Canto()
		}
	}

	if b.store == nil {
		return fmt.Errorf("ion store not initialized")
	}

	p, err := providerFactory(ctx, b.cfg)
	if err != nil {
		return err
	}
	p = configureRetryProvider(p, b.cfg, b.events)
	p = observeProviderRequests(p)
	b.compactLLM = p
	b.llm = p

	cwd := b.Meta()["cwd"]
	if cwd == "" {
		cwd, _ = os.Getwd()
	}

	instructions, err := backend.BuildInstructions(buildInstructions(cwd, time.Now()), cwd)
	if err != nil {
		return err
	}

	registry := tool.NewRegistry()
	b.tools = registry

	registry.Register(tools.NewBash(cwd))
	registry.Register(&tools.Read{FileTool: *tools.NewFileTool(cwd)})
	registry.Register(&tools.Write{FileTool: *tools.NewFileTool(cwd)})
	registry.Register(&tools.Edit{FileTool: *tools.NewFileTool(cwd)})
	registry.Register(&tools.MultiEdit{FileTool: *tools.NewFileTool(cwd)})
	registry.Register(&tools.List{FileTool: *tools.NewFileTool(cwd)})
	registry.Register(&tools.Grep{SearchTool: *tools.NewSearchTool(cwd)})
	registry.Register(&tools.Glob{SearchTool: *tools.NewSearchTool(cwd)})
	if b.cfg.SkillToolMode() != "off" {
		skillsDir, err := ionconfig.DefaultSkillsDir()
		if err != nil {
			return fmt.Errorf("resolve skills dir: %w", err)
		}
		registry.Register(tools.NewReadSkill([]string{skillsDir}))
	}
	if b.cfg.SubagentToolMode() == "on" {
		personas, err := loadSubagentPersonas(b.cfg)
		if err != nil {
			return err
		}
		if err := validateSubagentPersonaTools(personas, registry); err != nil {
			return err
		}
		registry.Register(NewSubagentTool(b, personas))
	}

	requestProcessors := []prompt.RequestProcessor{
		reasoningEffortProcessor(b.cfg),
		toolVisibilityProcessor(b.policy),
	}
	b.steering = newSteeringMutator()

	agentOptions := []agent.Option{
		agent.WithRequestProcessors(requestProcessors...),
		agent.WithMutators(b.steering),
	}
	b.agent = agent.New("ion", instructions, modelName, p, registry, agentOptions...)

	b.runner = runtime.NewRunner(
		b.store,
		b.agent,
		runtime.WithOverflowRecovery(
			p.IsContextOverflow,
			func(ctx context.Context, sess *session.Session) error {
				b.events <- ionsession.StatusChanged{Status: "Compacting context..."}
				_, err := b.compactSession(ctx, sess)
				if err == nil {
					b.events <- ionsession.StatusChanged{Status: "Thinking..."}
				}
				return err
			},
			1,
		),
	)

	return nil
}

func (b *Backend) Resume(ctx context.Context, sessionID string) error {
	b.mu.Lock()
	needOpen := b.runner == nil
	b.mu.Unlock()

	if needOpen {
		return b.Open(ctx)
	}

	return nil
}

func (b *Backend) Close() error {
	b.closeOnce.Do(func() {
		b.mu.Lock()
		cancel := b.cancel
		stopWatch := b.stopWatch
		runner := b.runner
		b.mu.Unlock()

		if cancel != nil {
			cancel()
		}
		if stopWatch != nil {
			stopWatch()
		}
		if runner != nil {
			runner.Close()
		}
		b.wg.Wait()
		close(b.events)
	})
	return nil
}
