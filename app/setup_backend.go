package app

import (
	"context"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/session"
)

// setupBackend is a minimal Backend used before the user configures a provider
// and model. It shows a bootstrap status message and holds the store + config
// for later materialization. It has no session or runner — the TUI shows a
// setup prompt until the user runs /provider and /model.
type setupBackend struct {
	cfg   *config.Config
	store session.Store
	msg   string
}

func NewSetupBackend(cfg *config.Config, store session.Store, msg string) *setupBackend {
	return &setupBackend{cfg: cfg, store: store, msg: msg}
}

func (b *setupBackend) Name() string              { return "setup" }
func (b *setupBackend) Provider() string           { if b.cfg != nil { return b.cfg.Provider }; return "" }
func (b *setupBackend) Model() string              { if b.cfg != nil { return b.cfg.Model }; return "" }
func (b *setupBackend) ContextLimit() int          { if b.cfg != nil { return b.cfg.ContextLimit }; return 0 }
func (b *setupBackend) Session() session.Session   { return nil }
func (b *setupBackend) SetStore(s session.Store)   { b.store = s }
func (b *setupBackend) SetConfig(cfg *config.Config) { b.cfg = cfg }
func (b *setupBackend) SetSession(session.Session) {}

func (b *setupBackend) Bootstrap() Bootstrap {
	entries := []session.Entry{}
	if b.store != nil {
		entries, _ = b.store.Entries(context.Background())
	}
	return Bootstrap{
		Entries: entries,
		Status:  b.msg,
	}
}
