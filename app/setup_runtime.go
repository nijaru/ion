package app

import (
	"context"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/session"
)

// setupRuntime is a minimal RuntimeInfo used before the user configures a provider
// and model. It shows a bootstrap status message and holds a read-only entry
// reader plus config for later materialization. It has no session or runner —
// the TUI shows a setup prompt until the user runs /provider and /model.
type setupRuntime struct {
	cfg   *config.Config
	store RuntimeEntryReader
	msg   string
}

func NewSetupRuntime(cfg *config.Config, store RuntimeEntryReader, msg string) *setupRuntime {
	return &setupRuntime{cfg: cfg, store: store, msg: msg}
}

func (r *setupRuntime) Name() string { return "setup" }
func (r *setupRuntime) Provider() string {
	if r.cfg != nil {
		return r.cfg.Provider
	}
	return ""
}
func (r *setupRuntime) Model() string {
	if r.cfg != nil {
		return r.cfg.Model
	}
	return ""
}
func (r *setupRuntime) ContextLimit() int {
	if r.cfg != nil {
		return r.cfg.ContextLimit
	}
	return 0
}
func (r *setupRuntime) Bootstrap() Bootstrap {
	entries := []session.Entry{}
	if r.store != nil {
		entries, _ = r.store.Entries(context.Background())
	}
	return Bootstrap{
		Entries: entries,
		Status:  r.msg,
	}
}
