package main

import (
	"context"
	"flag"
	"fmt"
	"io"
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
	ionworkspace "github.com/nijaru/ion/internal/workspace"
)

// version is set at build time via -ldflags "-X main.version=vX.Y.Z".
var version = "v0.0.0"

func main() {
	cli := registerCLIFlags()
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

	providerOverride := cli.providerOverride()
	modelOverride := cli.modelOverride()
	explicitRuntimeOverride := providerOverride != "" ||
		strings.TrimSpace(modelOverride) != "" ||
		strings.TrimSpace(os.Getenv("ION_PROVIDER")) != "" ||
		strings.TrimSpace(os.Getenv("ION_MODEL")) != ""
	applyCLIConfigOverrides(cfg, providerOverride, modelOverride, cli.thinkingOverride())
	mode, err := startupMode(cfg, cli.modeOverride(), cli.yolo())
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

	printRequested, prompt, output, err := resolvePrintFlags(
		cli.printRequested(),
		cli.printShortRequested(),
		cli.prompt(),
		flag.Args(),
		cli.output(),
		cli.jsonRequested(),
	)
	if err != nil {
		fmt.Fprintf(os.Stderr, "%v\n", err)
		os.Exit(2)
	}
	mode = applyWorkspaceTrustModeGate(mode, trusted, printRequested, cli.explicitModeRequested())
	if err := validatePrintSelection(printRequested, openResumePicker); err != nil {
		fmt.Fprintf(os.Stderr, "%v\n", err)
		os.Exit(2)
	}
	if err := validateSessionBundleSelection(cli.exportSessionPath(), cli.importSessionPath()); err != nil {
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
	if cli.importSessionPath() != "" {
		imported, err := importSessionBundleFile(ctx, store, cli.importSessionPath())
		closeErr := store.Close()
		if err != nil {
			fmt.Fprintf(os.Stderr, "failed to import session bundle: %v\n", err)
			os.Exit(1)
		}
		if closeErr != nil {
			fmt.Fprintf(os.Stderr, "failed to close storage: %v\n", closeErr)
			os.Exit(1)
		}
		printSessionBundleImport(os.Stdout, imported)
		return
	}

	sessionID, err := startupSessionID(
		ctx,
		store,
		cwd,
		cli.resumeID(),
		cli.resumeShortID(),
		cli.continueRequested(),
	)
	if err != nil {
		fmt.Fprintf(os.Stderr, "%v\n", err)
		os.Exit(1)
	}
	if cli.exportSessionPath() != "" {
		if sessionID == "" {
			fmt.Fprintln(os.Stderr, "--export-session requires --resume <id> or --continue")
			os.Exit(2)
		}
		exported, err := exportSessionBundleFile(ctx, store, sessionID, cli.exportSessionPath())
		closeErr := store.Close()
		if err != nil {
			fmt.Fprintf(os.Stderr, "failed to export session bundle: %v\n", err)
			os.Exit(1)
		}
		if closeErr != nil {
			fmt.Fprintf(os.Stderr, "failed to close storage: %v\n", closeErr)
			os.Exit(1)
		}
		printSessionBundleExport(os.Stdout, exported)
		return
	}
	if sessionID != "" && !explicitRuntimeOverride {
		if err := applySessionConfigFromMetadata(ctx, store, sessionID, cfg); err != nil {
			fmt.Fprintf(os.Stderr, "%v\n", err)
			os.Exit(1)
		}
	}
	runtimeCfg, activePreset, err := startupRuntimeConfig(
		ctx,
		cfg,
		sessionID,
		explicitRuntimeOverride,
	)
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
	configureSessionMode(b.Session(), mode)

	// Print mode: run a single turn and exit
	if printRequested {
		agent := b.Session()
		if agent == nil {
			fmt.Fprintf(os.Stderr, "print mode requires a configured provider and model\n")
			os.Exit(1)
		}
		runErr := runPrintModeWithTimeout(
			ctx,
			os.Stdout,
			agent,
			prompt,
			cli.timeout(),
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
		switchedBackend, switchedSession, err := openRuntime(
			ctx,
			store,
			cwd,
			currentBranch(),
			cfg,
			acpCommandOverride,
			sessionID,
		)
		if err != nil {
			return nil, nil, nil, err
		}
		return switchedBackend, switchedBackend.Session(), switchedSession, nil
	}

	model := app.New(b, sess, store, cwd, branch, version, switcher).
		WithConfigForRuntime(cfg, runtimeCfg).
		WithActivePreset(activePreset).
		WithMode(mode).
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

func loadWorkspaceTrust(
	cwd string,
	cfg *config.Config,
) (*ionworkspace.TrustStore, bool, string, error) {
	policy := "prompt"
	if cfg != nil {
		policy = config.ResolveWorkspaceTrust(cfg.WorkspaceTrust)
	}
	if policy == "off" {
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
		return store, true, "", nil
	}
	if policy == "strict" {
		return store, false, "Workspace: not trusted. READ mode active. Trust must be managed outside this session.", nil
	}
	return store, false, "Workspace: not trusted. READ mode active. Run /trust to enable edits.", nil
}

func applyWorkspaceTrustModeGate(
	mode session.Mode,
	trusted bool,
	printRequested bool,
	explicitModeRequested bool,
) session.Mode {
	if !trusted && mode != session.ModeRead {
		return session.ModeRead
	}
	return mode
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
