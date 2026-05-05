package app

import (
	"fmt"
	"strings"
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
		status := m.Progress.Status
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
	case stateApproval:
		line = m.st.warn.Render("⚠ Approval required")
	case stateCancelled:
		line = m.st.warn.Render("⚠ Canceled")
		if reason := strings.TrimSpace(m.Progress.BudgetStopReason); reason != "" {
			line += " • " + reason
		}
	case stateBlocked:
		line = m.st.warn.Render("⚠ Subagent blocked")
	case stateError:
		line = m.st.warn.Render("× Error")
	default:
		if status := strings.TrimSpace(m.configurationStatus()); status != "" {
			line = m.st.warn.Render("• " + status)
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
