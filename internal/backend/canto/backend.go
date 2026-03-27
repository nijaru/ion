package canto

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"

	"charm.land/catwalk/pkg/catwalk"
	"github.com/nijaru/canto/agent"
	ccontext "github.com/nijaru/canto/context"
	"github.com/nijaru/canto/hook"
	"github.com/nijaru/canto/llm"
	"github.com/nijaru/canto/llm/providers/anthropic"
	"github.com/nijaru/canto/llm/providers/gemini"
	"github.com/nijaru/canto/llm/providers/ollama"
	"github.com/nijaru/canto/llm/providers/openai"
	"github.com/nijaru/canto/llm/providers/openrouter"
	"github.com/nijaru/canto/runtime"
	"github.com/nijaru/canto/session"
	"github.com/nijaru/canto/tool"
	"github.com/nijaru/canto/tool/mcp"
	ctools "github.com/nijaru/canto/x/tools"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/backend/canto/tools"
	"github.com/nijaru/ion/internal/backend/registry"
	"github.com/nijaru/ion/internal/config"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

type Backend struct {
	runner *runtime.Runner
	store  session.Store
	agent  *agent.BaseAgent
	events chan ionsession.Event
	cfg    *config.Config
	llm    llm.Provider

	ionStore storage.Store
	sess     storage.Session

	mu        sync.Mutex
	cancel    context.CancelFunc
	closeOnce sync.Once

	policy     *backend.PolicyEngine
	approver   *tools.ApprovalManager
	mcpClients []*mcp.Client
}

func New() *Backend {
	return &Backend{
		events:     make(chan ionsession.Event, 100),
		policy:     backend.NewPolicyEngine(),
		approver:   tools.NewApprovalManager(),
		mcpClients: make([]*mcp.Client, 0),
	}
}

var providerFactory = newProvider

func (b *Backend) Name() string {
	return "canto"
}

func (b *Backend) SetConfig(cfg *config.Config) {
	b.cfg = cfg
}

func (b *Backend) Provider() string {
	if b.cfg != nil && b.cfg.Provider != "" {
		return b.cfg.Provider
	}
	return os.Getenv("ION_PROVIDER")
}

func (b *Backend) Model() string {
	if b.cfg != nil && b.cfg.Model != "" {
		return b.cfg.Model
	}
	m := os.Getenv("ION_MODEL")
	if i := strings.IndexByte(m, ' '); i > 0 {
		return strings.TrimSpace(m[i+1:])
	}
	return m
}

func (b *Backend) ContextLimit() int {
	if b.cfg != nil && b.cfg.ContextLimit > 0 {
		return b.cfg.ContextLimit
	}
	provider := b.Provider()
	model := b.Model()
	if meta, ok := registry.GetMetadata(context.Background(), provider, model); ok {
		return meta.ContextLimit
	}
	return 0
}

func (b *Backend) Bootstrap() backend.Bootstrap {
	status := "Ready"
	if b.sess != nil {
		if s, err := b.sess.LastStatus(context.Background()); err == nil && s != "" {
			status = s
		} else {
			// New session
			provider := b.Provider()
			model := b.Model()
			switch {
			case provider != "" && model != "":
				status = fmt.Sprintf("Connected to %s/%s via Canto", provider, model)
			case provider != "":
				status = fmt.Sprintf("Connected to %s via Canto", provider)
			case model != "":
				status = fmt.Sprintf("Connected to %s via Canto", model)
			default:
				status = "Connected to Canto"
			}
		}
	}
	return backend.Bootstrap{
		Entries: []ionsession.Entry{},
		Status:  status,
	}
}

func (b *Backend) Session() ionsession.AgentSession {
	return b
}

func (b *Backend) SetStore(s storage.Store) {
	b.ionStore = s
}

func (b *Backend) SetSession(s storage.Session) {
	b.sess = s
}

func (b *Backend) Open(ctx context.Context) error {
	b.mu.Lock()
	defer b.mu.Unlock()

	providerName := b.Provider()
	modelName := b.Model()

	if providerName == "" {
		return fmt.Errorf("No provider configured. Use /provider or Ctrl+P. Set ION_PROVIDER for scripts.")
	}
	if modelName == "" {
		return fmt.Errorf("No model configured. Use /model or Ctrl+M. Set ION_MODEL for scripts.")
	}

	// Pre-fetch metadata for the current model
	_, _ = registry.GetMetadata(ctx, providerName, modelName)

	// Initialize Canto store (SQLite) from ionStore if possible
	if b.ionStore != nil {
		if cs, ok := b.ionStore.(interface{ Canto() *session.SQLiteStore }); ok {
			b.store = cs.Canto()
		}
	}

	if b.store == nil {
		return fmt.Errorf("ion store not initialized")
	}

	p, err := providerFactory(providerName)
	if err != nil {
		return err
	}
	b.llm = p

	cwd := b.Meta()["cwd"]
	if cwd == "" {
		cwd, _ = os.Getwd()
	}

	baseInstructions := "You are ion, an elite AI coding assistant built on the Canto framework.\n\n" +
		"CORE PRINCIPLES:\n" +
		"1. Be concise, professional, and thorough.\n" +
		"2. Explore before acting. Use 'list', 'read', and 'glob' to understand the codebase context.\n" +
		"3. Work in small, verifiable steps. Apply changes and then run tests using 'bash' or 'verify'.\n" +
		"4. Streaming Output: When you run commands via 'bash', the output is streamed to the host in real-time.\n" +
		"5. Modern Idioms: Always prefer modern Go (v1.26+) patterns.\n" +
		"6. Error Handling: Always check errors and provide helpful feedback.\n" +
		"7. Approvals: Some sensitive tools may require host approval.\n" +
		"8. AUTO-VERIFICATION: After every 'edit', 'multi_edit', or 'write', you MUST run tests (e.g. 'go test ./...' or 'verify') to ensure no regressions were introduced. This is your high-fidelity verification loop.\n\n" +
		"TOOLSET:\n" +
		"- file: 'read', 'write', 'edit', 'multi_edit', 'list' for filesystem operations.\n" +
		"- search: 'grep', 'glob' for finding code patterns.\n" +
		"- recall: search long-term memory for relevant codebase patterns or cross-session insights.\n" +
		"- memorize: save important codebase insights or patterns for future sessions.\n" +
		"- system: 'bash' for running any shell command.\n" +
		"- verify: run verification commands (test, lint) and report results to the host."

	instructions, err := backend.BuildInstructions(baseInstructions, cwd)
	if err != nil {
		return err
	}

	registry := tool.NewRegistry()

	// Use standard tools; approvals are now handled globally by a PreToolUse hook.
	registry.Register(tools.NewBash(cwd))
	registry.Register(&tools.Read{FileTool: *tools.NewFileTool(cwd)})
	registry.Register(&tools.Write{FileTool: *tools.NewFileTool(cwd)})
	registry.Register(&tools.Edit{FileTool: *tools.NewFileTool(cwd)})
	registry.Register(&tools.MultiEdit{FileTool: *tools.NewFileTool(cwd)})
	registry.Register(&tools.List{FileTool: *tools.NewFileTool(cwd)})
	registry.Register(&tools.Grep{SearchTool: *tools.NewSearchTool(cwd)})
	registry.Register(&tools.Glob{SearchTool: *tools.NewSearchTool(cwd)})
	registry.Register(&tools.Verify{
		CWD: cwd,
		Callback: func(command string, passed bool, metric, output string) {
			b.events <- ionsession.VerificationResult{
				Command: command,
				Passed:  passed,
				Metric:  metric,
				Output:  output,
			}
		},
	})

	// Register Canto native memory tools
	sid := b.ID()
	if sid == "" {
		sid = "default"
	}

	coreStore := b.ionStore.CoreStore()
	if coreStore == nil {
		return fmt.Errorf("ion core store not initialized")
	}

	registry.Register(&ctools.RecallKnowledgeTool{Store: coreStore, Limit: 10})
	registry.Register(&ctools.MemorizeKnowledgeTool{Store: coreStore, SessionID: sid})

	// Add context processors
	requestProcessors := []ccontext.RequestProcessor{
		NewFileTagProcessor(cwd),
	}
	var processors []ccontext.Processor
	processors = append(processors, ccontext.KnowledgeMemory(coreStore, "", 5))

	b.agent = agent.New("ion", instructions, modelName, p, registry,
		agent.WithRequestProcessors(requestProcessors...),
		agent.WithProcessors(processors...),
	)

	// Initialize Runner
	b.runner = runtime.NewRunner(b.store, b.agent)

	// Register the global Permission Policy Hook
	b.runner.Hooks.Register(
		hook.NewFunc(
			"ion-policy",
			[]hook.Event{hook.EventPreToolUse},
			func(ctx context.Context, payload *hook.Payload) *hook.Result {
				toolName, _ := payload.Data["tool"].(string)
				args, _ := payload.Data["args"].(string)

				policy, reason := b.policy.Authorize(ctx, toolName, args)
				switch policy {
				case backend.PolicyAllow:
					return &hook.Result{Action: hook.ActionProceed}
				case backend.PolicyDeny:
					return &hook.Result{
						Action: hook.ActionBlock,
						Error:  fmt.Errorf("policy denied: %s", reason),
					}
				case backend.PolicyAsk:
					id := ionsession.ShortID()
					description := fmt.Sprintf("Tool: %s\nArgs: %s\n\n%s", toolName, args, reason)

					// Send approval request to TUI
					b.events <- ionsession.ApprovalRequest{
						RequestID:   id,
						Description: description,
						ToolName:    toolName,
						Args:        args,
					}

					// Wait for approval via ApprovalManager
					ch := b.approver.Request(id)
					select {
					case <-ctx.Done():
						return &hook.Result{Action: hook.ActionBlock, Error: ctx.Err()}
					case approved := <-ch:
						if !approved {
							return &hook.Result{
								Action: hook.ActionBlock,
								Error:  fmt.Errorf("user denied tool execution"),
							}
						}
						return &hook.Result{Action: hook.ActionProceed}
					}
				default:
					return &hook.Result{Action: hook.ActionProceed}
				}
			},
		),
	)

	return nil
}

func newProvider(providerName string) (llm.Provider, error) {
	switch strings.ToLower(strings.TrimSpace(providerName)) {
	case "anthropic":
		apiKey := os.Getenv("ANTHROPIC_API_KEY")
		if apiKey == "" {
			return nil, fmt.Errorf("ANTHROPIC_API_KEY not set")
		}
		return anthropic.NewProvider(catwalk.Provider{
			ID:     "anthropic",
			APIKey: apiKey,
		}), nil
	case "openai":
		apiKey := os.Getenv("OPENAI_API_KEY")
		if apiKey == "" {
			return nil, fmt.Errorf("OPENAI_API_KEY not set")
		}
		return openai.NewProvider(catwalk.Provider{
			ID:     "openai",
			APIKey: apiKey,
		}), nil
	case "openrouter":
		apiKey := os.Getenv("OPENROUTER_API_KEY")
		if apiKey == "" {
			return nil, fmt.Errorf("OPENROUTER_API_KEY not set")
		}
		return openrouter.NewProvider(catwalk.Provider{
			ID:     "openrouter",
			APIKey: apiKey,
		}), nil
	case "gemini":
		apiKey := os.Getenv("GEMINI_API_KEY")
		if apiKey == "" {
			apiKey = os.Getenv("GOOGLE_API_KEY")
		}
		if apiKey == "" {
			return nil, fmt.Errorf("GEMINI_API_KEY or GOOGLE_API_KEY not set")
		}
		return gemini.NewProvider(catwalk.Provider{
			ID:     "gemini",
			APIKey: apiKey,
		}), nil
	case "ollama":
		return ollama.NewProvider(catwalk.Provider{
			ID: "ollama",
		}), nil
	default:
		return nil, fmt.Errorf("unsupported canto provider %q", providerName)
	}
}

func (b *Backend) Resume(ctx context.Context, sessionID string) error {
	b.mu.Lock()
	needOpen := b.runner == nil
	b.mu.Unlock()

	if needOpen {
		return b.Open(ctx)
	}

	// In Canto, Runner.Subscribe will load the session if not present.
	// We don't need explicit Resume logic for the agent itself yet.
	return nil
}

func (b *Backend) SubmitTurn(ctx context.Context, input string) error {
	b.mu.Lock()
	defer b.mu.Unlock()

	if b.runner == nil {
		return fmt.Errorf("backend not initialized")
	}

	sessionID := b.ID()
	if sessionID == "" {
		// Fallback for standalone mode without ion storage
		sessionID = "default"
	}

	// Create a sub-context for the turn and store its cancel function.
	// This allows CancelTurn to interrupt the in-flight generation.
	turnCtx, cancel := context.WithCancel(context.Background())
	b.cancel = cancel

	// Subscribe to framework events and translate them to ion UI events.
	// Use the background context for the subscription to avoid it being
	// cancelled when the submission command context ends.
	evCh, err := b.runner.Subscribe(turnCtx, sessionID)
	if err != nil {
		cancel()
		return err
	}

	go b.translateEvents(turnCtx, evCh)

	// Run the agent turn with streaming
	go func() {
		defer cancel()
		_, err := b.runner.SendStream(turnCtx, sessionID, input, func(chunk *llm.Chunk) {
			if chunk.Reasoning != "" {
				b.events <- ionsession.ThinkingDelta{Delta: chunk.Reasoning}
			}
			if chunk.Content != "" {
				b.events <- ionsession.AssistantDelta{Delta: chunk.Content}
			}
			if chunk.Usage != nil {
				b.events <- ionsession.TokenUsage{
					Input:  chunk.Usage.InputTokens,
					Output: chunk.Usage.OutputTokens,
					Cost:   chunk.Usage.Cost,
				}
			}
		})
		if err != nil && err != context.Canceled {
			b.events <- ionsession.Error{Err: err}
		}
	}()

	return nil
}

func (b *Backend) translateEvents(ctx context.Context, evCh <-chan session.Event) {
	for {
		select {
		case <-ctx.Done():
			return
		case ev, ok := <-evCh:
			if !ok {
				return
			}

			switch ev.Type {
			case session.TurnStarted:
				b.events <- ionsession.TurnStarted{}
				b.events <- ionsession.StatusChanged{Status: "Thinking..."}
			case session.TurnCompleted:
				b.events <- ionsession.TurnFinished{}
				b.events <- ionsession.AssistantMessage{Message: ""} // Commit
				b.events <- ionsession.StatusChanged{Status: "Ready"}
			case session.ToolStarted:
				var data struct {
					Tool string `json:"tool"`
					ID   string `json:"id"`
					Args string `json:"args"`
				}
				if err := ev.UnmarshalData(&data); err == nil {
					b.events <- ionsession.ToolCallStarted{
						ToolName: data.Tool,
						Args:     data.Args,
					}
					b.events <- ionsession.StatusChanged{Status: fmt.Sprintf("Running %s...", data.Tool)}
				}
			case session.ToolCompleted:
				var data struct {
					Tool   string `json:"tool"`
					ID     string `json:"id"`
					Output string `json:"output"`
					Error  string `json:"error,omitempty"`
				}
				if err := ev.UnmarshalData(&data); err == nil {
					var execErr error
					if data.Error != "" {
						execErr = fmt.Errorf("%s", data.Error)
					}
					b.events <- ionsession.ToolResult{
						ToolName: data.Tool,
						Result:   data.Output,
						Error:    execErr,
					}
				}
			case session.ToolOutputDelta:
				var data struct {
					Delta string `json:"delta"`
				}
				if err := ev.UnmarshalData(&data); err == nil {
					b.events <- ionsession.ToolOutputDelta{Delta: data.Delta}
				}
			case session.ChildRequested:
				var data session.ChildRequestedData
				if err := ev.UnmarshalData(&data); err == nil {
					b.events <- ionsession.ChildRequested{
						AgentName: data.AgentID,
						Query:     data.Task,
					}
					b.events <- ionsession.StatusChanged{Status: fmt.Sprintf("Requesting child agent %s...", data.AgentID)}
				}
			case session.ChildStarted:
				var data session.ChildStartedData
				if err := ev.UnmarshalData(&data); err == nil {
					b.events <- ionsession.ChildStarted{
						AgentName: data.AgentID,
						SessionID: data.ChildSessionID,
					}
					b.events <- ionsession.StatusChanged{Status: fmt.Sprintf("Child agent %s started (%s)", data.AgentID, data.ChildSessionID)}
				}

			case session.ChildProgressed:
				var data session.ChildProgressedData
				if err := ev.UnmarshalData(&data); err == nil {
					b.events <- ionsession.ChildDelta{
						AgentName: data.ChildID,
						Delta:     data.Message,
					}
					b.events <- ionsession.StatusChanged{Status: fmt.Sprintf("Child agent %s: %s", data.ChildID, data.Message)}
				}
			case session.ChildCompleted:
				var data session.ChildCompletedData
				if err := ev.UnmarshalData(&data); err == nil {
					b.events <- ionsession.ChildCompleted{
						AgentName: data.ChildID,
						Result:    data.Summary,
					}
					b.events <- ionsession.StatusChanged{Status: "Ready"}
				}
			case session.ChildFailed:
				var data session.ChildFailedData
				if err := ev.UnmarshalData(&data); err == nil {
					b.events <- ionsession.ChildFailed{
						AgentName: data.ChildID,
						Error:     data.Error,
					}
					b.events <- ionsession.StatusChanged{Status: "Ready"}
				}
			case session.ChildCanceled:
				var data session.ChildCanceledData
				if err := ev.UnmarshalData(&data); err == nil {
					b.events <- ionsession.ChildFailed{
						AgentName: data.ChildID,
						Error:     "Canceled: " + data.Reason,
					}
					b.events <- ionsession.StatusChanged{Status: "Ready"}
				}
			}
		}
	}
}

func (b *Backend) CancelTurn(ctx context.Context) error {
	b.mu.Lock()
	defer b.mu.Unlock()

	if b.cancel != nil {
		b.cancel()
		b.cancel = nil
	}
	return nil
}

func (b *Backend) Approve(ctx context.Context, requestID string, approved bool) error {
	b.approver.Approve(requestID, approved)
	return nil
}

func (b *Backend) RegisterMCPServer(ctx context.Context, command string, args ...string) error {
	b.mu.Lock()
	defer b.mu.Unlock()

	if b.agent == nil {
		return fmt.Errorf("backend not initialized")
	}

	client, err := mcp.NewStdioClient(ctx, command, args...)
	if err != nil {
		return fmt.Errorf("failed to connect to MCP server: %w", err)
	}

	tools, err := client.DiscoverTools(ctx)
	if err != nil {
		client.Close()
		return fmt.Errorf("failed to discover tools: %w", err)
	}

	for _, t := range tools {
		b.agent.Tools.Register(t)
	}

	b.mcpClients = append(b.mcpClients, client)
	b.events <- ionsession.StatusChanged{Status: fmt.Sprintf("Registered %d MCP tools from %s", len(tools), command)}
	return nil
}

func (b *Backend) Compact(ctx context.Context) (bool, error) {
	b.mu.Lock()
	store := b.store
	provider := b.llm
	sessionID := b.ID()
	model := b.Model()
	maxTokens := b.ContextLimit()
	b.mu.Unlock()

	if store == nil {
		return false, fmt.Errorf("backend store not initialized")
	}
	if provider == nil {
		return false, fmt.Errorf("backend provider not initialized")
	}
	if sessionID == "" {
		return false, fmt.Errorf("session not initialized")
	}
	if model == "" {
		return false, fmt.Errorf("model not configured")
	}
	if maxTokens <= 0 {
		return false, fmt.Errorf("context limit unavailable for model %s", model)
	}

	sess, err := store.Load(ctx, sessionID)
	if err != nil {
		return false, err
	}

	dataDir, err := config.DefaultDataDir()
	if err != nil {
		return false, err
	}

	result, err := ccontext.CompactSession(ctx, provider, model, sess, ccontext.CompactOptions{
		MaxTokens:  maxTokens,
		OffloadDir: filepath.Join(dataDir, "artifacts"),
	})
	if err != nil {
		return false, err
	}
	return result.Compacted, nil
}

func (b *Backend) Close() error {
	b.closeOnce.Do(func() {
		b.mu.Lock()
		defer b.mu.Unlock()

		for _, client := range b.mcpClients {
			client.Close()
		}

		if b.runner != nil {
			b.runner.Close()
		}
		close(b.events)
	})
	return nil
}

func (b *Backend) Events() <-chan ionsession.Event {
	return b.events
}

func (b *Backend) ID() string {
	if b.sess != nil {
		return b.sess.ID()
	}
	return ""
}

func (b *Backend) Meta() map[string]string {
	if b.sess != nil {
		m := b.sess.Meta()
		return map[string]string{
			"model":  m.Model,
			"branch": m.Branch,
			"cwd":    m.CWD,
		}
	}
	return nil
}
