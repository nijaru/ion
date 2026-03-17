package canto

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"sync"

	"charm.land/catwalk/pkg/catwalk"
	"github.com/nijaru/canto/agent"
	"github.com/nijaru/canto/llm"
	"github.com/nijaru/canto/llm/providers/gemini"
	"github.com/nijaru/canto/runtime"
	"github.com/nijaru/canto/session"
	"github.com/nijaru/canto/tool"
	"github.com/nijaru/ion/internal/backend"
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
}

func New() *Backend {
	return &Backend{
		events: make(chan ionsession.Event, 100),
	}
}

func (b *Backend) Name() string {
	return "canto"
}

func (b *Backend) Bootstrap() backend.Bootstrap {
	return backend.Bootstrap{
		Entries: []ionsession.Entry{
			{Role: ionsession.RoleSystem, Content: "Canto Agent Backend (Gemini)"},
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
	instructions := "You are ion, a fast, lightweight coding agent. " +
		"Use tools to explore the codebase, run tests, and apply changes. " +
		"Be concise and professional."
	
	b.agent = agent.New("ion", instructions, modelName, p, tool.NewRegistry())
	
	// Initialize Runner
	b.runner = runtime.NewRunner(b.store, b.agent)

	b.events <- ionsession.EventStatusChanged{Status: fmt.Sprintf("Connected to %s via Canto", modelName)}
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

	// Subscribe to framework events and translate them to ion UI events
	evCh, err := b.runner.Subscribe(ctx, sessionID)
	if err != nil {
		return err
	}

	go b.translateEvents(ctx, evCh)

	// Run the agent turn with streaming
	go func() {
		_, err := b.runner.SendStream(ctx, sessionID, input, func(chunk *llm.Chunk) {
			if chunk.Content != "" {
				b.events <- ionsession.EventAssistantDelta{Delta: chunk.Content}
			}
		})
		if err != nil {
			b.events <- ionsession.EventError{Error: err}
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
			case session.EventTypeTurnStarted:
				b.events <- ionsession.EventTurnStarted{}
				b.events <- ionsession.EventStatusChanged{Status: "Thinking..."}
			case session.EventTypeTurnCompleted:
				b.events <- ionsession.EventTurnFinished{}
				b.events <- ionsession.EventAssistantMessage{Message: ""} // Commit
				b.events <- ionsession.EventStatusChanged{Status: "Ready"}
			case session.EventTypeToolExecutionStarted:
				var data struct {
					Tool string `json:"tool"`
					ID   string `json:"id"`
					Args string `json:"args"`
				}
				if err := ev.UnmarshalData(&data); err == nil {
					b.events <- ionsession.EventToolCallStarted{
						ToolName: data.Tool,
						Args:     data.Args,
					}
					b.events <- ionsession.EventStatusChanged{Status: fmt.Sprintf("Running %s...", data.Tool)}
				}
			case session.EventTypeToolExecutionCompleted:
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
					b.events <- ionsession.EventToolResult{
						ToolName: data.Tool,
						Result:   data.Output,
						Error:    execErr,
					}
				}
			case session.EventTypeToolOutputDelta:
				var data struct {
					Delta string `json:"delta"`
				}
				if err := ev.UnmarshalData(&data); err == nil {
					// Map tool delta to assistant delta for real-time progress in UI
					b.events <- ionsession.EventAssistantDelta{Delta: data.Delta}
				}
			}
		}
	}
}

func (b *Backend) CancelTurn(ctx context.Context) error {
	// TODO: Implement cancellation in Canto Runner
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
