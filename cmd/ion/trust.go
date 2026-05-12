package main

import (
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	ionworkspace "github.com/nijaru/ion/internal/workspace"
)

func loadWorkspaceTrust(
	cwd string,
	cfg *config.Config,
) (*ionworkspace.TrustStore, bool, string, error) {
	_ = cwd
	_ = cfg
	return nil, true, "", nil
}

func applyWorkspaceTrustModeGate(mode session.Mode, trusted bool) session.Mode {
	_ = trusted
	return mode
}
