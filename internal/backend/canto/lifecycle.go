package canto

import (
	"context"
	"fmt"
	"os"
	"time"

	"github.com/nijaru/canto/agent"
	"github.com/nijaru/canto/memory"
	"github.com/nijaru/canto/prompt"
	"github.com/nijaru/canto/runtime"
	"github.com/nijaru/canto/session"
	"github.com/nijaru/canto/tool"
	"github.com/nijaru/canto/tool/mcp"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/backend/canto/tools"
	ionsession "github.com/nijaru/ion/internal/session"
)

type coreStoreProvider interface {
	CoreStore() *memory.CoreStore
}

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
		if cs, ok := b.ionStore.(coreStoreProvider); ok {
			coreStore := cs.CoreStore()
			if coreStore == nil {
				return fmt.Errorf("ion memory store not initialized")
			}
			b.coreMemory = coreStore
			b.memory = memory.NewManager(coreStore)
		}
	}

	if b.store == nil {
		return fmt.Errorf("ion store not initialized")
	}
	if b.memory == nil {
		return fmt.Errorf("ion memory manager not initialized")
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

	requestProcessors := []prompt.RequestProcessor{
		reasoningEffortProcessor(b.cfg),
	}

	agentOptions := []agent.Option{
		agent.WithRequestProcessors(requestProcessors...),
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
		clients := append([]*mcp.Client(nil), b.mcpClients...)
		memory := b.memory
		runner := b.runner
		b.mu.Unlock()

		if cancel != nil {
			cancel()
		}
		if stopWatch != nil {
			stopWatch()
		}
		for _, client := range clients {
			client.Close()
		}

		if memory != nil {
			_ = memory.Close()
		}
		if runner != nil {
			runner.Close()
		}
		b.wg.Wait()
		close(b.events)
	})
	return nil
}
