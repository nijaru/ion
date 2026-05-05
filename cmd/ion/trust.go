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
	policy := "prompt"
	if cfg != nil {
		policy = config.ResolveWorkspaceTrust(cfg.WorkspaceTrust)
	}
	if policy == "off" {
		return nil, true, "", nil
	}
	path, err := ionworkspace.DefaultTrustPath()
	if err != nil {
		return nil, false, "", err
	}
	store := ionworkspace.NewTrustStore(path)
	trusted, err := store.IsTrusted(cwd)
	if err != nil {
		return nil, false, "", err
	}
	if trusted {
		return store, true, "", nil
	}
	if policy == "strict" {
		return store, false, "Workspace: not trusted. READ mode active. Trust must be managed outside this session.", nil
	}
	return store, false, "Workspace: not trusted. READ mode active. Run /trust to enable edits.", nil
}

func applyWorkspaceTrustModeGate(mode session.Mode, trusted bool) session.Mode {
	if !trusted && mode != session.ModeRead {
		return session.ModeRead
	}
	return mode
}
