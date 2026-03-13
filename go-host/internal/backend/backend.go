package backend

import (
	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/go-host/internal/session"
)

type ReplyMsg struct {
	Entry  session.Entry
	Status string
}

type Backend interface {
	Name() string
	Bootstrap() ([]session.Entry, string)
	Submit(input string) tea.Cmd
}
