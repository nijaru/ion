package smoke

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
	"github.com/nijaru/ion/app"
	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
	csession "github.com/nijaru/ion/session"
)

func main() {
	mode := flag.String(
		"mode",
		"complete",
		"smoke script mode: complete, controls, files, markdown, session-picker, cancel, or error",
	)
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
	store, err := session.NewCantoStore(storeRoot)
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
	if mode == "session-picker" {
		if err := seedSmokeSessionPicker(ctx, store, cwd); err != nil {
			return err
		}
	}

	smoke := newSmokeBackend(mode)
	smoke.SetCantoEventStore(eventStore)
	smoke.SetStore(store)
	smoke.SetSession(stored)
	cfg := &config.Config{
		Provider: "fake",
		Model:    "fake-model",
	}
	if mode == "controls" {
		cfg = &config.Config{}
	}
	smoke.SetConfig(cfg)

	fmt.Println("ion v0.0.0")
	fmt.Println(cwd + " • smoke")
	fmt.Println()

	model := app.New(smoke, stored, store, cwd, "smoke", "v0.0.0", nil).
		WithConfig(cfg)
	if mode == "session-picker" {
		model = model.WithSessionPicker()
	}
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
	OpenSessionWithID(ctx context.Context, id, cwd, model, branch string) (session.SessionHandle, error)
}

func openSmokeSession(
	ctx context.Context,
	store session.SessionStore,
	sessionID, cwd string,
	resume bool,
) (session.SessionHandle, error) {
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

func seedSmokeSessionPicker(ctx context.Context, store session.SessionStore, cwd string) error {
	opener, ok := store.(storeWithSessionID)
	if !ok {
		return fmt.Errorf("store does not support deterministic session ids")
	}
	fixtures := []struct {
		id      string
		title   string
		preview string
	}{
		{
			id:      "ion-tmux-session-picker-primary",
			title:   "Resume deterministic picker",
			preview: "read ai/status.md",
		},
		{
			id:      "ion-tmux-session-picker-alternate",
			title:   "Alternate deterministic branch",
			preview: "fix tui frame",
		},
	}
	for _, fixture := range fixtures {
		sess, err := opener.OpenSessionWithID(ctx, fixture.id, cwd, "fake/fake-model", "smoke")
		if err != nil {
			return fmt.Errorf("seed session %s: %w", fixture.id, err)
		}
		if err := sess.Close(); err != nil {
			return fmt.Errorf("close seed session %s: %w", fixture.id, err)
		}
		if err := store.UpdateSession(ctx, session.SessionInfo{
			ID:          fixture.id,
			Title:       fixture.title,
			LastPreview: fixture.preview,
			Model:       "fake/fake-model",
			Branch:      "smoke",
		}); err != nil {
			return fmt.Errorf("update seed session %s: %w", fixture.id, err)
		}
	}
	return nil
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
	events     chan session.AgentEvent
	eventStore csession.Writer
	storage    session.SessionHandle
	cfg        *config.Config

	mu     sync.Mutex
	cancel context.CancelFunc
}

func newSmokeBackend(mode string) *smokeBackend {
	return &smokeBackend{
		mode:   mode,
		events: make(chan session.AgentEvent, 64),
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

func (b *smokeBackend) Bootstrap() app.Bootstrap {
	return app.Bootstrap{Status: "[smoke] ready"}
}

func (b *smokeBackend) Session() session.AgentSession { return b }

func (b *smokeBackend) SetStore(session.SessionStore) {}

func (b *smokeBackend) SetSession(s session.SessionHandle) {
	b.storage = s
}

func (b *smokeBackend) SetCantoEventStore(store csession.Writer) {
	b.eventStore = store
}

func (b *smokeBackend) SetConfig(cfg *config.Config) {
	b.cfg = cfg
}

func (b *smokeBackend) ToolSurface() app.ToolSurface {
	return app.ToolSurface{
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

func (b *smokeBackend) Events() <-chan session.AgentEvent { return b.events }

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
		b.emit(ctx, session.TurnStart{})
		b.emit(ctx, session.StatusChange{Status: "[smoke] waiting for cancel"})
		<-ctx.Done()
	case "error":
		b.emit(ctx, session.UserMessage{Message: input})
		b.emit(ctx, session.TurnStart{})
		b.emit(ctx, session.StatusChange{Status: "[smoke] active before error"})
		if !b.sleep(ctx, 400*time.Millisecond) {
			return
		}
		b.emit(ctx, session.TurnEnd{Base: session.BaseNow(), Error: fmt.Errorf("smoke provider failure")})
	case "controls":
		b.runActiveControlsScript(ctx, input)
	case "files":
		b.runFileToolScript(ctx, input)
	case "markdown":
		b.runMarkdownScript(ctx, input)
	default:
		b.emit(ctx, session.UserMessage{Message: input})
		b.emit(ctx, session.TurnStart{})
		b.emit(ctx, session.StatusChange{Status: "[smoke] active progress"})
		if !b.sleep(ctx, 700*time.Millisecond) {
			return
		}
		b.emit(ctx, session.AgentDelta{Delta: "streaming from deterministic smoke backend"})
		if !b.sleep(ctx, 900*time.Millisecond) {
			return
		}
		b.emit(ctx, session.ToolCallStart{
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
		b.emit(ctx, session.ToolCallEnd{
			ToolUseID: "tool-1",
			ToolName:  "bash",
			Result:    "ion-tmux-smoke\n",
		})
		if !b.sleep(ctx, 500*time.Millisecond) {
			return
		}
		b.emit(ctx, session.AgentMessage{Message: "done"})
		b.emit(ctx, session.TurnEnd{})
	}
}

func (b *smokeBackend) runMarkdownScript(ctx context.Context, input string) {
	b.emit(ctx, session.UserMessage{Message: input})
	b.emit(ctx, session.TurnStart{})
	b.emit(ctx, session.StatusChange{Status: "[smoke] markdown stream"})
	if !b.sleep(ctx, 200*time.Millisecond) {
		return
	}
	b.emit(ctx, session.AgentDelta{Delta: strings.Join([]string{
		"Here's the summary of both status files:",
		"",
		"## Canto (`../canto/ai/STATUS.md`)",
		"",
		"**Key facts:**",
	}, "\n")})
	if !b.sleep(ctx, 500*time.Millisecond) {
		return
	}
	b.emit(ctx, session.AgentMessage{Message: strings.Join([]string{
		"Here's the summary of both status files:",
		"",
		"## Canto (`../canto/ai/STATUS.md`)",
		"",
		"**Key facts:**",
		"",
		"- The markdown stream should not be committed raw.",
		"- A long line with a verylongunbrokenidentifierthatshouldwrapbeforetheterminaldoes must still fit the shell width.",
		"",
		"Bottom line: formatted final output should be the only committed assistant entry.",
	}, "\n")})
	b.emit(ctx, session.TurnEnd{})
}

func (b *smokeBackend) runActiveControlsScript(ctx context.Context, input string) {
	b.emit(ctx, session.UserMessage{Message: input})
	b.emit(ctx, session.TurnStart{})
	b.emit(ctx, session.StatusChange{Status: "[smoke] active controls"})
	if !b.sleep(ctx, 9*time.Second) {
		return
	}
	b.emit(ctx, session.AgentMessage{Message: "controls done"})
	b.emit(ctx, session.TurnEnd{})
}

func (b *smokeBackend) runFileToolScript(ctx context.Context, input string) {
	b.emit(ctx, session.UserMessage{Message: input})
	b.emit(ctx, session.TurnStart{})
	b.emit(ctx, session.StatusChange{Status: "[smoke] file tool rows"})
	if !b.sleep(ctx, 200*time.Millisecond) {
		return
	}
	tools := []struct {
		id     string
		name   string
		args   string
		result string
	}{
		{
			id:     "read-1",
			name:   "read",
			args:   `{"path":"ai/STATUS.md"}`,
			result: "phase: p1\nfocus: smoke\n",
		},
		{
			id:     "find-1",
			name:   "find",
			args:   `{"pattern":"ai/*.md"}`,
			result: "ai/STATUS.md\nai/PLAN.md\n",
		},
		{
			id:     "grep-1",
			name:   "grep",
			args:   `{"pattern":"needle","path":"ai"}`,
			result: "ai/STATUS.md:2:needle path\n",
		},
		{
			id:     "ls-1",
			name:   "ls",
			args:   `{"path":"ai"}`,
			result: "STATUS.md\nPLAN.md\n",
		},
		{
			id:     "write-1",
			name:   "write",
			args:   `{"path":"notes/todo.md"}`,
			result: "Wrote notes/todo.md.\n",
		},
		{
			id:     "edit-1",
			name:   "edit",
			args:   `{"path":"src/main.go"}`,
			result: "Applied 1 edit(s).\n- old\n+ new\n",
		},
	}
	for _, tool := range tools {
		b.emit(ctx, session.ToolCallStart{
			ToolUseID: tool.id,
			ToolName:  tool.name,
			Args:      tool.args,
		})
		if !b.sleep(ctx, 150*time.Millisecond) {
			return
		}
		b.emit(ctx, session.ToolCallEnd{
			ToolUseID: tool.id,
			ToolName:  tool.name,
			Result:    tool.result,
		})
		if !b.sleep(ctx, 100*time.Millisecond) {
			return
		}
	}
	b.emit(ctx, session.AgentMessage{Message: "file tools done"})
	b.emit(ctx, session.TurnEnd{})
}

func (b *smokeBackend) emit(ctx context.Context, event session.AgentEvent) bool {
	if err := b.persistEvent(ctx, event); err != nil {
		select {
		case <-ctx.Done():
		case b.events <- session.TurnEnd{Base: session.BaseNow(), Error: fmt.Errorf("persist smoke event: %w", err)}:
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

func (b *smokeBackend) persistEvent(ctx context.Context, event session.AgentEvent) error {
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
			csession.NewMessage(sessionID, llm.TextMessage(llm.RoleUser, msg.Message)),
		)
	case session.TurnStart:
		return b.saveCantoEvent(
			ctx,
			msg.Timestamp,
			csession.NewTurnStart(sessionID, csession.TurnStartedData{}),
		)
	case session.ToolCallStart:
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
	case session.ToolCallEnd:
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
			csession.NewEvent(sessionID, csession.ToolCompleted, completed),
		)
	case session.AgentMessage:
		if strings.TrimSpace(msg.Message) == "" && strings.TrimSpace(msg.Reasoning) == "" {
			return nil
		}
		agent := llm.TextMessage(llm.RoleAssistant, msg.Message)
		agent.Reasoning = msg.Reasoning
		return b.saveCantoEvent(ctx, msg.Timestamp, csession.NewMessage(sessionID, agent))
	case session.TurnEnd:
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
