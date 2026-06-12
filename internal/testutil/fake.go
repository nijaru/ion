package testutil

import (
	"context"
	"fmt"
	"time"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/internal/core"
	"github.com/nijaru/ion/session"
)

type Backend struct {
	events  chan session.AgentEvent
	storage session.SessionStore
	sess    session.SessionHandle
	script  []ScriptStep
	cfg     *config.Config
}

func (b *Backend) SetConfig(cfg *config.Config) {
	b.cfg = cfg
}

type ScriptStep struct {
	Event session.AgentEvent
	Delay time.Duration
}

func (b *Backend) SetScript(steps []ScriptStep) {
	b.script = steps
}

func (b *Backend) SetStore(s session.SessionStore) {
	b.storage = s
}

func (b *Backend) SetSession(s session.SessionHandle) {
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
		events: make(chan session.AgentEvent, 100),
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

func (b *Backend) Bootstrap() core.Bootstrap {
	return core.Bootstrap{
		Entries: []session.Entry{
			{Role: session.RoleSystem, Content: "ion test backend"},
			{
				Role:    session.RoleAgent,
				Content: "This backend emits deterministic stream, tool, progress, and completion events for TUI tests.",
			},
		},
		Status: "[test] ready",
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
		b.events <- session.UserMessage{Message: input}
		b.events <- session.TurnStart{}
		b.events <- session.StatusChange{Status: "[fake] planning reply"}

		// Maintain partial message state (Pi model: message_update carries full state)
		partialMessage := session.AgentMessage{}

		time.Sleep(120 * time.Millisecond)
		delta1 := fmt.Sprintf("Reviewing %q in fake mode so we can exercise a streamed host loop.", input)
		partialMessage.Message = delta1
		b.events <- session.MessageUpdate{
			Message:   partialMessage,
			Delta:     delta1,
			BlockType: "text",
		}

		time.Sleep(160 * time.Millisecond)
		delta2 := "\n\nThis backend is intentionally emitting multiple event types because ion will eventually need transcript text, tool output, progress, and completion state from either ACP or a native agent runtime."
		partialMessage.Message += delta2
		b.events <- session.MessageUpdate{
			Message:   partialMessage,
			Delta:     delta2,
			BlockType: "text",
		}

		time.Sleep(140 * time.Millisecond)
		b.events <- session.ToolCallStart{ToolName: "bash", Args: "git status --short"}

		time.Sleep(100 * time.Millisecond)
		b.events <- session.ToolCallEnd{
			ToolName: "bash",
			Result:   "test tool result: working tree checked",
		}

		time.Sleep(160 * time.Millisecond)
		delta3 := "\n\nThat means the UI loop is already much closer to a real agent host than a one-shot echo demo."
		partialMessage.Message += delta3
		b.events <- session.MessageUpdate{
			Message:   partialMessage,
			Delta:     delta3,
			BlockType: "text",
		}

		time.Sleep(160 * time.Millisecond)
		b.events <- session.AgentMessage{Message: ""} // Signal end of message
		b.events <- session.StatusChange{Status: "[fake] turn complete"}
		b.events <- session.TurnEnd{}
	}()
	return nil
}

func (b *Backend) CancelTurn(ctx context.Context) error {
	return nil
}

func (b *Backend) Close() error {
	close(b.events)
	return nil
}

func (b *Backend) Events() <-chan session.AgentEvent {
	return b.events
}
