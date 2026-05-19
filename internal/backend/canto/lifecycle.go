package canto

import (
	"context"
	"fmt"
	"os"
	"time"

	cantofw "github.com/nijaru/canto"
	"github.com/nijaru/canto/agent"
	"github.com/nijaru/canto/runtime"
	"github.com/nijaru/canto/session"
	"github.com/nijaru/canto/tool"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/backend/canto/tools"
	ionconfig "github.com/nijaru/ion/internal/config"
	ionsession "github.com/nijaru/ion/internal/session"
)

func (b *Backend) Open(ctx context.Context) error {
	providerName := b.Provider()
	modelName := b.Model()
	cfg := b.configSnapshot()

	if providerName == "" {
		return fmt.Errorf(
			"No provider configured. Use /provider. Set ION_PROVIDER for scripts.",
		)
	}
	if modelName == "" {
		return fmt.Errorf("No model configured. Use /model. Set ION_MODEL for scripts.")
	}

	b.mu.Lock()
	ionStore := b.ionStore
	store := b.store
	cwd := ""
	if b.sess != nil {
		cwd = b.sess.Meta().CWD
	}
	b.mu.Unlock()

	if ionStore != nil {
		if cs, ok := ionStore.(interface{ Canto() *session.SQLiteStore }); ok {
			store = cs.Canto()
		}
	}

	if store == nil {
		return fmt.Errorf("ion store not initialized")
	}

	p, err := providerFactory(ctx, cfg)
	if err != nil {
		return err
	}
	p = configureRetryProvider(p, cfg, b.events)
	p = useProviderRetryOnly(p)
	p = observeProviderRequests(p)

	if cwd == "" {
		cwd, _ = os.Getwd()
	}

	instructions, err := backend.BuildInstructions(buildInstructions(cwd, time.Now()), cwd)
	if err != nil {
		return err
	}

	registry := tool.NewRegistry()

	registry.Register(tools.NewBashWithEnvironment(cwd, b.executorEnvironmentPolicy()))
	registry.Register(&tools.Read{FileTool: *tools.NewFileTool(cwd)})
	registry.Register(&tools.Write{FileTool: *tools.NewFileTool(cwd)})
	registry.Register(&tools.Edit{FileTool: *tools.NewFileTool(cwd)})
	registry.Register(&tools.MultiEdit{FileTool: *tools.NewFileTool(cwd)})
	registry.Register(&tools.List{FileTool: *tools.NewFileTool(cwd)})
	registry.Register(&tools.Grep{SearchTool: *tools.NewSearchTool(cwd)})
	registry.Register(&tools.Glob{SearchTool: *tools.NewSearchTool(cwd)})
	if cfg.SkillToolMode() != "off" {
		skillsDir, err := ionconfig.DefaultSkillsDir()
		if err != nil {
			return fmt.Errorf("resolve skills dir: %w", err)
		}
		registry.Register(tools.NewReadSkill([]string{skillsDir}))
	}
	if cfg.SubagentToolMode() == "on" {
		personas, err := loadSubagentPersonas(cfg)
		if err != nil {
			return err
		}
		if err := validateSubagentPersonaTools(personas, registry); err != nil {
			return err
		}
		registry.Register(NewSubagentTool(b, personas))
	}

	steering := newSteeringMutator()

	agentOptions := []agent.Option{
		agent.WithRequestProcessors(dynamicReasoningEffortProcessor(b.configSnapshot)),
		agent.WithMutators(steering),
	}
	harness, err := cantofw.NewHarness("ion").
		Instructions(instructions).
		Model(modelName).
		Provider(p).
		Registry(registry).
		SessionStore(store).
		AgentOptions(agentOptions...).
		RuntimeOptions(runtime.WithOverflowRecovery(
			p.IsContextOverflow,
			func(ctx context.Context, sess *session.Session) error {
				b.events <- ionsession.StatusChanged{
					Base:   ionsession.BaseNow(),
					Status: "Compacting context...",
				}
				_, err := b.compactSession(ctx, sess)
				if err == nil {
					b.events <- ionsession.StatusChanged{
						Base:   ionsession.BaseNow(),
						Status: "Thinking...",
					}
				}
				return err
			},
			1,
		)).
		Build()
	if err != nil {
		return err
	}

	b.mu.Lock()
	b.store = store
	b.compactLLM = p
	b.llm = p
	b.tools = registry
	b.steering = steering
	b.harness = harness
	b.mu.Unlock()

	return nil
}

func (b *Backend) Resume(ctx context.Context, sessionID string) error {
	b.mu.Lock()
	needOpen := b.harness == nil
	b.mu.Unlock()

	if needOpen {
		return b.Open(ctx)
	}

	return nil
}

func (b *Backend) Close() error {
	b.closeOnce.Do(func() {
		b.mu.Lock()
		cancel := b.turn.cancel
		harness := b.harness
		b.mu.Unlock()

		if cancel != nil {
			cancel()
		}
		if harness != nil {
			harness.Close()
		}
		b.wg.Wait()
		close(b.events)
	})
	return nil
}
