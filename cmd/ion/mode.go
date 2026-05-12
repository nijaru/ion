package main

import (
	"github.com/nijaru/ion/internal/session"
)

type modeConfigurableSession interface {
	SetMode(session.Mode)
	SetAutoApprove(bool)
}

func configureSessionMode(agent session.AgentSession, mode session.Mode) {
	configurable, ok := agent.(modeConfigurableSession)
	if !ok {
		return
	}
	configurable.SetMode(mode)
	configurable.SetAutoApprove(mode == session.ModeYolo)
}
