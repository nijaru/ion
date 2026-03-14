package fake

import (
	"context"
	"fmt"
	"time"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/session"
)

type Backend struct {
	events chan session.Event
}

func New() *Backend {
	return &Backend{
		events: make(chan session.Event, 100),
	}
}

func (b *Backend) Name() string {
	return "fake"
}

func (b *Backend) Bootstrap() backend.Bootstrap {
	return backend.Bootstrap{
		Entries: []session.Entry{
			{Role: session.RoleSystem, Content: "ion-go rewrite branch"},
			{
				Role:    session.RoleAssistant,
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
	go func() {
		b.events <- session.EventTurnStarted{}
		b.events <- session.EventStatusChanged{Status: "[fake] planning reply"}
		
		time.Sleep(120 * time.Millisecond)
		b.events <- session.EventAssistantDelta{Delta: fmt.Sprintf("Reviewing %q in fake mode so we can exercise a streamed host loop.", input)}
		
		time.Sleep(160 * time.Millisecond)
		b.events <- session.EventAssistantDelta{Delta: "\n\nThis backend is intentionally emitting multiple event types because ion will eventually need transcript text, tool output, progress, and completion state from either ACP or a native agent runtime."}
		
		time.Sleep(140 * time.Millisecond)
		b.events <- session.EventToolCallStarted{ToolName: "bash", Args: "git status --short"}
		
		time.Sleep(100 * time.Millisecond)
		b.events <- session.EventToolResult{
			ToolName: "bash",
			Result:   "✓ fake tool result: working tree checked for rewrite branch cleanliness",
		}
		
		time.Sleep(160 * time.Millisecond)
		b.events <- session.EventAssistantDelta{Delta: "\n\nThat means the UI loop is already much closer to a real agent host than a one-shot echo demo."}
		
		time.Sleep(160 * time.Millisecond)
		b.events <- session.EventAssistantMessage{Message: ""} // Signal end of message
		b.events <- session.EventStatusChanged{Status: "[fake] turn complete"}
		b.events <- session.EventTurnFinished{}
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

func (b *Backend) Events() <-chan session.Event {
	return b.events
}
