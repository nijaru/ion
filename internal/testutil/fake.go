package testutil

import (
	"context"
	"fmt"
	"time"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

type Backend struct {
	events  chan session.Event
	storage storage.Store
	sess    storage.Session
	script  []ScriptStep
	cfg     *config.Config
}

func (b *Backend) SetConfig(cfg *config.Config) {
	b.cfg = cfg
}

type ScriptStep struct {
	Event session.Event
	Delay time.Duration
}

func (b *Backend) SetScript(steps []ScriptStep) {
	b.script = steps
}

func (b *Backend) SetStore(s storage.Store) {
	b.storage = s
}

func (b *Backend) SetSession(s storage.Session) {
	b.sess = s
}

func (b *Backend) ID() string {
	if b.sess != nil {
		return b.sess.ID()
	}
	return "fake-session"
}

func (b *Backend) Meta() map[string]string {
	return map[string]string{
		"model": "fake-model",
	}
}

func New() *Backend {
	return &Backend{
		events: make(chan session.Event, 100),
	}
}

func (b *Backend) Name() string {
	return "fake"
}

func (b *Backend) Provider() string {
	if b.cfg != nil && b.cfg.Provider != "" {
		return b.cfg.Provider
	}
	return "fake"
}

func (b *Backend) Model() string {
	if b.cfg != nil && b.cfg.Model != "" {
		return b.cfg.Model
	}
	return "fake-model"
}

func (b *Backend) ContextLimit() int {
	if b.cfg != nil {
		return b.cfg.ContextLimit
	}
	return 0
}

func (b *Backend) Bootstrap() backend.Bootstrap {
	return backend.Bootstrap{
		Entries: []session.Entry{
			{Role: session.System, Content: "ion-go rewrite branch"},
			{
				Role:    session.Agent,
				Content: "This host is now shaped around streamed backend events, tool output, and a stable transcript/composer loop so we can judge Bubble Tea by real behavior instead of setup speed.",
			},
		},
		Status: "[rewrite] Bubble Tea v2 host stream scaffold",
	}
}

func (b *Backend) Session() session.AgentSession {
	return b
}

func (b *Backend) Open(ctx context.Context) error {
	return nil
}

func (b *Backend) Resume(ctx context.Context, sessionID string) error {
	return nil
}

func (b *Backend) SubmitTurn(ctx context.Context, input string) error {
	if len(b.script) > 0 {
		go func() {
			for _, step := range b.script {
				if step.Delay > 0 {
					time.Sleep(step.Delay)
				}
				b.events <- step.Event
			}
		}()
		return nil
	}

	go func() {
		b.events <- session.TurnStarted{}
		b.events <- session.StatusChanged{Status: "[fake] planning reply"}

		time.Sleep(120 * time.Millisecond)
		b.events <- session.AgentDelta{Delta: fmt.Sprintf("Reviewing %q in fake mode so we can exercise a streamed host loop.", input)}

		time.Sleep(160 * time.Millisecond)
		b.events <- session.AgentDelta{Delta: "\n\nThis backend is intentionally emitting multiple event types because ion will eventually need transcript text, tool output, progress, and completion state from either ACP or a native agent runtime."}

		time.Sleep(140 * time.Millisecond)
		b.events <- session.ToolCallStarted{ToolName: "bash", Args: "git status --short"}

		time.Sleep(100 * time.Millisecond)
		b.events <- session.ToolResult{
			ToolName: "bash",
			Result:   "✓ fake tool result: working tree checked for rewrite branch cleanliness",
		}

		time.Sleep(160 * time.Millisecond)
		b.events <- session.AgentDelta{Delta: "\n\nThat means the UI loop is already much closer to a real agent host than a one-shot echo demo."}

		time.Sleep(160 * time.Millisecond)
		b.events <- session.AgentMessage{Message: ""} // Signal end of message
		b.events <- session.StatusChanged{Status: "[fake] turn complete"}
		b.events <- session.TurnFinished{}
	}()
	return nil
}

func (b *Backend) CancelTurn(ctx context.Context) error {
	return nil
}

func (b *Backend) Approve(ctx context.Context, requestID string, approved bool) error {
	return nil
}

func (b *Backend) RegisterMCPServer(ctx context.Context, command string, args ...string) error {
	return nil
}

func (b *Backend) SetMode(session.Mode) {}

func (b *Backend) Close() error {

	close(b.events)
	return nil
}

func (b *Backend) Events() <-chan session.Event {
	return b.events
}
