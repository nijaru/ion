package app

import (
	"fmt"
	"strings"
	"time"
)

func (m Model) renderQueuedTurns() string {
	if len(m.InFlight.QueuedTurns) == 0 {
		return ""
	}
	preview := compactQueuedText(m.InFlight.QueuedTurns[0])
	label := fmt.Sprintf("• Queued (Ctrl+G edit): %s", preview)
	if extra := len(m.InFlight.QueuedTurns) - 1; extra > 0 {
		label += fmt.Sprintf(" • +%d more", extra)
	}
	return m.st.dim.Render(fitLine(label, m.shellWidth()))
}

func compactQueuedText(text string) string {
	return strings.Join(strings.Fields(strings.TrimSpace(text)), " ")
}

// progressLine renders the single-line progress indicator between Plane B and the composer.
func (m Model) progressLine() string {
	var line string
	idleReady := false
	if m.Progress.Compacting {
		line = m.Input.Spinner.View() + " Compacting context..."
		if n := len(m.InFlight.QueuedTurns); n > 0 {
			line += m.st.dim.Render(fmt.Sprintf(" • %d queued", n))
		}
		return fitLine(strings.TrimRight(line, " "), m.shellWidth())
	}
	switch m.Progress.Mode {
	case stateIonizing, stateStreaming, stateWorking:
		status := retryCountdownStatus(
			m.Progress.Status,
			m.Progress.StatusUpdatedAt,
			time.Now(),
		)
		if isIdleStatus(status) || isConfigurationStatus(status) {
			switch m.Progress.Mode {
			case stateIonizing:
				status = "Ionizing..."
			case stateStreaming:
				status = "Streaming..."
			case stateWorking:
				if len(m.InFlight.Subagents) > 0 {
					for _, k := range sortedKeys(m.InFlight.Subagents) {
						status = "Waiting for " + m.InFlight.Subagents[k].Name + "..."
						break
					}
				} else {
					status = "Working..."
				}
			}
		}
		line = m.Input.Spinner.View() + " " + status
		if stats := m.runningProgressParts(); len(stats) > 0 {
			line += m.renderProgressStats(stats)
		}
	case stateComplete:
		line = m.st.success.Render("✓") + " Complete"
		if stats := m.completedProgressParts(); len(stats) > 0 {
			line += m.renderProgressStats(stats)
		}
	case stateCancelled:
		line = m.st.warn.Render("⚠ Canceled")
		if reason := strings.TrimSpace(m.Progress.BudgetStopReason); reason != "" {
			line += " • " + reason
		}
	case stateBlocked:
		line = m.st.warn.Render("⚠ Subagent blocked")
	case stateError:
		if m.suppressTerminalErrorProgress() {
			return ""
		}
		line = m.st.warn.Render("× Error")
	default:
		if status := strings.TrimSpace(m.configurationStatus()); status != "" {
			line = m.st.warn.Render("• " + status)
		} else if status := strings.TrimSpace(m.Progress.LocalStatus); status != "" {
			line = m.st.dim.Render("• " + status)
		} else if status := strings.TrimSpace(m.Progress.Status); !isIdleStatus(status) && !isConfigurationStatus(status) {
			line = m.st.dim.Render("• " + status)
		} else {
			idleReady = true
			line = m.st.dim.Render("• Ready")
		}
	}
	if n := len(m.InFlight.QueuedTurns); n > 0 {
		line += m.st.dim.Render(fmt.Sprintf(" • %d queued", n))
	}
	if idleReady && m.suppressIdleReadyProgress() {
		return ""
	}
	return fitLine(strings.TrimRight(line, " "), m.shellWidth())
}

func (m Model) suppressIdleReadyProgress() bool {
	return m.App.PrintedTranscript && len(m.InFlight.QueuedTurns) == 0
}

func (m Model) suppressTerminalErrorProgress() bool {
	return m.App.PrintedTranscript && len(m.InFlight.QueuedTurns) == 0
}

func (m Model) renderProgressStats(parts []string) string {
	if len(parts) == 0 {
		return ""
	}
	var b strings.Builder
	for _, part := range parts {
		b.WriteString(m.st.dim.Render(" • "))
		b.WriteString(m.st.dim.Render(part))
	}
	return b.String()
}

func retryCountdownStatus(status string, updatedAt, now time.Time) string {
	if updatedAt.IsZero() || now.IsZero() {
		return status
	}
	prefix, rest, ok := strings.Cut(status, "Retrying in ")
	if !ok {
		return status
	}
	delayText, suffix, ok := strings.Cut(rest, "...")
	if !ok {
		return status
	}
	delay, err := time.ParseDuration(strings.TrimSpace(delayText))
	if err != nil {
		return status
	}
	remaining := updatedAt.Add(delay).Sub(now)
	if remaining <= 0 {
		return prefix + "Retrying now..." + suffix
	}
	return prefix + "Retrying in " + roundUpSecond(remaining).String() + "..." + suffix
}

func roundUpSecond(d time.Duration) time.Duration {
	if d <= 0 {
		return 0
	}
	return ((d + time.Second - 1) / time.Second) * time.Second
}
