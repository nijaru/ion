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

	"github.com/nijaru/canto/workspace"
	"github.com/nijaru/ion/internal/app"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/features"
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
	continueShortFlag := flag.Bool("c", false, "Continue the most recent session in this directory")
	resumeFlag := flag.String("resume", "", "Resume a specific session by ID")
	resumeShortFlag := flag.String("r", "", "Resume a specific session by ID")
	providerFlag := flag.String("provider", "", "Provider to use")
	modelFlag := flag.String("model", "", "Model to use")
	modelShortFlag := flag.String("m", "", "Model to use")
	thinkingFlag := flag.String("thinking", "", "Thinking effort: auto, off, minimal, low, medium, high, xhigh")
	modeFlag := flag.String("mode", "", "Permission mode: read, edit, or auto")
	yoloFlag := flag.Bool("yolo", false, "Start in AUTO mode (alias for --mode auto)")
	printFlag := flag.Bool("print", false, "Print response and exit (use with --prompt or stdin)")
	promptFlag := flag.String("prompt", "", "Prompt to send in print mode")
	printShortFlag := flag.Bool("p", false, "Print response and exit (alias for --print)")
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

	providerOverride := strings.TrimSpace(*providerFlag)
	modelOverride := firstNonEmpty(*modelFlag, *modelShortFlag)
	explicitRuntimeOverride := providerOverride != "" ||
		strings.TrimSpace(modelOverride) != "" ||
		strings.TrimSpace(os.Getenv("ION_PROVIDER")) != "" ||
		strings.TrimSpace(os.Getenv("ION_MODEL")) != ""
	applyCLIConfigOverrides(cfg, providerOverride, modelOverride, *thinkingFlag)
	shutdownTelemetry := func(context.Context) error { return nil }
	if !features.CoreLoopOnly {
		shutdownTelemetry, err = telemetry.Setup(context.Background(), cfg)
		if err != nil {
			fmt.Fprintf(os.Stderr, "failed to initialize telemetry: %v\n", err)
			os.Exit(1)
		}
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
	explicitModeRequested := strings.TrimSpace(*modeFlag) != "" || *yoloFlag

	ctx := context.Background()
	cwd, _ := os.Getwd()
	branch := currentBranch()
	trustStore, trusted, trustNotice, err := loadWorkspaceTrust(cwd, cfg)
	if err != nil {
		fmt.Fprintf(os.Stderr, "failed to load workspace trust: %v\n", err)
		os.Exit(1)
	}
	var escalation *workspace.EscalationConfig
	if !features.CoreLoopOnly {
		escalation, err = loadEscalationConfig(cwd)
		if err != nil {
			fmt.Fprintf(os.Stderr, "failed to load ESCALATE.md: %v\n", err)
			os.Exit(1)
		}
	}

	printRequested, prompt, output, err := resolvePrintFlags(
		*printFlag,
		*printShortFlag,
		*promptFlag,
		flag.Args(),
		*outputFlag,
		*jsonFlag,
	)
	if err != nil {
		fmt.Fprintf(os.Stderr, "%v\n", err)
		os.Exit(2)
	}
	mode = applyWorkspaceTrustModeGate(mode, trusted, printRequested, explicitModeRequested)
	if err := validatePrintSelection(printRequested, openResumePicker); err != nil {
		fmt.Fprintf(os.Stderr, "%v\n", err)
		os.Exit(2)
	}
	if printRequested {
		if isStdinPipe() {
			data, err := io.ReadAll(os.Stdin)
			if err != nil {
				fmt.Fprintf(os.Stderr, "failed to read stdin: %v\n", err)
				os.Exit(1)
			}
			prompt = promptWithStdinContext(prompt, string(data))
		}
		if prompt == "" {
			fmt.Fprintf(os.Stderr, "print mode requires --prompt or stdin pipe\n")
			os.Exit(1)
		}
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

	sessionID, err := startupSessionID(
		ctx,
		store,
		cwd,
		*resumeFlag,
		*resumeShortFlag,
		*continueFlag || *continueShortFlag,
	)
	if err != nil {
		fmt.Fprintf(os.Stderr, "%v\n", err)
		os.Exit(1)
	}
	if sessionID != "" && !explicitRuntimeOverride {
		if err := applySessionConfigFromMetadata(ctx, store, sessionID, cfg); err != nil {
			fmt.Fprintf(os.Stderr, "%v\n", err)
			os.Exit(1)
		}
	}
	runtimeCfg, activePreset, err := startupRuntimeConfig(ctx, cfg, sessionID, explicitRuntimeOverride)
	if err != nil {
		fmt.Fprintf(os.Stderr, "failed to resolve runtime config: %v\n", err)
		os.Exit(1)
	}

	acpCommandOverride := strings.TrimSpace(os.Getenv("ION_ACP_COMMAND"))

	b, sess, err := openRuntime(ctx, store, cwd, branch, runtimeCfg, acpCommandOverride, sessionID)
	if err != nil {
		fmt.Fprintf(os.Stderr, "failed to initialize runtime: %v\n", err)
		os.Exit(1)
	}

	// Print mode: run a single turn and exit
	if printRequested {
		agent := b.Session()
		if agent == nil {
			fmt.Fprintf(os.Stderr, "print mode requires a configured provider and model\n")
			os.Exit(1)
		}
		configureSessionMode(agent, mode)
		runErr := runPrintModeWithTimeout(
			ctx,
			os.Stdout,
			agent,
			prompt,
			*timeoutFlag,
			mode == session.ModeYolo,
			output,
		)
		closeErr := closeRuntimeHandles(agent, sess, store)
		if runErr != nil {
			fmt.Fprintf(os.Stderr, "print mode error: %v\n", runErr)
			os.Exit(1)
		}
		if closeErr != nil {
			fmt.Fprintf(os.Stderr, "failed to close runtime: %v\n", closeErr)
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
		WithConfigForRuntime(cfg, runtimeCfg).
		WithActivePreset(activePreset).
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
	_, runErr := p.Run()
	closeErr := closeRuntimeHandles(b.Session(), sess, store)
	if runErr != nil {
		if closeErr != nil {
			fmt.Fprintf(os.Stderr, "failed to close runtime: %v\n", closeErr)
		}
		fmt.Fprintf(os.Stderr, "ion error: %v\n", runErr)
		os.Exit(1)
	}
	if closeErr != nil {
		fmt.Fprintf(os.Stderr, "failed to close runtime: %v\n", closeErr)
		os.Exit(1)
	}
}

func closeRuntimeHandles(agent session.AgentSession, sess storage.Session, store storage.Store) error {
	var errs []error
	if agent != nil {
		errs = append(errs, agent.Close())
	}
	if sess != nil {
		errs = append(errs, sess.Close())
	}
	if store != nil {
		errs = append(errs, store.Close())
	}
	return errors.Join(errs...)
}

func validatePrintSelection(printRequested, openResumePicker bool) error {
	if printRequested && openResumePicker {
		return fmt.Errorf("--resume requires a session ID in print mode")
	}
	return nil
}

func normalizeFlagArgs(args []string) ([]string, bool) {
	if len(args) > 1 && args[0] == "--" && strings.HasPrefix(args[1], "-") {
		args = args[1:]
	}
	flagArgs := make([]string, 0, len(args))
	positionals := make([]string, 0, len(args))
	openResumePicker := false
	allowFlagLikePositionals := false
	for i := 0; i < len(args); i++ {
		arg := args[i]
		if arg == "--" {
			positionals = append(positionals, args[i+1:]...)
			break
		}
		name, hasInlineValue, isKnown := ionFlagName(arg)
		if !isKnown {
			if strings.HasPrefix(arg, "-") && arg != "-" && !allowFlagLikePositionals {
				flagArgs = append(flagArgs, arg)
				continue
			}
			positionals = append(positionals, arg)
			continue
		}
		if name == "print" || name == "p" || name == "json" {
			allowFlagLikePositionals = true
		}
		switch {
		case (name == "resume" || name == "r") && !hasInlineValue:
			if i+1 < len(args) && !strings.HasPrefix(args[i+1], "-") {
				flagArgs = append(flagArgs, arg, args[i+1])
				i++
				continue
			}
			openResumePicker = true
		case ionFlagNeedsValue(name) && !hasInlineValue:
			flagArgs = append(flagArgs, arg)
			if i+1 < len(args) {
				flagArgs = append(flagArgs, args[i+1])
				i++
			}
		default:
			flagArgs = append(flagArgs, arg)
		}
	}
	if len(positionals) == 0 {
		return flagArgs, openResumePicker
	}
	normalized := make([]string, 0, len(flagArgs)+1+len(positionals))
	normalized = append(normalized, flagArgs...)
	normalized = append(normalized, "--")
	normalized = append(normalized, positionals...)
	return normalized, openResumePicker
}

func ionFlagName(arg string) (string, bool, bool) {
	if !strings.HasPrefix(arg, "-") || arg == "-" {
		return "", false, false
	}
	name := strings.TrimLeft(arg, "-")
	if name == "" {
		return "", false, false
	}
	if before, _, found := strings.Cut(name, "="); found {
		name = before
		return name, true, ionKnownFlag(name)
	}
	return name, false, ionKnownFlag(name)
}

func ionKnownFlag(name string) bool {
	switch name {
	case "continue", "c", "resume", "r", "provider", "model", "m", "thinking", "mode", "yolo", "print", "prompt", "p", "output", "json", "timeout":
		return true
	default:
		return false
	}
}

func ionFlagNeedsValue(name string) bool {
	switch name {
	case "resume", "r", "provider", "model", "m", "thinking", "mode", "prompt", "output", "timeout":
		return true
	default:
		return false
	}
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

func startupSessionID(
	ctx context.Context,
	store storage.Store,
	cwd string,
	resumeID string,
	resumeShortID string,
	continueRequested bool,
) (string, error) {
	if resumeID != "" {
		return resumeID, nil
	}
	if resumeShortID != "" {
		return resumeShortID, nil
	}
	if !continueRequested {
		return "", nil
	}
	recent, err := recentSessionForContinue(ctx, store, cwd)
	if err != nil {
		return "", fmt.Errorf("failed to find recent session: %w", err)
	}
	if recent == nil {
		return "", fmt.Errorf("no conversation session to continue in this directory")
	}
	return recent.ID, nil
}

func loadWorkspaceTrust(cwd string, cfg *config.Config) (*ionworkspace.TrustStore, bool, string, error) {
	if features.CoreLoopOnly {
		return nil, true, "", nil
	}
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
		return store, true, "Workspace: trusted.", nil
	}
	return store, false, "Workspace: not trusted. READ mode active. Run /trust to enable edits.", nil
}

func applyWorkspaceTrustModeGate(
	mode session.Mode,
	trusted bool,
	printRequested bool,
	explicitModeRequested bool,
) session.Mode {
	if features.CoreLoopOnly {
		return mode
	}
	if trusted || mode == session.ModeRead {
		return mode
	}
	if printRequested && explicitModeRequested {
		return mode
	}
	return session.ModeRead
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
	parts := []string{fmt.Sprintf("Tools: %d registered", surface.Count)}
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
		b := backend.NewUnconfigured(&runtimeCfg, err)
		b.SetStore(store)
		if sessionID == "" {
			return b, nil, nil
		}
		sess, resumeErr := store.ResumeSession(ctx, sessionID)
		if resumeErr != nil {
			return nil, nil, fmt.Errorf("failed to resume session %s: %w", sessionID, resumeErr)
		}
		b.SetSession(sess)
		return b, sess, nil
	}

	b, err := backendForProvider(runtimeCfg.Provider)
	if err != nil {
		return nil, nil, err
	}
	b.SetStore(store)
	b.SetConfig(&runtimeCfg)
	if !features.CoreLoopOnly {
		if policyConfig, err := loadPolicyConfig(&runtimeCfg); err != nil {
			return nil, nil, err
		} else if policyConfig != nil {
			if policyBackend, ok := b.(backend.PolicyConfigurer); ok {
				policyBackend.SetPolicyConfig(policyConfig)
			}
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
	lines = append(lines, "")
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
	trimmed := strings.TrimSpace(line)
	if trimmed == "--- resumed ---" {
		return startupMetaStyle().Render(line)
	}
	if strings.HasPrefix(trimmed, "Workspace: not trusted.") {
		return startupWarnStyle().Render(line)
	}
	if strings.HasPrefix(trimmed, "Workspace: trusted.") {
		return startupOKStyle().Render(line)
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

func startupOKStyle() lipgloss.Style {
	return lipgloss.NewStyle().Foreground(lipgloss.Color("2"))
}
