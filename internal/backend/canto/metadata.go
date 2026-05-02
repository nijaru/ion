package canto

import (
	"context"
	"os"
	"strings"

	"github.com/nijaru/canto/llm"
	"github.com/nijaru/canto/prompt"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/backend/canto/tools"
	"github.com/nijaru/ion/internal/backend/registry"
	"github.com/nijaru/ion/internal/config"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func (b *Backend) Name() string {
	return "canto"
}

func (b *Backend) SetConfig(cfg *config.Config) {
	b.cfg = cfg
	if retry, ok := b.compactLLM.(*llm.RetryProvider); ok {
		retry.Config.RetryForever = cfg.RetryUntilCancelledEnabled()
		retry.Config.RetryForeverTransportOnly = true
	}
}

func (b *Backend) SetPolicyConfig(cfg *backend.PolicyConfig) {
	b.policy.ApplyConfig(cfg)
}

func (b *Backend) Provider() string {
	if b.cfg != nil && b.cfg.Provider != "" {
		return b.cfg.Provider
	}
	return os.Getenv("ION_PROVIDER")
}

func (b *Backend) Model() string {
	if b.cfg != nil && b.cfg.Model != "" {
		return b.cfg.Model
	}
	m := os.Getenv("ION_MODEL")
	if i := strings.IndexByte(m, ' '); i > 0 {
		return strings.TrimSpace(m[i+1:])
	}
	return m
}

func (b *Backend) ContextLimit() int {
	if b.cfg != nil && b.cfg.ContextLimit > 0 {
		return b.cfg.ContextLimit
	}
	provider := b.Provider()
	model := b.Model()
	if meta, ok := registry.GetMetadata(context.Background(), provider, model); ok {
		return meta.ContextLimit
	}
	return 0
}

func (b *Backend) ToolSurface() backend.ToolSurface {
	if b.tools == nil {
		return backend.ToolSurface{}
	}
	names := b.policy.VisibleToolNames(b.tools.Names())
	threshold := prompt.DefaultLazyThreshold
	return backend.ToolSurface{
		Count:         len(names),
		LazyThreshold: threshold,
		LazyEnabled:   len(names) > threshold,
		Names:         names,
		Sandbox:       tools.SandboxSummary(),
	}
}

func (b *Backend) Bootstrap() backend.Bootstrap {
	status := "Ready"
	if b.sess != nil {
		if s, err := b.sess.LastStatus(context.Background()); err == nil && s != "" {
			status = s
		} else {
			status = "Connected via Canto"
		}
	}
	return backend.Bootstrap{
		Entries: []ionsession.Entry{},
		Status:  status,
	}
}

func (b *Backend) Session() ionsession.AgentSession {
	return b
}

func (b *Backend) SetStore(s storage.Store) {
	b.ionStore = s
}

func (b *Backend) SetSession(s storage.Session) {
	b.sess = s
}

func (b *Backend) Events() <-chan ionsession.Event {
	return b.events
}

func (b *Backend) ID() string {
	if b.sess != nil {
		return b.sess.ID()
	}
	return ""
}

func (b *Backend) Meta() map[string]string {
	if b.sess != nil {
		m := b.sess.Meta()
		return map[string]string{
			"model":  m.Model,
			"branch": m.Branch,
			"cwd":    m.CWD,
		}
	}
	return nil
}

func storageModelName(provider, model string) string {
	provider = strings.TrimSpace(provider)
	model = strings.TrimSpace(model)
	if provider == "" {
		return model
	}
	if model == "" {
		return provider
	}
	return provider + "/" + model
}
