package backend

import (
	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/go-host/internal/session"
)

type Bootstrap struct {
	Entries []session.Entry
	Status  string
}

type StatusMsg struct {
	Text string
}

type TurnStateMsg struct {
	Running bool
}

type StreamStartMsg struct {
	Role session.Role
}

type StreamDeltaMsg struct {
	Delta string
}

type StreamDoneMsg struct{}

type AppendEntryMsg struct {
	Entry session.Entry
}

type Backend interface {
	Name() string
	Bootstrap() Bootstrap
	Submit(input string) tea.Cmd
}
