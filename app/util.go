package app

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

func systemEntry(content string) session.Entry {
	entry, _ := session.EntrySystem(content, time.Time{})
	return entry
}

func now() int64 { return time.Now().Unix() }

// clearVisibleScreenCmd clears the visible inline frame after terminal width
// shrink, then asks Bubble Tea's renderer to discard its old frame. The raw
// clear handles rows already reflowed by the terminal; ClearScreen keeps the
// renderer state aligned for the next managed draw.
func clearVisibleScreenCmd() tea.Cmd {
	return tea.Sequence(
		tea.Raw(ansi.CursorHomePosition+ansi.EraseEntireScreen),
		tea.ClearScreen,
	)
}

func (m Model) renderHelpLine(index int, line string) string {
	if index == 0 || isHelpSectionLine(line) {
		return m.st.cyan.Bold(true).Render(line)
	}
	if key, sep, detail, ok := splitHelpDetail(line); ok {
		return "  " + m.st.cyan.Render(key) + sep + detail
	}
	return line
}

func splitHelpDetail(line string) (string, string, string, bool) {
	if !strings.HasPrefix(line, "  ") {
		return "", "", "", false
	}
	trimmed := strings.TrimSpace(line)
	if trimmed == "" {
		return "", "", "", false
	}
	rest := strings.TrimLeft(line, " ")
	for i := 0; i < len(rest)-1; i++ {
		if rest[i] == ' ' && rest[i+1] == ' ' {
			key := strings.TrimSpace(rest[:i])
			j := i
			for j < len(rest) && rest[j] == ' ' {
				j++
			}
			sep := rest[i:j]
			detail := strings.TrimSpace(rest[j:])
			if key == "" || detail == "" {
				return "", "", "", false
			}
			return key, sep, detail, true
		}
	}
	return "", "", "", false
}

func isHelpSectionLine(line string) bool {
	switch strings.TrimSpace(line) {
	case "commands", "keys":
		return true
	default:
		return false
	}
}

func (m *Model) clearProgressError() {
	m.progressReducer().clearError()
}

func (m *Model) clearPendingAction() {
	m.inputReducer().clearPendingAction()
}

func (m *Model) armPendingAction(action pendingAction) tea.Cmd {
	m.inputReducer().armPendingAction(action)
	switch action {
	case pendingActionQuitCtrlC, pendingActionQuitCtrlD:
	default:
		m.clearPendingAction()
		return nil
	}
	return tea.Tick(pendingActionTimeout, func(time.Time) tea.Msg {
		return clearPendingMsg{action: action}
	})
}

func (m Model) pendingActionStatus() string {
	switch m.Input.Pending {
	case pendingActionQuitCtrlC:
		return "Press Ctrl+C again to quit"
	case pendingActionQuitCtrlD:
		return "Press Ctrl+D again to quit"
	default:
		return ""
	}
}

func isConfigurationStatus(status string) bool {
	trimmed := strings.TrimSpace(status)
	if trimmed == "" {
		return false
	}
	lower := strings.ToLower(trimmed)
	return trimmed == noProviderConfiguredStatus() ||
		trimmed == noModelConfiguredStatus() ||
		strings.HasPrefix(lower, "provider and model are required")
}

func noProviderConfiguredStatus() string {
	return NoProviderConfiguredStatus
}

func noModelConfiguredStatus() string {
	return NoModelConfiguredStatus
}

func toolSurfaceSummary(surface ToolSurface) string {
	if surface.Count == 0 {
		return "No tools registered"
	}
	parts := []string{}
	if surface.LazyEnabled {
		parts = append(parts, fmt.Sprintf("search tools enabled above %d", surface.LazyThreshold))
	}
	sandbox := strings.TrimSpace(surface.Sandbox)
	if sandbox != "" {
		parts = append(parts, "sandbox "+sandbox)
	}
	environment := strings.TrimSpace(surface.Environment)
	if environment != "" {
		parts = append(parts, strings.ToLower(ToolEnvironmentSummary(environment)))
	}
	suffix := ""
	if len(parts) > 0 {
		suffix = " (" + strings.Join(parts, "; ") + ")"
	}
	names := strings.Join(surface.Names, ", ")
	lines := []string{fmt.Sprintf("Tools: %d%s", surface.Count, suffix)}
	if names != "" {
		lines = append(lines, "Registered: "+names)
	}
	activeNames := surface.ActiveNames
	if len(activeNames) > 0 && !slices.Equal(activeNames, surface.Names) {
		mode := strings.TrimSpace(surface.Mode)
		if mode == "" {
			mode = "coding"
		}
		lines = append(lines, fmt.Sprintf("Active (%s): %s", mode, strings.Join(activeNames, ", ")))
	}
	return strings.Join(lines, "\n")
}

func runtimeStatusSummary(m Model) string {
	lines := []string{"Permissions: confirm by default"}
	if provider := m.runtimeProvider(); provider != "" {
		lines = append(lines, "Provider: "+llm.DisplayName(provider)+" ("+provider+")")
	}
	if model := m.runtimeModel(); model != "" {
		lines = append(lines, "Model: "+model)
	}
	if reasoning := m.Model.Runtime.Reasoning; reasoning != "" {
		lines = append(lines, "Thinking: "+reasoning)
	}
	if preset := m.Model.Runtime.Preset; preset != "" {
		lines = append(lines, "Preset: "+string(preset))
	}
	if sessionID := m.Model.Runtime.SessionID; sessionID != "" {
		lines = append(lines, "Session: "+sessionID[:8]+"...")
	}
	// Token usage
	progress := m.Progress
	if progress.TokensSent > 0 || progress.TokensReceived > 0 {
		lines = append(lines, fmt.Sprintf("Tokens: %s sent, %s received",
			compactCount(progress.TokensSent), compactCount(progress.TokensReceived)))
	}
	if progress.TotalCost > 0 {
		lines = append(lines, fmt.Sprintf("Cost: $%.4f", progress.TotalCost))
	}
	if summarizer, ok := m.Model.Info.(ToolSummarizer); ok {
		surface := summarizer.ToolSurface()
		lines = append(lines, toolSurfaceSummary(surface))
	}
	return strings.Join(lines, "\n")
}

func compactCount(n int) string {
	switch {
	case n < 1000:
		return fmt.Sprintf("%d", n)
	case n < 10_000:
		return fmt.Sprintf("%.1fk", float64(n)/1000.0)
	case n < 999_500:
		return fmt.Sprintf("%dk", (n+500)/1000)
	case n < 10_000_000:
		return fmt.Sprintf("%.1fM", float64(n)/1_000_000.0)
	default:
		return fmt.Sprintf("%dM", (n+500_000)/1_000_000)
	}
}

func isIdleStatus(status string) bool {
	trimmed := strings.TrimSpace(status)
	if trimmed == "" {
		return true
	}
	switch strings.ToLower(trimmed) {
	case "ready", "connected via ion", "connected via acp":
		return true
	default:
		return false
	}
}

func fitLine(line string, width int) string {
	if width <= 0 {
		return ""
	}
	if ansi.StringWidth(line) <= width {
		return line
	}
	return ansi.Truncate(line, width, "…")
}

func joinLineSegments(sep string, segments ...string) string {
	filtered := make([]string, 0, len(segments))
	for _, segment := range segments {
		if segment != "" {
			filtered = append(filtered, segment)
		}
	}
	if len(filtered) == 0 {
		return ""
	}
	return strings.Join(filtered, sep)
}

func sortedKeys[V any](m map[string]V) []string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	slices.Sort(keys)
	return keys
}

// handleDebugCommand writes a debug log to a file for troubleshooting.
func (m Model) handleDebugCommand() (Model, tea.Cmd) {
	if m.localCommandBusy() {
		return m, cmdError(m.localCommandBusyMessage("/debug"))
	}

	var b strings.Builder
	fmt.Fprintf(&b, "Debug output at %s\n", time.Now().Format(time.RFC3339))
	fmt.Fprintf(&b, "Go: %s\n", runtime.Version())
	fmt.Fprintf(&b, "OS: %s/%s\n", runtime.GOOS, runtime.GOARCH)
	fmt.Fprintf(&b, "\n")

	// Provider/model info
	if provider := m.runtimeProvider(); provider != "" {
		fmt.Fprintf(&b, "Provider: %s (%s)\n", llm.DisplayName(provider), provider)
	}
	if model := m.runtimeModel(); model != "" {
		fmt.Fprintf(&b, "Model: %s\n", model)
	}
	if reasoning := m.Model.Runtime.Reasoning; reasoning != "" {
		fmt.Fprintf(&b, "Thinking: %s\n", reasoning)
	}
	if preset := m.Model.Runtime.Preset; preset != "" {
		fmt.Fprintf(&b, "Preset: %s\n", string(preset))
	}
	fmt.Fprintf(&b, "\n")

	// Token usage
	progress := m.Progress
	fmt.Fprintf(&b, "Token usage:\n")
	fmt.Fprintf(&b, "  Sent: %d\n", progress.TokensSent)
	fmt.Fprintf(&b, "  Received: %d\n", progress.TokensReceived)
	fmt.Fprintf(&b, "  Context: %d\n", progress.ContextTokens)
	fmt.Fprintf(&b, "  Cost: $%.6f\n", progress.TotalCost)
	fmt.Fprintf(&b, "\n")

	// Session info
	if m.Model.Storage != nil {
		sessID := m.Model.Storage.ID()
		fmt.Fprintf(&b, "Session: %s\n", sessID)
		entries, err := m.Model.Storage.Entries(context.Background())
		if err != nil {
			fmt.Fprintf(&b, "  Error loading entries: %v\n", err)
		} else {
			fmt.Fprintf(&b, "Entries: %d\n", len(entries))
			for i, e := range entries {
				role := string(session.EntryRole(e))
				content := session.EntryContent(e)
				if len(content) > 200 {
					content = content[:200] + "..."
				}
				// JSON encode for safe output
				entryJSON, _ := json.Marshal(map[string]any{
					"index":   i,
					"role":    role,
					"content": content,
				})
				fmt.Fprintf(&b, "  %s\n", string(entryJSON))
			}
		}
	}
	fmt.Fprintf(&b, "\n")

	// Write to file
	home, err := os.UserHomeDir()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to get home dir: %v", err))
	}
	debugDir := filepath.Join(home, ".ion")
	if mkErr := os.MkdirAll(debugDir, 0o755); mkErr != nil {
		return m, cmdError(fmt.Sprintf("failed to create debug dir: %v", mkErr))
	}
	debugPath := filepath.Join(debugDir, "debug.log")
	if writeErr := os.WriteFile(debugPath, []byte(b.String()), 0o644); writeErr != nil {
		return m, cmdError(fmt.Sprintf("failed to write debug log: %v", writeErr))
	}

	msg := fmt.Sprintf("Debug log written to %s", debugPath)
	return m, m.terminalCommit().Entries(systemEntry(msg))
}
