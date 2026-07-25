package app

import (
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
	if recovery := actionRecoverySummary(m.Model.Recovery); recovery != "" {
		lines = append(lines, recovery)
	}
	if interrupted := interruptedTurnSummary(m.Model.InterruptedTurns); interrupted != "" {
		lines = append(lines, interrupted)
	}
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
		if len(m.Model.ActiveTools) > 0 {
			surface.ActiveNames = slices.Clone(m.Model.ActiveTools)
		}
		lines = append(lines, toolSurfaceSummary(surface))
	}
	return strings.Join(lines, "\n")
}

func actionRecoverySummary(actions []session.ActionRecord) string {
	if len(actions) == 0 {
		return ""
	}
	lines := []string{fmt.Sprintf("Action recovery: %d unsettled external action(s); verify before retry", len(actions))}
	limit := min(len(actions), 8)
	for _, action := range actions[:limit] {
		line := fmt.Sprintf("- %s: %s %s", action.ID, action.Tool, action.State)
		if reason := strings.Join(strings.Fields(action.Error), " "); reason != "" {
			if len(reason) > 160 {
				reason = reason[:160] + "..."
			}
			line += " — " + reason
		}
		lines = append(lines, line)
	}
	if len(actions) > limit {
		lines = append(lines, fmt.Sprintf("- ... and %d more; see /actions", len(actions)-limit))
	}
	return strings.Join(lines, "\n")
}

func interruptedTurnSummary(turns []session.TurnRecord) string {
	if len(turns) == 0 {
		return ""
	}
	lines := []string{fmt.Sprintf("Interrupted turns: %d retained and excluded from replay; use /turns to inspect", len(turns))}
	limit := min(len(turns), 8)
	for _, turn := range turns[:limit] {
		input := strings.Join(strings.Fields(turn.Input), " ")
		if input == "" {
			input = "(no input)"
		}
		if len(input) > 120 {
			input = input[:120] + "..."
		}
		lines = append(lines, fmt.Sprintf("- %s: %s", turn.ID, input))
	}
	if len(turns) > limit {
		lines = append(lines, fmt.Sprintf("- ... and %d more; see /turns", len(turns)-limit))
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

type debugLogWrittenMsg struct {
	generation uint64
	path       string
	err        error
}

// handleDebugCommand writes a debug log to a file for troubleshooting. The
// active-session section is read by the runtime projection command so debug
// output cannot observe a different branch or hide an in-flight durable turn.
func (m Model) handleDebugCommand() (Model, tea.Cmd) {
	if m.localCommandBusy() {
		return m, cmdError(m.localCommandBusyMessage("/debug"))
	}
	runner := m.Model.Runner
	storage := m.Model.Storage
	generation := m.Model.EventGeneration
	ctx := m.runtimeOperationContext()
	return m, func() tea.Msg {
		projection, err := loadSessionProjection(ctx, runner, storage)
		if err != nil {
			return debugLogWrittenMsg{
				generation: generation,
				err:        fmt.Errorf("failed to load debug session projection: %w", err),
			}
		}
		if err := ctx.Err(); err != nil {
			return debugLogWrittenMsg{generation: generation, err: fmt.Errorf("write debug log: %w", err)}
		}

		var b strings.Builder
		fmt.Fprintf(&b, "Debug output at %s\n", time.Now().Format(time.RFC3339))
		fmt.Fprintf(&b, "Go: %s\n", runtime.Version())
		fmt.Fprintf(&b, "OS: %s/%s\n", runtime.GOOS, runtime.GOARCH)
		fmt.Fprintf(&b, "\n")

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

		progress := m.Progress
		fmt.Fprintf(&b, "Token usage:\n")
		fmt.Fprintf(&b, "  Sent: %d\n", progress.TokensSent)
		fmt.Fprintf(&b, "  Received: %d\n", progress.TokensReceived)
		fmt.Fprintf(&b, "  Context: %d\n", progress.ContextTokens)
		fmt.Fprintf(&b, "  Cost: $%.6f\n", progress.TotalCost)
		fmt.Fprintf(&b, "\n")

		if projection.ID != "" || len(projection.Branch) > 0 {
			fmt.Fprintf(&b, "Session: %s\n", projection.ID)
			fmt.Fprintf(&b, "Entries: %d\n", len(projection.Branch))
			for i, e := range projection.Branch {
				content := session.EntryContent(e)
				if len(content) > 200 {
					content = content[:200] + "..."
				}
				entryJSON, _ := json.Marshal(map[string]any{
					"index":   i,
					"role":    string(session.EntryRole(e)),
					"content": content,
				})
				fmt.Fprintf(&b, "  %s\n", string(entryJSON))
			}
		}
		fmt.Fprintf(&b, "\n")

		home, err := os.UserHomeDir()
		if err != nil {
			return debugLogWrittenMsg{
				generation: generation,
				err:        fmt.Errorf("failed to get home dir: %w", err),
			}
		}
		if err := ctx.Err(); err != nil {
			return debugLogWrittenMsg{generation: generation, err: fmt.Errorf("write debug log: %w", err)}
		}
		debugDir := filepath.Join(home, ".ion")
		if err := os.MkdirAll(debugDir, 0o755); err != nil {
			return debugLogWrittenMsg{
				generation: generation,
				err:        fmt.Errorf("failed to create debug dir: %w", err),
			}
		}
		if err := ctx.Err(); err != nil {
			return debugLogWrittenMsg{generation: generation, err: fmt.Errorf("write debug log: %w", err)}
		}
		debugPath := filepath.Join(debugDir, "debug.log")
		if err := os.WriteFile(debugPath, []byte(b.String()), 0o644); err != nil {
			return debugLogWrittenMsg{
				generation: generation,
				err:        fmt.Errorf("failed to write debug log: %w", err),
			}
		}
		return debugLogWrittenMsg{generation: generation, path: debugPath}
	}
}
