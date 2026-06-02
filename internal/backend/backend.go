package backend

import (
	"context"
	"strings"

	"github.com/nijaru/ion/internal/config"
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
	// ContextLimit must be cheap and nonblocking; render paths call it often.
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

func ToolEnvironmentLabel(value string) string {
	switch strings.TrimSpace(value) {
	case "":
		return ""
	case "inherit":
		return "inherited"
	case "inherit_without_provider_keys":
		return "inherited without provider keys"
	default:
		return strings.TrimSpace(value)
	}
}

func ToolEnvironmentSummary(value string) string {
	label := ToolEnvironmentLabel(value)
	if label == "" {
		return ""
	}
	return "Bash env " + label
}
