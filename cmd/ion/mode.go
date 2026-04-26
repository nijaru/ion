package main

import (
	"fmt"
	"strings"

	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
)

func modeFromName(value string) (session.Mode, error) {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "read", "r":
		return session.ModeRead, nil
	case "", "edit", "e", "write", "w":
		return session.ModeEdit, nil
	case "yolo", "y":
		return session.ModeYolo, nil
	default:
		return session.ModeEdit, fmt.Errorf("invalid mode %q (want read, edit, or yolo)", value)
	}
}

func startupMode(cfg *config.Config, modeFlag string, yoloFlag bool) (session.Mode, error) {
	configMode := ""
	if cfg != nil {
		configMode = config.ResolveDefaultMode(cfg.DefaultMode)
	}
	mode, err := modeFromName(configMode)
	if err != nil {
		return session.ModeEdit, err
	}

	if strings.TrimSpace(modeFlag) != "" {
		mode, err = modeFromName(modeFlag)
		if err != nil {
			return session.ModeEdit, err
		}
	}
	if yoloFlag {
		if strings.TrimSpace(modeFlag) != "" && mode != session.ModeYolo {
			return session.ModeEdit, fmt.Errorf("--yolo conflicts with --mode %s", strings.TrimSpace(modeFlag))
		}
		mode = session.ModeYolo
	}

	return mode, nil
}

func configureSessionMode(agent session.AgentSession, mode session.Mode) {
	if agent == nil {
		return
	}
	agent.SetMode(mode)
	agent.SetAutoApprove(mode == session.ModeYolo)
}
