package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/canto/llm"
	csession "github.com/nijaru/canto/session"
	"github.com/nijaru/ion/internal/app"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func main() {
	mode := flag.String("mode", "complete", "smoke script mode: complete, cancel, or error")
	storeRoot := flag.String("store", "", "session store directory")
	sessionID := flag.String("session-id", "", "session id to open or resume")
	resume := flag.Bool("resume", false, "resume an existing smoke session")
	startupCheck := flag.Bool("startup-check", false, "render the ready shell once and exit")
	flag.Parse()

	if err := run(*mode, *storeRoot, *sessionID, *resume, *startupCheck); err != nil {
		fmt.Fprintf(os.Stderr, "ion-tui-smoke: %v\n", err)
		os.Exit(1)
	}
}

func run(mode, storeRoot, sessionID string, resume, startupCheck bool) error {
	ctx := context.Background()
	cwd, err := os.Getwd()
	if err != nil {
		return err
	}
	if storeRoot == "" {
		tmp, err := os.MkdirTemp("", "ion-tui-smoke-store.*")
		if err != nil {
			return err
		}
		defer os.RemoveAll(tmp)
		storeRoot = tmp
	}
	storeRoot, err = filepath.Abs(storeRoot)
	if err != nil {
		return err
	}
	store, err := storage.NewCantoStore(storeRoot)
	if err != nil {
		return err
	}
	defer store.Close()
	eventStore, err := csession.NewSQLiteStore(filepath.Join(storeRoot, "sessions.db"))
	if err != nil {
		return err
	}
	defer eventStore.Close()
	stored, err := openSmokeSession(ctx, store, sessionID, cwd, resume)
	if err != nil {
		return err
	}

	smoke := newSmokeBackend(mode)
	smoke.SetCantoEventStore(eventStore)
	smoke.SetStore(store)
	smoke.SetSession(stored)
	cfg := &config.Config{
		Provider: "fake",
		Model:    "fake-model",
	}
	smoke.SetConfig(cfg)

	fmt.Println("ion v0.0.0")
	fmt.Println(cwd + " • smoke")
	fmt.Println()

	model := app.New(smoke, stored, store, cwd, "smoke", "v0.0.0", nil).
		WithConfig(cfg)
	if resume {
		startupEntries, err := stored.Entries(ctx)
		if err != nil {
			return fmt.Errorf("load startup history: %w", err)
		}
		printSmokeStartup(model.RenderEntries(startupEntries...))
		model = model.WithPrintedTranscript(len(startupEntries) > 0)
	}
	if startupCheck {
		updated, _ := model.Update(tea.WindowSizeMsg{Width: 120, Height: 32})
		ready, ok := updated.(*app.Model)
		if !ok {
			return fmt.Errorf("startup update returned %T, want *app.Model", updated)
		}
		view := ready.View().Content
		if !strings.Contains(view, "›") || !strings.Contains(view, "fake-model") {
			return fmt.Errorf("startup view missing ready shell markers")
		}
		fmt.Println("startup-check: ready shell rendered")
		smoke.Close()
		return nil
	}
	_, err = tea.NewProgram(&model).Run()
	smoke.Close()
	return err
}

type storeWithSessionID interface {
	OpenSessionWithID(ctx context.Context, id, cwd, model, branch string) (storage.Session, error)
}

func openSmokeSession(
	ctx context.Context,
	store storage.Store,
	sessionID, cwd string,
	resume bool,
) (storage.Session, error) {
	sessionID = strings.TrimSpace(sessionID)
	if resume {
		if sessionID == "" {
			return nil, fmt.Errorf("session-id is required with --resume")
		}
		return store.ResumeSession(ctx, sessionID)
	}
	if sessionID == "" {
		return store.OpenSession(ctx, cwd, "fake/fake-model", "smoke")
	}
	opener, ok := store.(storeWithSessionID)
	if !ok {
		return nil, fmt.Errorf("store does not support deterministic session ids")
	}
	return opener.OpenSessionWithID(ctx, sessionID, cwd, "fake/fake-model", "smoke")
}

func printSmokeStartup(renderedEntries []string) {
	fmt.Println("--- resumed ---")
	if len(renderedEntries) == 0 {
		fmt.Println()
		return
	}
	fmt.Println()
	fmt.Println(strings.Join(renderedEntries, "\n"))
	fmt.Println()
}

type smokeBackend struct {
	mode       string
	events     chan session.Event
	eventStore csession.Writer
	storage    storage.Session
	cfg        *config.Config

	mu     sync.Mutex
	cancel context.CancelFunc
}

func newSmokeBackend(mode string) *smokeBackend {
	return &smokeBackend{
		mode:   mode,
		events: make(chan session.Event, 64),
	}
}

func (b *smokeBackend) Name() string { return "smoke" }

func (b *smokeBackend) Provider() string {
	if b.cfg != nil && b.cfg.Provider != "" {
		return b.cfg.Provider
	}
	return "fake"
}

func (b *smokeBackend) Model() string {
	if b.cfg != nil && b.cfg.Model != "" {
		return b.cfg.Model
	}
	return "fake-model"
}

func (b *smokeBackend) ContextLimit() int { return 262144 }

func (b *smokeBackend) Bootstrap() backend.Bootstrap {
	return backend.Bootstrap{Status: "[smoke] ready"}
}

func (b *smokeBackend) Session() session.AgentSession { return b }

func (b *smokeBackend) SetStore(storage.Store) {}

func (b *smokeBackend) SetSession(s storage.Session) {
	b.storage = s
}

func (b *smokeBackend) SetCantoEventStore(store csession.Writer) {
	b.eventStore = store
}

func (b *smokeBackend) SetConfig(cfg *config.Config) {
	b.cfg = cfg
}

func (b *smokeBackend) ToolSurface() backend.ToolSurface {
	return backend.ToolSurface{
		Count:       7,
		Names:       []string{"bash", "edit", "find", "grep", "ls", "read", "write"},
		Environment: "inherit_without_provider_keys",
	}
}

func (b *smokeBackend) Open(context.Context) error { return nil }

func (b *smokeBackend) Resume(context.Context, string) error { return nil }

func (b *smokeBackend) SubmitTurn(ctx context.Context, input string) error {
	b.mu.Lock()
	if b.cancel != nil {
		b.cancel()
	}
	runCtx, cancel := context.WithCancel(ctx)
	b.cancel = cancel
	b.mu.Unlock()

	go b.runScript(runCtx, input)
	return nil
}

func (b *smokeBackend) CancelTurn(context.Context) error {
	b.mu.Lock()
	cancel := b.cancel
	b.cancel = nil
	b.mu.Unlock()
	if cancel != nil {
		cancel()
	}
	return nil
}

func (b *smokeBackend) SteerTurn(ctx context.Context, text string) (session.SteeringResult, error) {
	if strings.TrimSpace(text) == "" {
		return session.SteeringResult{}, fmt.Errorf("steering text is empty")
	}
	return session.SteeringResult{Outcome: session.SteeringAccepted}, nil
}

func (b *smokeBackend) Close() error {
	_ = b.CancelTurn(context.Background())
	return nil
}

func (b *smokeBackend) Events() <-chan session.Event { return b.events }

func (b *smokeBackend) ID() string {
	if b.storage != nil {
		return b.storage.ID()
	}
	return "smoke-session"
}

func (b *smokeBackend) Meta() map[string]string {
	return map[string]string{"model": "fake/fake-model"}
}

func (b *smokeBackend) runScript(ctx context.Context, input string) {
	switch b.mode {
	case "cancel":
		b.emit(ctx, session.UserMessage{Message: input})
		b.emit(ctx, session.TurnStarted{})
		b.emit(ctx, session.StatusChanged{Status: "[smoke] waiting for cancel"})
		<-ctx.Done()
	case "error":
		b.emit(ctx, session.UserMessage{Message: input})
		b.emit(ctx, session.TurnStarted{})
		b.emit(ctx, session.StatusChanged{Status: "[smoke] active before error"})
		if !b.sleep(ctx, 400*time.Millisecond) {
			return
		}
		b.emit(ctx, session.Error{Err: fmt.Errorf("smoke provider failure")})
	default:
		b.emit(ctx, session.UserMessage{Message: input})
		b.emit(ctx, session.TurnStarted{})
		b.emit(ctx, session.StatusChanged{Status: "[smoke] active progress"})
		if !b.sleep(ctx, 700*time.Millisecond) {
			return
		}
		b.emit(ctx, session.AgentDelta{Delta: "streaming from deterministic smoke backend"})
		if !b.sleep(ctx, 900*time.Millisecond) {
			return
		}
		b.emit(ctx, session.ToolCallStarted{
			ToolUseID: "tool-1",
			ToolName:  "bash",
			Args:      `{"command":"sleep 2; echo ion-tmux-smoke"}`,
		})
		if !b.sleep(ctx, 1200*time.Millisecond) {
			return
		}
		b.emit(ctx, session.ToolOutputDelta{
			ToolUseID: "tool-1",
			Delta:     "ion-tmux-",
		})
		if !b.sleep(ctx, 500*time.Millisecond) {
			return
		}
		b.emit(ctx, session.ToolResult{
			ToolUseID: "tool-1",
			ToolName:  "bash",
			Result:    "ion-tmux-smoke\n",
		})
		if !b.sleep(ctx, 500*time.Millisecond) {
			return
		}
		b.emit(ctx, session.AgentMessage{Message: "done"})
		b.emit(ctx, session.TurnFinished{})
	}
}

func (b *smokeBackend) emit(ctx context.Context, event session.Event) bool {
	if err := b.persistEvent(ctx, event); err != nil {
		select {
		case <-ctx.Done():
		case b.events <- session.Error{Err: fmt.Errorf("persist smoke event: %w", err)}:
		}
		return false
	}
	select {
	case <-ctx.Done():
		return false
	case b.events <- event:
		return true
	}
}

func (b *smokeBackend) persistEvent(ctx context.Context, event session.Event) error {
	if b.eventStore == nil || b.storage == nil {
		return nil
	}

	sessionID := b.storage.ID()
	switch msg := event.(type) {
	case session.UserMessage:
		if strings.TrimSpace(msg.Message) == "" {
			return nil
		}
		return b.saveCantoEvent(
			ctx,
			msg.Timestamp,
			csession.NewUserMessage(sessionID, msg.Message),
		)
	case session.TurnStarted:
		return b.saveCantoEvent(
			ctx,
			msg.Timestamp,
			csession.NewTurnStartedEvent(sessionID, csession.TurnStartedData{}),
		)
	case session.ToolCallStarted:
		call := llm.Call{ID: msg.ToolUseID, Type: "function"}
		call.Function.Name = msg.ToolName
		call.Function.Arguments = msg.Args
		if err := b.saveCantoEvent(
			ctx,
			msg.Timestamp,
			csession.NewMessage(sessionID, llm.Message{
				Role:  llm.RoleAssistant,
				Calls: []llm.Call{call},
			}),
		); err != nil {
			return err
		}
		return b.saveCantoEvent(
			ctx,
			msg.Timestamp,
			csession.NewToolStartedEvent(sessionID, csession.ToolStartedData{
				Tool:      msg.ToolName,
				ID:        msg.ToolUseID,
				Arguments: msg.Args,
			}),
		)
	case session.ToolResult:
		completed := csession.ToolCompletedData{
			Tool:   msg.ToolName,
			ID:     msg.ToolUseID,
			Output: msg.Result,
		}
		if msg.Error != nil {
			completed.Error = msg.Error.Error()
		}
		return b.saveCantoEvent(
			ctx,
			msg.Timestamp,
			csession.NewToolCompletedEvent(sessionID, completed),
		)
	case session.AgentMessage:
		if strings.TrimSpace(msg.Message) == "" && strings.TrimSpace(msg.Reasoning) == "" {
			return nil
		}
		agent := llm.TextMessage(llm.RoleAssistant, msg.Message)
		agent.Reasoning = msg.Reasoning
		return b.saveCantoEvent(ctx, msg.Timestamp, csession.NewMessage(sessionID, agent))
	case session.TurnFinished:
		return b.saveCantoEvent(
			ctx,
			msg.Timestamp,
			csession.NewTurnCompletedEvent(sessionID, csession.TurnCompletedData{}),
		)
	default:
		return nil
	}
}

func (b *smokeBackend) saveCantoEvent(
	ctx context.Context,
	timestamp time.Time,
	event csession.Event,
) error {
	if !timestamp.IsZero() {
		event.Timestamp = timestamp.UTC()
	}
	return b.eventStore.Save(ctx, event)
}

func (b *smokeBackend) sleep(ctx context.Context, d time.Duration) bool {
	timer := time.NewTimer(d)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return false
	case <-timer.C:
		return true
	}
}
