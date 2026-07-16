package app

import (
	"fmt"
	"strings"
	"time"
)

type progressReducer struct {
	progress *ProgressState
}

func (m *Model) progressReducer() progressReducer {
	return progressReducer{progress: &m.Progress}
}

func (r progressReducer) beginLocalStatus(status string) {
	r.setLocalStatus(status)
}

func (r progressReducer) clearLocalBusyStatus() {
	if r.progress.LocalStatus != "" {
		r.setLocalStatus("")
	}
	if IsLocalBusyStatus(r.progress.Status) {
		r.setStatus("")
	}
}

func (r progressReducer) beginCompaction() {
	r.progress.Compacting = true
	r.setStatus("Compacting context...")
}

func (r progressReducer) completeCompaction() {
	r.progress.Compacting = false
	r.progress.ContextTokens = 0
	r.setStatus("Ready")
	r.clearError()
}

func (r progressReducer) clearError() {
	if r.progress.Mode == StateError {
		r.progress.Mode = StateReady
	}
	r.progress.LastError = ""
}

func (r progressReducer) setReasoningEffort(value string) {
	r.progress.ReasoningEffort = value
}

func (r progressReducer) applyRuntimeSnapshot(snapshot Snapshot) {
	r.setReasoningEffort(snapshot.Reasoning)
	if snapshot.Status != "" {
		r.setStatus(snapshot.Status)
	}
}

func (r progressReducer) markRuntimeReady() {
	r.progress.Mode = StateReady
}

func (r progressReducer) resetSessionUsage() {
	r.progress.TokensSent = 0
	r.progress.TokensReceived = 0
	r.progress.ContextTokens = 0
	r.progress.TotalCost = 0
}

func (r progressReducer) applySessionUsage(input, output int, cost float64) {
	r.progress.TokensSent = input
	r.progress.TokensReceived = output
	r.progress.TotalCost = cost
}

func (r progressReducer) setStatus(status string) {
	r.progress.Status = status
	if status == "" {
		r.progress.StatusUpdatedAt = time.Time{}
		return
	}
	r.progress.StatusUpdatedAt = time.Now()
}

func (r progressReducer) setLocalStatus(status string) {
	r.progress.LocalStatus = status
	if status == "" {
		r.progress.LocalStatusAt = time.Time{}
		return
	}
	r.progress.LocalStatusAt = time.Now()
}

func (m Model) renderQueuedTurns() string {
	count := m.queuedInputCount()
	if count == 0 {
		return ""
	}
	kind, text := m.firstQueuedInput()
	preview := compactQueuedText(text)
	label := fmt.Sprintf("• %s (Alt+Up edit): %s", kind, preview)
	if extra := count - 1; extra > 0 {
		label += fmt.Sprintf(" • +%d more", extra)
	}
	return m.st.dim.Render(fitLine(label, m.shellWidth()))
}

func (m Model) queuedInputCount() int {
	return len(m.InFlight.QueuedSteering) + len(m.InFlight.QueuedTurns)
}

func (m Model) firstQueuedInput() (string, string) {
	if len(m.InFlight.QueuedSteering) > 0 {
		return "Steering", m.InFlight.QueuedSteering[0]
	}
	if len(m.InFlight.QueuedTurns) > 0 {
		return "Queued", m.InFlight.QueuedTurns[0]
	}
	return "Queued", ""
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
		if n := m.queuedInputCount(); n > 0 {
			line += m.st.dim.Render(fmt.Sprintf(" • %d queued", n))
		}
		return fitLine(strings.TrimRight(line, " "), m.shellWidth())
	}
	switch m.Progress.Mode {
	case StateIonizing, StateStreaming, StateWorking:
		status := retryCountdownStatus(
			m.Progress.Status,
			m.Progress.StatusUpdatedAt,
			time.Now(),
		)
		if isIdleStatus(status) || isConfigurationStatus(status) {
			switch m.Progress.Mode {
			case StateIonizing:
				status = "Ionizing..."
			case StateStreaming:
				status = "Streaming..."
			case StateWorking:
				status = "Working..."
			}
		}
		line = m.Input.Spinner.View() + " " + status
		if stats := m.runningProgressParts(); len(stats) > 0 {
			line += m.renderProgressStats(stats)
		}
	case StateComplete:
		line = m.st.success.Render("✓") + " Complete"
		if stats := m.completedProgressParts(); len(stats) > 0 {
			line += m.renderProgressStats(stats)
		}
	case StateCancelled:
		line = m.st.warn.Render("⚠ Canceled")
		if reason := strings.TrimSpace(m.Progress.BudgetStopReason); reason != "" {
			line += " • " + reason
		}
	case StateError:
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
	if n := m.queuedInputCount(); n > 0 {
		line += m.st.dim.Render(fmt.Sprintf(" • %d queued", n))
	}
	if idleReady && m.suppressIdleReadyProgress() {
		return ""
	}
	return fitLine(strings.TrimRight(line, " "), m.shellWidth())
}

func (m Model) suppressIdleReadyProgress() bool {
	return m.App.PrintedTranscript && m.queuedInputCount() == 0
}

func (m Model) suppressTerminalErrorProgress() bool {
	return m.App.PrintedTranscript && m.queuedInputCount() == 0
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
