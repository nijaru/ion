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
	"github.com/nijaru/canto/llm/providers/gemini"
	"github.com/nijaru/canto/memory"
	"github.com/nijaru/canto/runtime"
	"github.com/nijaru/canto/session"
	"github.com/nijaru/canto/tool"
	"github.com/nijaru/canto/tool/mcp"
	ctools "github.com/nijaru/canto/x/tools"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/backend/canto/tools"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

type Backend struct {
	runner *runtime.Runner
	store  session.Store
	agent  *agent.BaseAgent
	events chan ionsession.Event

	ionStore storage.Store
	sess     storage.Session

	mu     sync.Mutex
	cancel context.CancelFunc

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
func (b *Backend) Name() string {
	return "canto"
}

func (b *Backend) Provider() string {
	m := os.Getenv("ION_MODEL")
	if m == "" {
		m = "openrouter minimax/minimax-m2.7"
	}
	if i := strings.IndexByte(m, ' '); i > 0 {
		return m[:i]
	}
	return m
}

func (b *Backend) Model() string {
	m := os.Getenv("ION_MODEL")
	if m == "" {
		m = "openrouter minimax/minimax-m2.7"
	}
	if i := strings.IndexByte(m, ' '); i > 0 {
		return strings.TrimSpace(m[i+1:])
	}
	return m
}

func (b *Backend) Bootstrap() backend.Bootstrap {
	return backend.Bootstrap{
		Entries: []ionsession.Entry{},
		Status:  "Ready",
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

	apiKey := os.Getenv("GEMINI_API_KEY")
	if apiKey == "" {
		apiKey = os.Getenv("GOOGLE_API_KEY")
	}
	if apiKey == "" {
		return fmt.Errorf("GEMINI_API_KEY or GOOGLE_API_KEY not set")
	}

	modelName := os.Getenv("ION_MODEL")
	if modelName == "" {
		modelName = "openrouter minimax/minimax-m2.7"
	}

	// Initialize Canto store (SQLite) from ionStore if possible
	if b.ionStore != nil {
		if cs, ok := b.ionStore.(interface{ Canto() *session.SQLiteStore }); ok {
			b.store = cs.Canto()
		}
	}

	if b.store == nil {
		home, _ := os.UserHomeDir()
		dbPath := filepath.Join(home, ".ion", "ion.db")
		os.MkdirAll(filepath.Dir(dbPath), 0755)

		store, err := session.NewSQLiteStore(dbPath)
		if err != nil {
			return fmt.Errorf("failed to open canto store: %w", err)
		}
		b.store = store
	}

	// Initialize Provider
	p := gemini.NewProvider(catwalk.Provider{
		ID:     "gemini",
		APIKey: apiKey,
	})

	// Initialize Agent
	// TODO: Load instructions from a config or file
	instructions := "You are ion, an elite AI coding assistant built on the Canto framework.\n\n" +
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

	cwd := b.Meta()["cwd"]
	if cwd == "" {
		cwd, _ = os.Getwd()
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
		// Fallback for legacy stores
		home, _ := os.UserHomeDir()
		dbPath := filepath.Join(home, ".ion", "ion.db")
		os.MkdirAll(filepath.Dir(dbPath), 0755)
		coreStore, _ = memory.NewCoreStore(dbPath)
	}

	if coreStore != nil {
		registry.Register(&ctools.RecallKnowledgeTool{Store: coreStore, Limit: 10})
		registry.Register(&ctools.MemorizeKnowledgeTool{Store: coreStore, SessionID: sid})
	}

	// Add context processors
	requestProcessors := []ccontext.RequestProcessor{
		NewFileTagProcessor(cwd),
	}
	var processors []ccontext.Processor
	if coreStore != nil {
		processors = append(processors, ccontext.KnowledgeMemory(coreStore, "", 5))
	}

	b.agent = agent.New("ion", instructions, modelName, p, registry,
		agent.WithRequestProcessors(requestProcessors...),
		agent.WithProcessors(processors...),
	)

	// Initialize Runner
	b.runner = runtime.NewRunner(b.store, b.agent)

	// Register the global Permission Policy Hook
	b.runner.Hooks.Register(hook.NewFunc("ion-policy", []hook.Event{hook.EventPreToolUse}, func(ctx context.Context, payload *hook.Payload) *hook.Result {
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
	}))

	b.events <- ionsession.StatusChanged{Status: fmt.Sprintf("Connected to %s via Canto", modelName)}
	return nil
}
func (b *Backend) Resume(ctx context.Context, sessionID string) error {
	b.mu.Lock()
	defer b.mu.Unlock()

	if b.runner == nil {
		if err := b.Open(ctx); err != nil {
			return err
		}
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

func (b *Backend) Close() error {
	b.mu.Lock()
	defer b.mu.Unlock()

	for _, client := range b.mcpClients {
		client.Close()
	}

	if b.runner != nil {
		b.runner.Close()
	}
	close(b.events)
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
