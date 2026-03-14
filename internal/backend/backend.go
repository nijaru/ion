package backend

import (
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

type Bootstrap struct {
	Entries []session.Entry
	Status  string
}

type Backend interface {
	Name() string
	Bootstrap() Bootstrap
	Session() session.AgentSession
	SetStore(storage.Store)
}
