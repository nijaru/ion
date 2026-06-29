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
	"github.com/nijaru/ion/app"
	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/internal/runtime"
	"github.com/nijaru/ion/session"
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
	_ = resume // resume not yet wired with new session model
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

	dbPath := filepath.Join(storeRoot, "sessions.db")
	id := strings.TrimSpace(sessionID)
	if id == "" {
		id = "smoke-session"
	}
	store, err := session.NewSQLiteStore(dbPath, id)
	if err != nil {
		return fmt.Errorf("open store: %w", err)
	}
	defer store.Close()

	sess := session.NewSession(store, 64)

	if mode == "session-picker" {
		if err := seedSmokeSessionPicker(ctx, store, cwd); err != nil {
			return err
		}
	}

	smoke := newSmokeBackend(mode)
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

	model := app.New(smoke, sess, store, cwd, "smoke", "v0.0.0", nil).
		WithConfig(cfg)
	if mode == "session-picker" {
		model = model.WithSessionPicker()
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

func seedSmokeSessionPicker(ctx context.Context, store session.Store, cwd string) error {
	_ = cwd
	fixtures := []struct {
		id    string
		title string
	}{
		{id: "ion-tmux-session-picker-primary", title: "Resume deterministic picker"},
		{id: "ion-tmux-session-picker-alternate", title: "Alternate deterministic branch"},
	}
	for _, fixture := range fixtures {
		info := session.SessionInfoEntry{
			EntryBase: session.EntryBase{ID: fixture.id},
			Name:      fixture.title,
			Model:     "fake/fake-model",
		}
		if err := store.UpdateSession(ctx, info); err != nil {
			return fmt.Errorf("update seed session %s: %w", fixture.id, err)
		}
	}
	return nil
}

type smokeBackend struct {
	mode   string
	events chan session.Event
	cfg    *config.Config

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

func (b *smokeBackend) Bootstrap() app.Bootstrap {
	return app.Bootstrap{Status: "[smoke] ready"}
}

func (b *smokeBackend) Session() session.Session { return nil }

func (b *smokeBackend) SetStore(session.Store) {}

func (b *smokeBackend) SetConfig(cfg *config.Config) {
	b.cfg = cfg
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

func (b *smokeBackend) Steer(text string) {
	// no-op: test backend
}

func (b *smokeBackend) FollowUp(text string) {
	// no-op: test backend
}

func (b *smokeBackend) Close() error {
	_ = b.CancelTurn(context.Background())
	return nil
}

func (b *smokeBackend) Events() <-chan session.Event { return b.events }

func now() time.Time { return time.Now() }

func userEvent(text string) session.UserMessage {
	return session.UserMessage{
		Content:   []session.Content{session.TextContent{Text: text}},
		Timestamp: now(),
	}
}

func agentEndEvent(text string) session.MessageEnd {
	return session.MessageEnd{
		Message: &session.AssistantMessage{
			Content: []session.Content{session.TextContent{Text: text}},
		},
		Timestamp: now(),
	}
}

func messageDelta(text string) session.MessageUpdate {
	return session.MessageUpdate{
		Delta:     session.TextDelta{Text: text},
		BlockType: "text",
	}
}

func (b *smokeBackend) runScript(ctx context.Context, input string) {
	switch b.mode {
	case "cancel":
		b.emit(ctx, userEvent(input))
		b.emit(ctx, session.TurnStart{Timestamp: now()})
		b.emit(ctx, runtime.StatusChange{Status: "[smoke] waiting for cancel"})
		<-ctx.Done()
	case "error":
		b.emit(ctx, userEvent(input))
		b.emit(ctx, session.TurnStart{Timestamp: now()})
		b.emit(ctx, runtime.StatusChange{Status: "[smoke] active before error"})
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
		b.emit(ctx, userEvent(input))
		b.emit(ctx, session.TurnStart{Timestamp: now()})
		b.emit(ctx, runtime.StatusChange{Status: "[smoke] active progress"})
		if !b.sleep(ctx, 700*time.Millisecond) {
			return
		}
		b.emit(ctx, messageDelta("streaming from deterministic smoke backend"))
		if !b.sleep(ctx, 900*time.Millisecond) {
			return
		}
		b.emit(ctx, session.ToolExecStart{
			ToolCallID: "tool-1",
			Name:       "bash",
			Args:       []byte(`{"command":"sleep 2; echo ion-tmux-smoke"}`),
		})
		if !b.sleep(ctx, 1200*time.Millisecond) {
			return
		}
		b.emit(ctx, session.ToolExecUpdate{
			ToolCallID: "tool-1",
			Partial:    "ion-tmux-",
		})
		if !b.sleep(ctx, 500*time.Millisecond) {
			return
		}
		b.emit(ctx, session.ToolExecEnd{
			ToolCallID: "tool-1",
			Result: session.ToolResultMessage{
				ToolCallID: "tool-1",
				ToolName:   "bash",
				Content:    []session.Content{session.TextContent{Text: "ion-tmux-smoke\n"}},
			},
		})
		if !b.sleep(ctx, 500*time.Millisecond) {
			return
		}
		b.emit(ctx, agentEndEvent("done"))
		b.emit(ctx, session.TurnEnd{Base: session.BaseNow()})
	}
}

func (b *smokeBackend) runMarkdownScript(ctx context.Context, input string) {
	b.emit(ctx, userEvent(input))
	b.emit(ctx, session.TurnStart{Timestamp: now()})
	b.emit(ctx, runtime.StatusChange{Status: "[smoke] markdown stream"})
	if !b.sleep(ctx, 200*time.Millisecond) {
		return
	}
	b.emit(ctx, messageDelta(strings.Join([]string{
		"Here's the summary of both status files:",
		"",
		"## Canto (`../canto/ai/STATUS.md`)",
		"",
		"**Key facts:**",
	}, "\n")))
	if !b.sleep(ctx, 500*time.Millisecond) {
		return
	}
	b.emit(ctx, agentEndEvent(strings.Join([]string{
		"Here's the summary of both status files:",
		"",
		"## Canto (`../canto/ai/STATUS.md`)",
		"",
		"**Key facts:**",
		"",
		"- The markdown stream should not be committed raw.",
		"- A long line with a verylongunbrokenidentifierthatshouldwrapbeforetheterminaldoes must still fit the shell width.",
		"",
		"### Example with syntax highlighting",
		"",
		"```go",
		"func main() {",
		"\tfmt.Println(\"hello world\")",
		"}",
		"```",
		"",
		"Bottom line: formatted final output should be the only committed assistant entry.",
	}, "\n")))
	b.emit(ctx, session.TurnEnd{Base: session.BaseNow()})
}

func (b *smokeBackend) runActiveControlsScript(ctx context.Context, input string) {
	b.emit(ctx, userEvent(input))
	b.emit(ctx, session.TurnStart{Timestamp: now()})
	b.emit(ctx, runtime.StatusChange{Status: "[smoke] active controls"})
	if !b.sleep(ctx, 9*time.Second) {
		return
	}
	b.emit(ctx, agentEndEvent("controls done"))
	b.emit(ctx, session.TurnEnd{Base: session.BaseNow()})
}

func (b *smokeBackend) runFileToolScript(ctx context.Context, input string) {
	b.emit(ctx, userEvent(input))
	b.emit(ctx, session.TurnStart{Timestamp: now()})
	b.emit(ctx, runtime.StatusChange{Status: "[smoke] file tool rows"})
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
		b.emit(ctx, session.ToolExecStart{
			ToolCallID: tool.id,
			Name:       tool.name,
			Args:       []byte(tool.args),
		})
		if !b.sleep(ctx, 150*time.Millisecond) {
			return
		}
		b.emit(ctx, session.ToolExecEnd{
			ToolCallID: tool.id,
			Result: session.ToolResultMessage{
				ToolCallID: tool.id,
				ToolName:   tool.name,
				Content:    []session.Content{session.TextContent{Text: tool.result}},
			},
		})
		if !b.sleep(ctx, 100*time.Millisecond) {
			return
		}
	}
	b.emit(ctx, agentEndEvent("file tools done"))
	b.emit(ctx, session.TurnEnd{Base: session.BaseNow()})
}

func (b *smokeBackend) emit(ctx context.Context, event session.Event) bool {
	select {
	case <-ctx.Done():
		return false
	case b.events <- event:
		return true
	}
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
