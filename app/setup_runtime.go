package app

import (
	"github.com/nijaru/ion/config"
)

// setupRuntime is a minimal RuntimeInfo used before the user configures a provider
// and model. It shows a bootstrap status message and holds only effective
// configuration. It has no session or runner —
// the TUI shows a setup prompt until the user runs /provider and /model.
type setupRuntime struct {
	cfg *config.Config
	msg string
}

func NewSetupRuntime(cfg *config.Config, msg string) *setupRuntime {
	return &setupRuntime{cfg: cfg, msg: msg}
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
	return Bootstrap{
		Status: r.msg,
	}
}
