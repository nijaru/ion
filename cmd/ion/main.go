package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"os"
	"os/exec"
	"strings"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"

	"github.com/nijaru/ion/internal/app"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

// version is set at build time via -ldflags "-X main.version=vX.Y.Z".
var version = "v0.0.0"

func main() {
	continueFlag := flag.Bool(
		"continue",
		false,
		"Continue the most recent session in this directory",
	)
	resumeFlag := flag.String("resume", "", "Resume a specific session by ID")
	providerFlag := flag.String("provider", "", "Provider to use")
	flag.Parse()

	// Load config
	cfg, err := config.Load()
	if err != nil {
		fmt.Fprintf(os.Stderr, "failed to load config: %v\n", err)
		os.Exit(1)
	}

	if *providerFlag != "" {
		cfg.Provider = *providerFlag
	}

	ctx := context.Background()
	cwd, _ := os.Getwd()
	branch := currentBranch()

	dataDir, err := config.DefaultDataDir()
	if err != nil {
		fmt.Fprintf(os.Stderr, "failed to resolve data dir: %v\n", err)
		os.Exit(1)
	}

	// Initialize storage from the internal data dir.
	store, err := storage.NewCantoStore(dataDir)
	if err != nil {
		fmt.Fprintf(os.Stderr, "failed to initialize storage: %v\n", err)
		os.Exit(1)
	}

	var sessionID string
	if *resumeFlag != "" {
		sessionID = *resumeFlag
	} else if *continueFlag {
		recent, err := store.GetRecentSession(ctx, cwd)
		if err == nil && recent != nil {
			sessionID = recent.ID
		}
	}

	acpCommandOverride := strings.TrimSpace(os.Getenv("ION_ACP_COMMAND"))

	b, sess, err := openRuntime(ctx, store, cwd, branch, cfg, acpCommandOverride, sessionID)
	if err != nil {
		fmt.Fprintf(os.Stderr, "failed to initialize runtime: %v\n", err)
		os.Exit(1)
	}

	startupLines := startupBannerLines(version, b.Provider(), b.Model(), sessionID != "")
	var startupEntries []session.Entry
	if sess != nil {
		entries, err := sess.Entries(ctx)
		if err != nil {
			fmt.Fprintf(os.Stderr, "failed to load startup history: %v\n", err)
		} else {
			startupEntries = entries
		}
	}
	switcher := func(ctx context.Context, cfg *config.Config, sessionID string) (backend.Backend, session.AgentSession, storage.Session, error) {
		switchedBackend, switchedSession, err := openRuntime(ctx, store, cwd, currentBranch(), cfg, acpCommandOverride, sessionID)
		if err != nil {
			return nil, nil, nil, err
		}
		return switchedBackend, switchedBackend.Session(), switchedSession, nil
	}

	printStartup(os.Stdout, startupLines, workspaceHeader(cwd, branch), startupEntries, b.Bootstrap().Status)
	model := app.New(b, sess, store, cwd, branch, version, switcher).WithPrintedTranscript(len(startupEntries) > 0)
	p := tea.NewProgram(model)
	if _, err := p.Run(); err != nil {
		fmt.Fprintf(os.Stderr, "ion error: %v\n", err)
		os.Exit(1)
	}
}

func openRuntime(ctx context.Context, store storage.Store, cwd, branch string, cfg *config.Config, acpCommandOverride string, sessionID string) (backend.Backend, storage.Session, error) {
	runtimeCfg := *cfg
	if err := resolveStartupConfig(&runtimeCfg); err != nil {
		if errors.Is(err, errNoProviderConfigured) || errors.Is(err, errNoModelConfigured) {
			b := backend.NewUnconfigured(&runtimeCfg, err)
			b.SetStore(store)
			return b, nil, nil
		}
		return nil, nil, err
	}

	b, err := backendForProvider(runtimeCfg.Provider)
	if err != nil {
		return nil, nil, err
	}
	b.SetStore(store)
	b.SetConfig(&runtimeCfg)

	if isACPProvider(runtimeCfg.Provider) {
		command := strings.TrimSpace(acpCommandOverride)
		if command == "" {
			derived, ok := defaultACPCommand(runtimeCfg.Provider)
			if !ok {
				return nil, nil, fmt.Errorf("ION_ACP_COMMAND environment variable not set")
			}
			command = derived
		}
		if err := os.Setenv("ION_ACP_COMMAND", command); err != nil {
			return nil, nil, fmt.Errorf("failed to set ION_ACP_COMMAND: %w", err)
		}
	}

	if sessionID != "" {
		sess, err := store.ResumeSession(ctx, sessionID)
		if err != nil {
			return nil, nil, fmt.Errorf("failed to resume session %s: %w", sessionID, err)
		}
		b.SetSession(sess)
		if err := b.Session().Resume(ctx, sessionID); err != nil {
			_ = sess.Close()
			return nil, nil, fmt.Errorf("backend resume error: %w", err)
		}
		if err := syncSessionMetadata(ctx, store, sessionID, sessionModelName(runtimeCfg.Provider, runtimeCfg.Model), branch); err != nil {
			_ = sess.Close()
			return nil, nil, fmt.Errorf("failed to update resumed session metadata: %w", err)
		}
		return b, sess, nil
	}

	modelName := sessionModelName(runtimeCfg.Provider, runtimeCfg.Model)
	if modelName == "" {
		return nil, nil, fmt.Errorf("provider and model must be set (e.g. provider=\"openrouter\" model=\"openai/gpt-5.4\")")
	}

	sess, err := store.OpenSession(ctx, cwd, modelName, branch)
	if err != nil {
		return nil, nil, fmt.Errorf("failed to open session: %w", err)
	}
	b.SetSession(sess)
	if err := b.Session().Open(ctx); err != nil {
		_ = sess.Close()
		return nil, nil, fmt.Errorf("backend initialization error: %w", err)
	}
	return b, sess, nil
}

func syncSessionMetadata(ctx context.Context, store storage.Store, sessionID, modelName, branch string) error {
	if store == nil || sessionID == "" {
		return nil
	}
	return store.UpdateSession(ctx, storage.SessionInfo{
		ID:     sessionID,
		Model:  modelName,
		Branch: branch,
	})
}

func currentBranch() string {
	out, err := exec.Command("git", "rev-parse", "--abbrev-ref", "HEAD").Output()
	if err != nil {
		return "unknown"
	}
	return strings.TrimSpace(string(out))
}

func printStartup(out *os.File, startupLines []string, workspaceLine string, entries []session.Entry, status string) {
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
	if strings.TrimSpace(status) != "" && !isConfigurationStatus(status) {
		lines = append(lines, "")
		lines = append(lines, renderStartupEntry(session.Entry{Role: session.System, Content: status}))
	}
	if len(entries) > 0 {
		lines = append(lines, "")
	}
	for _, entry := range entries {
		lines = append(lines, renderStartupEntry(entry))
	}
	if len(lines) == 0 {
		return
	}
	_, _ = fmt.Fprintln(out, strings.Join(lines, "\n"))
}

func workspaceHeader(cwd, branch string) string {
	home, _ := os.UserHomeDir()
	dir := cwd
	if home != "" && strings.HasPrefix(dir, home) {
		dir = "~" + dir[len(home):]
	}
	parts := []string{dir}
	if strings.TrimSpace(branch) != "" {
		parts = append(parts, branch)
	}
	return strings.Join(parts, " • ")
}

func renderStartupEntry(entry session.Entry) string {
	switch entry.Role {
	case session.User:
		return "› " + entry.Content
	case session.System:
		return "• " + entry.Content
	case session.Tool:
		if entry.Title != "" {
			if entry.Content == "" {
				return "• " + entry.Title
			}
			return "• " + entry.Title + "\n" + entry.Content
		}
		return "• " + entry.Content
	case session.Agent, session.Assistant:
		return entry.Content
	default:
		return entry.Content
	}
}

func isConfigurationStatus(status string) bool {
	trimmed := strings.TrimSpace(status)
	if trimmed == "" {
		return false
	}
	lower := strings.ToLower(trimmed)
	return strings.HasPrefix(lower, "no provider configured") ||
		strings.HasPrefix(lower, "no model configured") ||
		strings.HasPrefix(lower, "provider and model are required")
}

func styleStartupLine(line string) string {
	if strings.TrimSpace(line) == "--- resumed ---" {
		return startupMetaStyle().Render(line)
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
	return lipgloss.NewStyle().Faint(true)
}

func startupWorkspaceStyle() lipgloss.Style {
	return lipgloss.NewStyle().Faint(true)
}
