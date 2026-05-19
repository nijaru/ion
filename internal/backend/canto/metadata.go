package canto

import (
	"context"
	"os"
	"slices"
	"strings"

	"github.com/nijaru/canto/prompt"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/backend/registry"
	"github.com/nijaru/ion/internal/config"
	ionsession "github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func (b *Backend) Name() string {
	return "canto"
}

func (b *Backend) SetConfig(cfg *config.Config) {
	b.cfgMu.Lock()
	if cfg == nil {
		b.cfg = nil
		b.cfgMu.Unlock()
		return
	}
	copied := *cfg
	b.cfg = &copied
	b.cfgMu.Unlock()
	if retry, ok := retryProviderInChain(b.compactLLM); ok {
		retry.Config.RetryForever = copied.RetryUntilCancelledEnabled()
		retry.Config.RetryForeverTransportOnly = true
	}
}

func (b *Backend) configSnapshot() *config.Config {
	b.cfgMu.RLock()
	defer b.cfgMu.RUnlock()
	if b.cfg == nil {
		return nil
	}
	copied := *b.cfg
	return &copied
}

func (b *Backend) Provider() string {
	if cfg := b.configSnapshot(); cfg != nil && cfg.Provider != "" {
		return cfg.Provider
	}
	return os.Getenv("ION_PROVIDER")
}

func (b *Backend) Model() string {
	if cfg := b.configSnapshot(); cfg != nil && cfg.Model != "" {
		return cfg.Model
	}
	m := os.Getenv("ION_MODEL")
	if i := strings.IndexByte(m, ' '); i > 0 {
		return strings.TrimSpace(m[i+1:])
	}
	return m
}

func (b *Backend) ContextLimit() int {
	cfg := b.configSnapshot()
	if cfg != nil && cfg.ContextLimit > 0 {
		return cfg.ContextLimit
	}
	if limit, ok := registry.CachedContextLimitForConfig(cfg); ok {
		return limit
	}
	provider := b.Provider()
	model := b.Model()
	if limit, ok := registry.CachedContextLimit(provider, model); ok {
		return limit
	}
	return 0
}

func (b *Backend) ToolSurface() backend.ToolSurface {
	if b.tools == nil {
		return backend.ToolSurface{}
	}
	names := b.tools.Names()
	threshold := prompt.DefaultLazyThreshold
	environment := ""
	if slices.Contains(names, "bash") {
		environment = b.executorEnvironmentPolicy().Summary()
	}
	return backend.ToolSurface{
		Count:         len(names),
		LazyThreshold: threshold,
		LazyEnabled:   len(names) > threshold,
		Names:         names,
		Environment:   environment,
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
	b.mu.Lock()
	defer b.mu.Unlock()
	b.ionStore = s
}

func (b *Backend) SetSession(s storage.Session) {
	b.mu.Lock()
	defer b.mu.Unlock()
	b.sess = s
}

func (b *Backend) Events() <-chan ionsession.Event {
	return b.events
}

func (b *Backend) ID() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.idLocked()
}

func (b *Backend) idLocked() string {
	if b.sess != nil {
		return b.sess.ID()
	}
	return ""
}

func (b *Backend) Meta() map[string]string {
	b.mu.Lock()
	defer b.mu.Unlock()
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
