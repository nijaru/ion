// Package core defines shared types used by app, testutil, and internal/agent
// without creating import cycles.
package core

import (
	"context"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/session"
)

type Bootstrap struct {
	Entries []session.Entry
	Status  string
}

type Backend interface {
	Name() string
	Provider() string
	Model() string
	ContextLimit() int
	Bootstrap() Bootstrap
	Session() session.AgentSession
	SetStore(session.SessionStore)
	SetSession(session.SessionHandle)
	SetConfig(*config.Config)
}

type Compactor interface {
	Compact(context.Context) (bool, error)
}

type ToolSurface struct {
	Count         int
	LazyThreshold int
	LazyEnabled   bool
	Names         []string
	ActiveNames   []string
	Mode          string
	Sandbox       string
	Environment   string
}

type ToolSummarizer interface {
	ToolSurface() ToolSurface
}
