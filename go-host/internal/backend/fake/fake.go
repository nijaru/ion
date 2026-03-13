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

func (Backend) Bootstrap() ([]session.Entry, string) {
	return []session.Entry{
		{Role: session.RoleSystem, Content: "ion-go rewrite branch"},
		{
			Role:    session.RoleAssistant,
			Content: "This is a Bubble Tea v2 host slice. It simulates turns so we can evaluate transcript, composer, resize, and inline behavior.",
		},
	}, "[rewrite] Bubble Tea v2 host slice"
}

func (Backend) Submit(input string) tea.Cmd {
	return tea.Tick(350*time.Millisecond, func(time.Time) tea.Msg {
		return backend.ReplyMsg{
			Entry: session.Entry{
				Role:    session.RoleAssistant,
				Content: fmt.Sprintf("Echoing back %q so we can exercise the host loop without a real agent yet.", input),
			},
			Status: "[reply] fake assistant turn complete",
		}
	})
}
