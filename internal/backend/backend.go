package backend

import (
	"context"
	"strings"

	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
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
	SetStore(storage.Store)
	SetSession(storage.Session)
	SetConfig(*config.Config)
}

type Compactor interface {
	Compact(context.Context) (bool, error)
}

type PolicyConfigurer interface {
	SetPolicyConfig(*PolicyConfig)
}

type ToolSurface struct {
	Count         int
	LazyThreshold int
	LazyEnabled   bool
	Names         []string
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
