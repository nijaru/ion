package main

import (
	ionacp "github.com/nijaru/ion/internal/acp"
	"github.com/nijaru/ion/session"
)

type modeConfigurableSession interface {
	SetMode(ionacp.Mode)
	SetAutoApprove(bool)
}

func configureSessionMode(agent session.AgentSession, mode ionacp.Mode) {
	configurable, ok := agent.(modeConfigurableSession)
	if !ok {
		return
	}
	configurable.SetMode(mode)
	configurable.SetAutoApprove(mode == ionacp.ModeYolo)
}
