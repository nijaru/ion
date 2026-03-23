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
	"github.com/nijaru/canto/llm"
	"github.com/nijaru/canto/llm/providers/gemini"
	"github.com/nijaru/canto/runtime"
	"github.com/nijaru/canto/session"
	"github.com/nijaru/canto/tool"
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

	approver *tools.ApprovalManager
}

func New() *Backend {
	return &Backend{
		events:   make(chan ionsession.Event, 100),
		approver: tools.NewApprovalManager(),
	}
}

func (b *Backend) Name() string {
	return "canto"
}

func (b *Backend) Bootstrap() backend.Bootstrap {
	return backend.Bootstrap{
		Entries: []ionsession.Entry{
			{Role: ionsession.System, Content: "Canto Agent Backend (Gemini)"},
		},
		Status: "Initializing Canto runtime...",
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
		modelName = "gemini-2.0-flash"
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
		"3. Work in small, verifiable steps. Apply changes and then run tests using 'bash'.\n" +
		"4. Streaming Output: When you run commands via 'bash', the output is streamed to the host in real-time. " +
		"This allows you to see progress for long-running tasks like 'go test ./...'.\n" +
		"5. Modern Idioms: Always prefer modern Go (v1.26+) patterns. Use 'slices', 'maps', and 'iter' packages. " +
		"Prefer 'sync.WaitGroup.Go' for concurrency.\n" +
		"6. Error Handling: Always check errors and provide helpful feedback. If a tool fails, explain why and recommend a fix.\n" +
		"7. Approvals: Some sensitive tools may require host approval. If prompted, wait for the user to 'y/n' before proceeding.\n\n" +
		"TOOLSET:\n" +
		"- file: 'read', 'write', 'edit', 'list' for filesystem operations.\n" +
		"- search: 'grep', 'glob' for finding code patterns.\n" +
		"- recall: search long-term memory for relevant codebase patterns or cross-session insights.\n" +
		"- memorize: save important codebase insights or patterns for future sessions.\n" +
		"- system: 'bash' for running any shell command."
	
	cwd := b.Meta()["cwd"]
	if cwd == "" {
		cwd, _ = os.Getwd()
	}

	registry := tool.NewRegistry()

	// Sensitive tools wrapped with ApprovingTool
	bash := tools.NewBash(cwd)
	registry.Register(&tools.ApprovingTool{
		Tool:    bash,
		Manager: b.approver,
		Callback: func(id, description string) {
			b.events <- ionsession.ApprovalRequest{
				RequestID:   id,
				Description: description,
			}
		},
	})

	registry.Register(&tools.Read{FileTool: *tools.NewFileTool(cwd)})
	registry.Register(&tools.Write{FileTool: *tools.NewFileTool(cwd)})
	registry.Register(&tools.Edit{FileTool: *tools.NewFileTool(cwd)})
	registry.Register(&tools.MultiEdit{FileTool: *tools.NewFileTool(cwd)})
	registry.Register(&tools.List{FileTool: *tools.NewFileTool(cwd)})
	registry.Register(&tools.Grep{SearchTool: *tools.NewSearchTool(cwd)})
	registry.Register(&tools.Glob{SearchTool: *tools.NewSearchTool(cwd)})

	// Register Canto native memory tools
	sid := b.ID()
	if sid == "" {
		sid = "default"
	}
	registry.Register(&ctools.RecallKnowledgeTool{Store: b.ionStore.CoreStore(), Limit: 10})
	registry.Register(&ctools.MemorizeKnowledgeTool{Store: b.ionStore.CoreStore(), SessionID: sid})

	// Add context processors
	processors := []ccontext.RequestProcessor{
		NewFileTagProcessor(cwd),
		ccontext.KnowledgeMemory(b.ionStore.CoreStore(), "", 5),
	}

	b.agent = agent.New("ion", instructions, modelName, p, registry,
		agent.WithRequestProcessors(processors...),
	)
	
	// Initialize Runner
	b.runner = runtime.NewRunner(b.store, b.agent)

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
				var data struct {
					Agent string `json:"agent"`
					Query string `json:"query"`
				}
				if err := ev.UnmarshalData(&data); err == nil {
					b.events <- ionsession.StatusChanged{Status: fmt.Sprintf("Requesting child agent %s...", data.Agent)}
				}
			case session.ChildStarted:
				var data struct {
					Agent     string `json:"agent"`
					SessionID string `json:"session_id"`
				}
				if err := ev.UnmarshalData(&data); err == nil {
					b.events <- ionsession.StatusChanged{Status: fmt.Sprintf("Child agent %s started (%s)", data.Agent, data.SessionID)}
				}
			case session.ChildCompleted:
				var data struct {
					Agent  string `json:"agent"`
					Result string `json:"result"`
				}
				if err := ev.UnmarshalData(&data); err == nil {
					b.events <- ionsession.StatusChanged{Status: fmt.Sprintf("Child agent %s completed", data.Agent)}
				}
			case session.ChildFailed, session.ChildCanceled:
				var data struct {
					Agent string `json:"agent"`
					Error string `json:"error"`
				}
				if err := ev.UnmarshalData(&data); err == nil {
					b.events <- ionsession.StatusChanged{Status: fmt.Sprintf("Child agent %s stopped: %s", data.Agent, data.Error)}
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
func (b *Backend) Close() error {
	b.mu.Lock()
	defer b.mu.Unlock()

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
