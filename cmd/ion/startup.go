package main

import (
	"context"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"

	"github.com/nijaru/ion/internal/app"
	"github.com/nijaru/ion/internal/backend"
)

func startupBannerLines(version string) []string {
	version = strings.TrimSpace(version)

	if version == "" {
		version = "v0.0.0"
	}
	return []string{"ion " + version}
}

func startupToolLine(b backend.Backend) string {
	summarizer, ok := b.(backend.ToolSummarizer)
	if !ok {
		return ""
	}
	surface := summarizer.ToolSurface()
	if surface.Count == 0 {
		return ""
	}
	var parts []string
	if surface.LazyEnabled {
		parts = append(parts, "Search tools enabled")
	}
	sandbox := strings.TrimSpace(surface.Sandbox)
	if sandbox != "" && sandbox != "off" {
		parts = append(parts, "Sandbox "+sandbox)
	}
	if len(parts) == 0 {
		return ""
	}
	return strings.Join(parts, " • ")
}

func startupKeyboardLine() string {
	if strings.TrimSpace(os.Getenv("TMUX")) == "" {
		return ""
	}
	return tmuxKeyboardLine(showTmuxOption)
}

func tmuxKeyboardLine(show func(string) (string, error)) string {
	extendedKeys, err := show("extended-keys")
	if err != nil {
		return ""
	}
	switch strings.TrimSpace(extendedKeys) {
	case "on", "always":
	default:
		return "tmux extended-keys is off; Shift+Enter may submit. Use Ctrl+J for newline or enable tmux extended-keys."
	}

	extendedKeysFormat, err := show("extended-keys-format")
	if err != nil {
		return ""
	}
	if strings.TrimSpace(extendedKeysFormat) == "xterm" {
		return "tmux extended-keys-format is xterm; Shift+Enter may be unreliable. Use Ctrl+J for newline or set extended-keys-format csi-u."
	}
	return ""
}

func showTmuxOption(option string) (string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	out, err := exec.CommandContext(ctx, "tmux", "show", "-gv", option).Output()
	if err != nil {
		return "", err
	}
	return strings.TrimSpace(string(out)), nil
}

func currentBranch() string {
	out, err := exec.Command("git", "rev-parse", "--abbrev-ref", "HEAD").Output()
	if err != nil {
		return "unknown"
	}
	return strings.TrimSpace(string(out))
}

func resumeHintSessionID(model tea.Model) string {
	appModel, ok := model.(*app.Model)
	if !ok || appModel == nil {
		return ""
	}
	return appModel.ResumeSessionID()
}

func printResumeHint(w io.Writer, sessionID string) {
	sessionID = strings.TrimSpace(sessionID)
	if sessionID == "" {
		return
	}
	fmt.Fprintf(w, "\nResume this session with:\nion --resume %s\n", sessionID)
}

func printStartup(
	out io.Writer,
	startupLines []string,
	workspaceLine string,
	resumed bool,
	renderedEntries []string,
) {
	if out == nil {
		return
	}
	var lines []string
	for _, line := range startupLines {
		lines = append(lines, styleStartupLine(line))
	}
	if workspaceLine != "" {
		lines = append(lines, startupWorkspaceStyle().Render(workspaceLine))
	}
	if resumed {
		lines = append(lines, "", startupMetaStyle().Render("--- resumed ---"))
	}
	if len(renderedEntries) > 0 {
		lines = append(lines, "")
	}
	lines = append(lines, renderedEntries...)
	if len(lines) == 0 {
		return
	}
	lines = append(lines, "")
	_, _ = fmt.Fprintln(out, strings.Join(lines, "\n"))
}

func workspaceHeader(cwd, branch string) string {
	home, _ := os.UserHomeDir()
	dir := shortenHomePath(cwd, home)
	parts := []string{dir}
	if strings.TrimSpace(branch) != "" {
		parts = append(parts, branch)
	}
	return strings.Join(parts, " • ")
}

func shortenHomePath(path, home string) string {
	if home == "" || path == "" {
		return path
	}
	if path == home {
		return "~"
	}
	prefix := strings.TrimRight(home, string(os.PathSeparator)) + string(os.PathSeparator)
	if strings.HasPrefix(path, prefix) {
		return "~" + string(os.PathSeparator) + strings.TrimPrefix(path, prefix)
	}
	return path
}

func styleStartupLine(line string) string {
	trimmed := strings.TrimSpace(line)
	if trimmed == "--- resumed ---" {
		return startupMetaStyle().Render(line)
	}
	if strings.HasPrefix(trimmed, "tmux ") {
		return startupWarnStyle().Render(line)
	}
	parts := strings.Split(line, " • ")
	if len(parts) == 0 {
		return line
	}
	if len(parts) >= 1 && strings.HasPrefix(parts[0], "ion ") {
		first := strings.TrimPrefix(parts[0], "ion ")
		parts[0] = startupNameStyle().Render("ion") + " " + startupVersionStyle().Render(first)
	}
	for i := 1; i < len(parts); i++ {
		parts[i] = startupMetaStyle().Render(parts[i])
	}
	sep := startupMetaStyle().Render(" • ")
	return strings.Join(parts, sep)
}

func startupNameStyle() lipgloss.Style {
	return lipgloss.NewStyle().Foreground(lipgloss.Color("6"))
}

func startupVersionStyle() lipgloss.Style {
	return lipgloss.NewStyle()
}

func startupMetaStyle() lipgloss.Style {
	return lipgloss.NewStyle().Foreground(lipgloss.Color("8"))
}

func startupWorkspaceStyle() lipgloss.Style {
	return startupMetaStyle()
}

func startupWarnStyle() lipgloss.Style {
	return lipgloss.NewStyle().Foreground(lipgloss.Color("3"))
}
