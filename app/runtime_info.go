package app

import (
	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/session"
)

// Bootstrap is the initial projection shown before the TUI has received
// runtime events. It belongs to the host application, not the agent loop.
type Bootstrap struct {
	Entries []session.Entry
	Status  string
}

// RuntimeInfo describes the configured runtime for host and TUI presentation.
// The active turn/session behavior is owned by agent.Runner; this interface is
// deliberately limited to startup and display metadata.
type RuntimeInfo interface {
	Name() string
	Provider() string
	Model() string
	ContextLimit() int
	Bootstrap() Bootstrap
	SetStore(session.Store)
	SetConfig(*config.Config)
}

// ToolSurface describes the tools exposed by the materialized runtime.
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

// ToolSummarizer is an optional startup projection for runtimes that expose
// tool-surface details.
type ToolSummarizer interface {
	ToolSurface() ToolSurface
}
