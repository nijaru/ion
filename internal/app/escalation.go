package app

import (
	"fmt"
	"strings"
	"time"

	"github.com/nijaru/canto/workspace"
)

func escalationSummary(cfg *workspace.EscalationConfig) string {
	if cfg == nil {
		return ""
	}
	parts := make([]string, 0, len(cfg.Channels)+1)
	for _, channel := range cfg.Channels {
		label := escalationChannelLabel(channel)
		if label != "" {
			parts = append(parts, label)
		}
	}
	if cfg.Approval.Timeout > 0 {
		parts = append(parts, "approval timeout "+formatEscalationDuration(cfg.Approval.Timeout))
	}
	return strings.Join(parts, "; ")
}

func escalationChannelLabel(channel workspace.EscalationChannel) string {
	switch strings.ToLower(strings.TrimSpace(channel.Type)) {
	case "email":
		if channel.Address == "" {
			return ""
		}
		return "email " + channel.Address
	case "slack":
		if channel.Channel == "" {
			return ""
		}
		return "slack " + channel.Channel
	default:
		target := strings.TrimSpace(channel.Address)
		if target == "" {
			target = strings.TrimSpace(channel.Channel)
		}
		if target == "" {
			return ""
		}
		kind := strings.TrimSpace(channel.Type)
		if kind == "" {
			return target
		}
		return kind + " " + target
	}
}

func formatEscalationDuration(d time.Duration) string {
	if d%time.Hour == 0 {
		return fmt.Sprintf("%dh", int(d/time.Hour))
	}
	if d%time.Minute == 0 {
		return fmt.Sprintf("%dm", int(d/time.Minute))
	}
	return d.String()
}
