package canto

import (
	"context"
	"strings"

	"github.com/nijaru/canto/llm"
	"github.com/nijaru/canto/prompt"
	"github.com/nijaru/canto/session"
	"github.com/nijaru/ion/internal/config"
)

func reasoningEffortProcessor(cfg *config.Config) prompt.RequestProcessor {
	return prompt.RequestProcessorFunc(
		func(ctx context.Context, p llm.Provider, model string, sess *session.Session, req *llm.Request) error {
			if cfg == nil {
				return nil
			}
			effort := normalizeReasoningEffort(cfg.ReasoningEffort)
			if effort == "" || effort == config.DefaultReasoningEffort {
				req.ReasoningEffort = ""
				return nil
			}
			if p == nil || !p.Capabilities(model).ReasoningEffort {
				req.ReasoningEffort = ""
				return nil
			}
			if effort == "off" {
				req.ReasoningEffort = "none"
				return nil
			}
			if effort == "max" {
				req.ReasoningEffort = ""
				return nil
			}
			req.ReasoningEffort = effort
			return nil
		},
	)
}

func reflexionProcessor() prompt.RequestProcessor {
	return prompt.RequestProcessorFunc(
		func(ctx context.Context, p llm.Provider, model string, sess *session.Session, req *llm.Request) error {
			if len(req.Messages) == 0 {
				return nil
			}
			lastMsgIdx := len(req.Messages) - 1
			lastMsg := req.Messages[lastMsgIdx]

			if lastMsg.Role == llm.RoleUser && lastMsg.ToolID != "" {
				ev, ok := sess.LastEvent()
				if !ok {
					return nil
				}
				if ev.Type == session.ToolCompleted {
					var data struct {
						Error string `json:"error,omitempty"`
					}
					if err := ev.UnmarshalData(&data); err == nil && data.Error != "" {
						req.Messages[lastMsgIdx].Content += "\n\n[System Note: The tool execution failed. Analyze the error carefully. Think step-by-step about what went wrong, and formulate a new plan before your next tool call.]"
					}
				}
			}
			return nil
		},
	)
}

func normalizeReasoningEffort(value string) string {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "", config.DefaultReasoningEffort:
		return config.DefaultReasoningEffort
	case "off", "none", "disabled":
		return "off"
	case "minimal", "min":
		return "minimal"
	case "low":
		return "low"
	case "medium", "med":
		return "medium"
	case "high":
		return "high"
	case "xhigh", "extra-high", "extra_high", "extra high":
		return "xhigh"
	case "max", "maximum":
		return "max"
	default:
		return config.DefaultReasoningEffort
	}
}
