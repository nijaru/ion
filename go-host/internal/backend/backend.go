package backend

import (
	"github.com/nijaru/ion/go-host/internal/session"
)

type Bootstrap struct {
	Entries []session.Entry
	Status  string
}

type Backend interface {
	Name() string
	Bootstrap() Bootstrap
	Session() session.AgentSession
}
