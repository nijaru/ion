package workspace

import (
	"fmt"
	"strconv"
	"strings"
	"time"
)

// EscalationConfig describes the human-approval protocol declared in
// ESCALATE.md.
type EscalationConfig struct {
	Triggers []EscalationTrigger
	Channels []EscalationChannel
	Approval EscalationApproval
}

// EscalationTrigger describes one always-escalate rule.
type EscalationTrigger struct {
	Name  string
	Value string
}

// EscalationChannel describes one notification target.
type EscalationChannel struct {
	Type     string
	Address  string
	Channel  string
	Timeout  time.Duration
	Metadata map[string]string
}

// EscalationApproval describes the approval fallback behavior.
type EscalationApproval struct {
	Timeout    time.Duration
	OnTimeout  string
	OnDenial   string
	OnApproval string
}

// LoadEscalate reads ESCALATE.md from the workspace root and parses it.
func LoadEscalate(fs WorkspaceFS) (*EscalationConfig, error) {
	if fs == nil {
		return nil, fmt.Errorf("workspace is not open")
	}
	data, err := fs.ReadFile("ESCALATE.md")
	if err != nil {
		return nil, err
	}
	return ParseEscalate(data)
}

// ParseEscalate parses the supported ESCALATE.md markdown subset.
func ParseEscalate(data []byte) (*EscalationConfig, error) {
	text := strings.ReplaceAll(string(data), "\r\n", "\n")
	lines := strings.Split(text, "\n")

	cfg := &EscalationConfig{}
	var section string
	var channel *EscalationChannel

	flushChannel := func() {
		if channel == nil {
			return
		}
		if channel.Metadata == nil {
			channel.Metadata = map[string]string{}
		}
		cfg.Channels = append(cfg.Channels, *channel)
		channel = nil
	}

	for i, raw := range lines {
		line := strings.TrimSpace(raw)
		if line == "" || line == "---" || strings.HasPrefix(line, ">") {
			continue
		}
		if strings.HasPrefix(line, "## ") {
			flushChannel()
			section = strings.ToUpper(strings.TrimSpace(strings.TrimPrefix(line, "## ")))
			continue
		}
		if strings.HasPrefix(line, "#") {
			continue
		}

		switch section {
		case "TRIGGERS":
			if strings.HasSuffix(line, ":") {
				continue
			}
			if !strings.HasPrefix(line, "- ") {
				continue
			}
			item := strings.TrimSpace(strings.TrimPrefix(line, "- "))
			if item == "" {
				continue
			}
			trigger := EscalationTrigger{Name: item}
			if key, value, ok := strings.Cut(item, ":"); ok {
				trigger.Name = strings.TrimSpace(key)
				trigger.Value = strings.TrimSpace(value)
			}
			cfg.Triggers = append(cfg.Triggers, trigger)

		case "CHANNELS":
			if strings.EqualFold(line, "channels:") {
				continue
			}
			if strings.HasPrefix(line, "- ") {
				flushChannel()
				channel = &EscalationChannel{Metadata: map[string]string{}}
				if err := assignChannelField(channel, strings.TrimSpace(strings.TrimPrefix(line, "- ")), i+1); err != nil {
					return nil, err
				}
				continue
			}
			if channel == nil {
				continue
			}
			if err := assignChannelField(channel, line, i+1); err != nil {
				return nil, err
			}

		case "APPROVAL":
			if err := assignApprovalField(&cfg.Approval, line, i+1); err != nil {
				return nil, err
			}
		}
	}
	flushChannel()

	if len(cfg.Triggers) == 0 && len(cfg.Channels) == 0 && cfg.Approval == (EscalationApproval{}) {
		return nil, fmt.Errorf("ESCALATE.md did not contain recognized directives")
	}
	return cfg, nil
}

func assignChannelField(channel *EscalationChannel, line string, lineno int) error {
	key, value, ok := strings.Cut(line, ":")
	if !ok {
		return nil
	}
	key = strings.ToLower(strings.TrimSpace(key))
	value = cleanEscalateValue(value)
	switch key {
	case "type":
		channel.Type = value
	case "address":
		channel.Address = value
	case "channel":
		channel.Channel = value
	case "timeout_minutes":
		timeout, err := parseMinutes(value)
		if err != nil {
			return fmt.Errorf("ESCALATE.md line %d: %w", lineno, err)
		}
		channel.Timeout = timeout
	default:
		if channel.Metadata == nil {
			channel.Metadata = map[string]string{}
		}
		channel.Metadata[key] = value
	}
	return nil
}

func assignApprovalField(approval *EscalationApproval, line string, lineno int) error {
	key, value, ok := strings.Cut(line, ":")
	if !ok {
		return nil
	}
	key = strings.ToLower(strings.TrimSpace(key))
	value = cleanEscalateValue(value)
	switch key {
	case "approval_timeout_minutes":
		timeout, err := parseMinutes(value)
		if err != nil {
			return fmt.Errorf("ESCALATE.md line %d: %w", lineno, err)
		}
		approval.Timeout = timeout
	case "on_timeout":
		approval.OnTimeout = value
	case "on_denial":
		approval.OnDenial = value
	case "on_approval":
		approval.OnApproval = value
	}
	return nil
}

func parseMinutes(value string) (time.Duration, error) {
	value = cleanEscalateValue(value)
	minutes, err := strconv.Atoi(value)
	if err != nil {
		return 0, fmt.Errorf("invalid minute value %q: %w", value, err)
	}
	if minutes < 0 {
		return 0, fmt.Errorf("invalid minute value %q: must be non-negative", value)
	}
	return time.Duration(minutes) * time.Minute, nil
}

func cleanEscalateValue(value string) string {
	value = strings.TrimSpace(value)
	if len(value) >= 2 {
		if value[0] == '"' && value[len(value)-1] == '"' {
			return value[1 : len(value)-1]
		}
		if value[0] == '\'' && value[len(value)-1] == '\'' {
			return value[1 : len(value)-1]
		}
	}
	return value
}
