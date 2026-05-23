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

	"github.com/nijaru/ion/internal/app"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func main() {
	mode := flag.String("mode", "complete", "smoke script mode: complete, cancel, or error")
	storeRoot := flag.String("store", "", "session store directory")
	startupCheck := flag.Bool("startup-check", false, "render the ready shell once and exit")
	flag.Parse()

	if err := run(*mode, *storeRoot, *startupCheck); err != nil {
		fmt.Fprintf(os.Stderr, "ion-tui-smoke: %v\n", err)
		os.Exit(1)
	}
}

func run(mode, storeRoot string, startupCheck bool) error {
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
	stored, err := store.OpenSession(ctx, cwd, "fake/fake-model", "smoke")
	if err != nil {
		return err
	}

	smoke := newSmokeBackend(mode)
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

type smokeBackend struct {
	mode    string
	events  chan session.Event
	storage storage.Session
	cfg     *config.Config

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

func (b *smokeBackend) SetConfig(cfg *config.Config) {
	b.cfg = cfg
}

func (b *smokeBackend) ToolSurface() backend.ToolSurface {
	return backend.ToolSurface{
		Count:       7,
		Names:       []string{"bash", "edit", "glob", "grep", "list", "read", "write"},
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
