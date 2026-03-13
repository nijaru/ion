package fake

import (
	"fmt"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/go-host/internal/backend"
	"github.com/nijaru/ion/go-host/internal/session"
)

type Backend struct{}

func New() Backend {
	return Backend{}
}

func (Backend) Name() string {
	return "fake"
}

func (Backend) Bootstrap() backend.Bootstrap {
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

func (Backend) Submit(input string) tea.Cmd {
	return tea.Sequence(
		deliverAfter(0, backend.TurnStateMsg{Running: true}),
		deliverAfter(0, backend.StatusMsg{Text: "[fake] planning reply"}),
		deliverAfter(120*time.Millisecond, backend.StreamStartMsg{Role: session.RoleAssistant}),
		deliverAfter(220*time.Millisecond, backend.StreamDeltaMsg{
			Delta: fmt.Sprintf("Reviewing %q in fake mode so we can exercise a streamed host loop.", input),
		}),
		deliverAfter(380*time.Millisecond, backend.StreamDeltaMsg{
			Delta: "\n\nThis backend is intentionally emitting multiple event types because ion will eventually need transcript text, tool output, progress, and completion state from either ACP or a native agent runtime.",
		}),
		deliverAfter(520*time.Millisecond, backend.AppendEntryMsg{
			Entry: session.Entry{
				Role:    session.RoleTool,
				Title:   "bash(git status --short)",
				Content: "✓ fake tool result: working tree checked for rewrite branch cleanliness",
			},
		}),
		deliverAfter(680*time.Millisecond, backend.StreamDeltaMsg{
			Delta: "\n\nThat means the UI loop is already much closer to a real agent host than a one-shot echo demo.",
		}),
		deliverAfter(840*time.Millisecond, backend.StreamDoneMsg{}),
		deliverAfter(860*time.Millisecond, backend.StatusMsg{Text: "[fake] turn complete"}),
		deliverAfter(860*time.Millisecond, backend.TurnStateMsg{Running: false}),
	)
}

func deliverAfter(delay time.Duration, msg tea.Msg) tea.Cmd {
	return tea.Tick(delay, func(time.Time) tea.Msg {
		return msg
	})
}
