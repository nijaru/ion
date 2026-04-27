package main

import (
	"context"
	"errors"
	"flag"
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
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
	"github.com/nijaru/ion/internal/telemetry"
	ionworkspace "github.com/nijaru/ion/internal/workspace"
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
	modeFlag := flag.String("mode", "", "Permission mode: read, edit, or auto")
	yoloFlag := flag.Bool("yolo", false, "Start in AUTO mode (alias for --mode auto)")
	printFlag := flag.Bool("print", false, "Print response and exit (use with --prompt or stdin)")
	promptFlag := flag.String("prompt", "", "Prompt to send in print mode")
	promptShortFlag := flag.String("p", "", "Prompt to send and print response (implies --print)")
	outputFlag := flag.String("output", "text", "Print mode output: text or json")
	jsonFlag := flag.Bool("json", false, "Emit JSON in print mode")
	timeoutFlag := flag.Duration("timeout", 5*time.Minute, "Timeout for print mode")
	args, openResumePicker := normalizeFlagArgs(os.Args[1:])
	if err := flag.CommandLine.Parse(args); err != nil {
		fmt.Fprintf(os.Stderr, "%v\n", err)
		os.Exit(2)
	}

	// Load config
	cfg, err := config.Load()
	if err != nil {
		fmt.Fprintf(os.Stderr, "failed to load config: %v\n", err)
		os.Exit(1)
	}

	if *providerFlag != "" {
		cfg.Provider = *providerFlag
	}
	shutdownTelemetry, err := telemetry.Setup(context.Background(), cfg)
	if err != nil {
		fmt.Fprintf(os.Stderr, "failed to initialize telemetry: %v\n", err)
		os.Exit(1)
	}
	defer func() {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_ = shutdownTelemetry(ctx)
	}()
	mode, err := startupMode(cfg, *modeFlag, *yoloFlag)
	if err != nil {
		fmt.Fprintf(os.Stderr, "%v\n", err)
		os.Exit(2)
	}

	ctx := context.Background()
	cwd, _ := os.Getwd()
	branch := currentBranch()
	trustStore, trusted, trustNotice, err := loadWorkspaceTrust(cwd, cfg)
	if err != nil {
		fmt.Fprintf(os.Stderr, "failed to load workspace trust: %v\n", err)
		os.Exit(1)
	}
	if !trusted && mode != session.ModeRead {
		mode = session.ModeRead
	}
	escalation, err := loadEscalationConfig(cwd)
	if err != nil {
		fmt.Fprintf(os.Stderr, "failed to load ESCALATE.md: %v\n", err)
		os.Exit(1)
	}

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
		recent, err := recentSessionForContinue(ctx, store, cwd)
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

	printRequested, prompt, output, err := resolvePrintFlags(
		*printFlag,
		*promptFlag,
		*promptShortFlag,
		flag.Args(),
		*outputFlag,
		*jsonFlag,
	)
	if err != nil {
		fmt.Fprintf(os.Stderr, "%v\n", err)
		os.Exit(2)
	}

	// Print mode: run a single turn and exit
	if printRequested {
		if prompt == "" && isStdinPipe() {
			data, err := io.ReadAll(os.Stdin)
			if err != nil {
				fmt.Fprintf(os.Stderr, "failed to read stdin: %v\n", err)
				os.Exit(1)
			}
			prompt = string(data)
		}
		if prompt == "" {
			fmt.Fprintf(os.Stderr, "print mode requires --prompt or stdin pipe\n")
			os.Exit(1)
		}
		agent := b.Session()
		if agent == nil {
			fmt.Fprintf(os.Stderr, "print mode requires a configured provider and model\n")
			os.Exit(1)
		}
		configureSessionMode(agent, mode)
		if err := runPrintModeWithTimeout(
			ctx,
			os.Stdout,
			agent,
			prompt,
			*timeoutFlag,
			mode == session.ModeYolo,
			output,
		); err != nil {
			fmt.Fprintf(os.Stderr, "print mode error: %v\n", err)
			os.Exit(1)
		}
		return
	}

	startupLines := startupBannerLines(version, b.Provider(), b.Model(), sessionID != "")
	if trustNotice != "" {
		startupLines = append(startupLines, trustNotice)
	}
	if toolLine := startupToolLine(b); toolLine != "" {
		startupLines = append(startupLines, toolLine)
	}
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

	model := app.New(b, sess, store, cwd, branch, version, switcher).
		WithMode(mode).
		WithEscalation(escalation).
		WithTrust(trustStore, trusted, cfg.WorkspaceTrust)
	if openResumePicker {
		model = model.WithSessionPicker()
	}
	printStartup(
		os.Stdout,
		startupLines,
		workspaceHeader(cwd, branch),
		sessionID != "",
		model.RenderEntries(startupEntries...),
	)
	model = model.WithPrintedTranscript(len(startupEntries) > 0)
	p := tea.NewProgram(model)
	if _, err := p.Run(); err != nil {
		fmt.Fprintf(os.Stderr, "ion error: %v\n", err)
		os.Exit(1)
	}
}

func normalizeFlagArgs(args []string) ([]string, bool) {
	if len(args) > 1 && args[0] == "--" && strings.HasPrefix(args[1], "-") {
		args = args[1:]
	}
	normalized := make([]string, 0, len(args))
	openResumePicker := false
	for i := 0; i < len(args); i++ {
		arg := args[i]
		if arg != "-resume" && arg != "--resume" {
			normalized = append(normalized, arg)
			continue
		}
		if i+1 < len(args) && !strings.HasPrefix(args[i+1], "-") {
			normalized = append(normalized, arg, args[i+1])
			i++
			continue
		}
		openResumePicker = true
	}
	return normalized, openResumePicker
}

func recentSessionForContinue(ctx context.Context, store storage.Store, cwd string) (*storage.SessionInfo, error) {
	sessions, err := store.ListSessions(ctx, cwd)
	if err != nil {
		return nil, err
	}
	for i := range sessions {
		if !storage.IsConversationSessionInfo(sessions[i]) {
			continue
		}
		return &sessions[i], nil
	}
	return nil, nil
}

func loadWorkspaceTrust(cwd string, cfg *config.Config) (*ionworkspace.TrustStore, bool, string, error) {
	if cfg != nil && config.ResolveWorkspaceTrust(cfg.WorkspaceTrust) == "off" {
		return nil, true, "", nil
	}
	path, err := ionworkspace.DefaultTrustPath()
	if err != nil {
		return nil, false, "", err
	}
	store := ionworkspace.NewTrustStore(path)
	trusted, err := store.IsTrusted(cwd)
	if err != nil {
		return nil, false, "", err
	}
	if trusted {
		return store, true, "Workspace is trusted.", nil
	}
	return store, false, "Workspace is not trusted. Starting in READ mode. Run /trust to remember this workspace.", nil
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
	parts := []string{fmt.Sprintf("%d tools registered", surface.Count)}
	if surface.LazyEnabled {
		parts = append(parts, "Search tools enabled")
	}
	sandbox := strings.TrimSpace(surface.Sandbox)
	if sandbox != "" {
		parts = append(parts, "Sandbox "+sandbox)
	}
	return strings.Join(parts, " • ")
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
	if policyConfig, err := loadPolicyConfig(&runtimeCfg); err != nil {
		return nil, nil, err
	} else if policyConfig != nil {
		if policyBackend, ok := b.(backend.PolicyConfigurer); ok {
			policyBackend.SetPolicyConfig(policyConfig)
		}
	}

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

	sess := storage.NewLazySession(store, cwd, modelName, branch)
	b.SetSession(sess)
	if err := b.Session().Open(ctx); err != nil {
		_ = sess.Close()
		return nil, nil, fmt.Errorf("backend initialization error: %w", err)
	}
	return b, sess, nil
}

func loadPolicyConfig(cfg *config.Config) (*backend.PolicyConfig, error) {
	if cfg == nil {
		return nil, nil
	}
	path := cfg.PolicyPath
	if path == "" {
		defaultPath, err := config.DefaultPolicyPath()
		if err != nil {
			return nil, err
		}
		path = defaultPath
	}
	policyConfig, err := backend.LoadPolicyConfig(path)
	if err != nil {
		return nil, fmt.Errorf("failed to load policy config: %w", err)
	}
	return policyConfig, nil
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
