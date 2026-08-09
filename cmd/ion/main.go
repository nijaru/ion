package main

import (
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"runtime/debug"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/term"
	"github.com/nijaru/ion/app"
	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/ctxerr"
	"github.com/nijaru/ion/internal/agent"
	ionexport "github.com/nijaru/ion/internal/export"
	ionskills "github.com/nijaru/ion/internal/skills"
	"github.com/nijaru/ion/internal/timing"
	ionworkspace "github.com/nijaru/ion/internal/workspace"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
	"github.com/nijaru/ion/tool"
)

// version is set at build time via -ldflags "-X main.version=vX.Y.Z".
var version = "v0.0.0"

func main() {
	os.Exit(runCLI(os.Args[1:], os.Stdout, os.Stderr))
}

func runCLI(args []string, stdout, stderr io.Writer) int {
	if handled, code := runTopLevelCommand(args, stdout, stderr); handled {
		return code
	}

	timing.Record("cli-parse")

	flagSet := flag.NewFlagSet("ion", flag.ContinueOnError)
	flagSet.SetOutput(stderr)
	cli := registerCLIFlags(flagSet)
	normalizedArgs, openResumePicker := normalizeFlagArgs(args)
	if err := flagSet.Parse(normalizedArgs); err != nil {
		if errors.Is(err, flag.ErrHelp) {
			return 0
		}
		return 2
	}
	displayVersion := resolvedVersion()
	if cli.versionRequested() {
		var conflictingFlags []string
		if openResumePicker {
			conflictingFlags = append(conflictingFlags, "--resume")
		}
		flagSet.Visit(func(f *flag.Flag) {
			if f.Name != "version" && ionKnownFlag(f.Name) {
				conflictingFlags = append(conflictingFlags, "--"+f.Name)
			}
		})
		if err := validateVersionSelection(flagSet.Args(), conflictingFlags); err != nil {
			fmt.Fprintf(stderr, "%v\n", err)
			return 2
		}
		printVersion(stdout, displayVersion)
		return 0
	}

	// Load config
	cfg, err := config.Load()
	if err != nil {
		fmt.Fprintf(stderr, "failed to load config: %v\n", err)
		return 1
	}
	for _, w := range config.Validate(cfg) {
		fmt.Fprintf(stderr, "config warning: %s\n", w)
	}
	timing.Record("config-load")

	providerOverride := cli.providerOverride()
	modelOverride := cli.modelOverride()
	explicitRuntimeOverride := providerOverride != "" ||
		strings.TrimSpace(modelOverride) != "" ||
		cli.trustModeOverride() != "" ||
		strings.TrimSpace(os.Getenv("ION_PROVIDER")) != "" ||
		strings.TrimSpace(os.Getenv("ION_MODEL")) != ""
	applyCLIConfigOverrides(cfg, providerOverride, modelOverride, cli.thinkingOverride())
	applyCLITrustModeOverride(cfg, cli.trustModeOverride())
	cfg.APIKeyOverride = cli.apiKeyOverride()
	cfg.APIKeyOverrideProvider = llm.ResolveID(cfg.Provider)
	endpointResolver := llm.NewEndpointResolver(llm.EndpointResolverOptions{})
	catalog := llm.NewModelCatalog(llm.ModelCatalogOptions{
		EndpointResolver: endpointResolver,
	})
	selectionRequested := cli.sessionID() != "" || cli.resumeID() != "" ||
		cli.resumeShortID() != "" || cli.continueRequested() || openResumePicker
	forkRequested := cli.forkRequested()
	if cfg.APIKeyOverride != "" && firstNonEmpty(cfg.Model, cfg.FastModel, cfg.SummaryModel) == "" &&
		!selectionRequested && cli.exportSessionPath() == "" && cli.importSessionPath() == "" &&
		!cli.listModelsRequested() {
		if err := validateAPIKeyOverride(cfg.APIKeyOverride, ""); err != nil {
			fmt.Fprintf(stderr, "%v\n", err)
			return 2
		}
	}

	ctx := context.Background()
	cwd, err := os.Getwd()
	if err != nil {
		fmt.Fprintf(stderr, "failed to resolve working directory: %v\n", err)
		return 1
	}
	projectTrustRoot, err := config.TrustedProjectRoot(cwd)
	if err != nil {
		fmt.Fprintf(stderr, "failed to resolve project trust: %v\n", err)
		return 1
	}
	if cli.trustProjectRequested() {
		if err := config.TrustProject(cwd); err != nil {
			fmt.Fprintf(stderr, "failed to trust project: %v\n", err)
			return 1
		}
		projectTrustRoot, err = config.TrustedProjectRoot(cwd)
		if err != nil {
			fmt.Fprintf(stderr, "failed to resolve project trust: %v\n", err)
			return 1
		}
	}
	branch := currentBranch()

	listModelsSearch, err := resolveListModelsSearch(cli.listModelsRequested(), flagSet.Args())
	if err != nil {
		fmt.Fprintf(stderr, "%v\n", err)
		return 2
	}
	printArgs := flagSet.Args()
	if cli.listModelsRequested() {
		printArgs = nil
		if cli.printRequested() || cli.printShortRequested() || cli.prompt() != "" || cli.jsonRequested() {
			fmt.Fprintln(stderr, "--list-models cannot be combined with print-mode flags")
			return 2
		}
	}
	printRequested, prompt, output, err := resolvePrintFlags(
		cli.printRequested(),
		cli.printShortRequested(),
		cli.prompt(),
		printArgs,
		cli.output(),
		cli.jsonRequested(),
	)
	if err != nil {
		fmt.Fprintf(stderr, "%v\n", err)
		return 2
	}
	if err := validatePrintSelection(printRequested, openResumePicker); err != nil {
		fmt.Fprintf(stderr, "%v\n", err)
		return 2
	}
	if err := validatePrintTimeout(printRequested, cli.timeout()); err != nil {
		fmt.Fprintf(stderr, "%v\n", err)
		return 2
	}
	if err := validateSessionBundleSelection(cli.exportSessionPath(), cli.importSessionPath()); err != nil {
		fmt.Fprintf(stderr, "%v\n", err)
		return 2
	}
	if err := validateSessionSelection(
		cli.noSessionRequested(),
		cli.sessionID(),
		cli.resumeID(),
		cli.resumeShortID(),
		cli.continueRequested(),
		openResumePicker,
		cli.exportSessionPath(),
		cli.importSessionPath(),
	); err != nil {
		fmt.Fprintf(stderr, "%v\n", err)
		return 2
	}
	if err := validateForkSelection(
		forkRequested,
		cli.noSessionRequested(),
		printRequested,
		cli.listModelsRequested(),
		cli.sessionID(),
		cli.resumeID(),
		cli.resumeShortID(),
		cli.continueRequested(),
		openResumePicker,
		cli.exportSessionPath(),
		cli.importSessionPath(),
	); err != nil {
		fmt.Fprintf(stderr, "%v\n", err)
		return 2
	}
	if printRequested {
		if isStdinPipe() {
			data, err := io.ReadAll(os.Stdin)
			if err != nil {
				fmt.Fprintf(stderr, "failed to read stdin: %v\n", err)
				return 1
			}
			prompt = promptWithStdinContext(prompt, string(data))
		}
		if prompt == "" {
			fmt.Fprintf(stderr, "print mode requires --prompt or stdin pipe\n")
			return 1
		}
	}

	if cli.listModelsRequested() {
		if err := runListModels(ctx, stdout, stderr, cfg, listModelsSearch, catalog); err != nil {
			fmt.Fprintf(stderr, "--list-models: %v\n", err)
			return 1
		}
		return 0
	}

	store, err := openStartupStore(cli.noSessionRequested(), cli.sessionDirOverride())
	if err != nil {
		fmt.Fprintf(stderr, "failed to initialize storage: %v\n", err)
		return 1
	}
	timing.Record("store-open")

	if cli.importSessionPath() != "" {
		imported, err := importSessionBundleFile(ctx, store, cli.importSessionPath())
		closeErr := store.Close()
		if err != nil {
			fmt.Fprintf(stderr, "failed to import session bundle: %v\n", err)
			return 1
		}
		if closeErr != nil {
			fmt.Fprintf(stderr, "failed to close storage: %v\n", closeErr)
			return 1
		}
		printSessionBundleImport(stdout, imported)
		return 0
	}

	var sessionID string
	if openResumePicker {
		width, height := terminalSize(stdout)
		pickerModel := app.New(nil, nil, store, cwd, branch, displayVersion, nil).
			WithConfig(cfg).
			WithModelCatalog(catalog).
			WithEndpointResolver(endpointResolver).
			WithSize(width, height).
			WithSessionPreStartupMode()

		p := tea.NewProgram(&pickerModel)
		finalPickerModel, pickerErr := p.Run()
		if pickerErr != nil {
			closeErr := closeSessionPickerResources(finalPickerModel, store)
			if closeErr != nil {
				fmt.Fprintf(stderr, "failed to close session picker: %v\n", closeErr)
			}
			fmt.Fprintf(stderr, "session picker error: %v\n", pickerErr)
			return 1
		}

		appPickerModel, ok := finalPickerModel.(*app.Model)
		if !ok || appPickerModel == nil {
			closeErr := closeSessionPickerResources(finalPickerModel, store)
			if closeErr != nil {
				fmt.Fprintf(stderr, "failed to close session picker: %v\n", closeErr)
				return 1
			}
			return 0
		}

		selectedSessionID := appPickerModel.SelectedSessionID()
		if selectedSessionID == "" {
			closeErr := closeSessionPickerResources(appPickerModel, store)
			if closeErr != nil {
				fmt.Fprintf(stderr, "failed to close session picker: %v\n", closeErr)
				return 1
			}
			return 0
		}

		// The picker owns only frontend work. Keep the shared store open for the
		// selected session, but stop the picker before constructing the runtime.
		appPickerModel.Close()
		sessionID = selectedSessionID
		openResumePicker = false
	} else {
		var err error
		sessionID, err = startupSessionID(
			ctx,
			store,
			cwd,
			cli.sessionID(),
			cli.resumeID(),
			cli.resumeShortID(),
			cli.continueRequested(),
		)
		if err != nil {
			closeStartupStore(stderr, store)
			fmt.Fprintf(stderr, "%v\n", err)
			return 1
		}
	}
	if cli.exportSessionPath() != "" {
		if sessionID == "" {
			closeStartupStore(stderr, store)
			fmt.Fprintln(
				stderr,
				"--export-session requires --session <id>, --resume <id>, or --continue",
			)
			return 2
		}
		exported, err := exportSessionBundleFile(ctx, store, sessionID, cli.exportSessionPath())
		closeErr := store.Close()
		if err != nil {
			fmt.Fprintf(stderr, "failed to export session bundle: %v\n", err)
			return 1
		}
		if closeErr != nil {
			fmt.Fprintf(stderr, "failed to close storage: %v\n", closeErr)
			return 1
		}
		printSessionBundleExport(stdout, exported)
		return 0
	}
	if forkRequested {
		forkedID, err := ionexport.ForkSession(ctx, store, sessionID)
		if err != nil {
			closeStartupStore(stderr, store)
			fmt.Fprintf(stderr, "failed to fork session %s: %v\n", sessionID, err)
			return 1
		}
		sessionID = forkedID
	}
	resolvedCWD, resolvedBranch, err := runtimeLocationForSession(ctx, store, sessionID, cwd, branch)
	if err != nil {
		closeStartupStore(stderr, store)
		fmt.Fprintf(stderr, "failed to resolve session workspace: %v\n", err)
		return 1
	}
	if resolvedCWD != cwd || resolvedBranch != branch {
		cwd = resolvedCWD
		branch = resolvedBranch
		projectTrustRoot, err = config.TrustedProjectRoot(cwd)
		if err != nil {
			closeStartupStore(stderr, store)
			fmt.Fprintf(stderr, "failed to resolve session project trust: %v\n", err)
			return 1
		}
		if cli.trustProjectRequested() {
			if err := config.TrustProject(cwd); err != nil {
				closeStartupStore(stderr, store)
				fmt.Fprintf(stderr, "failed to trust session project: %v\n", err)
				return 1
			}
			projectTrustRoot, err = config.TrustedProjectRoot(cwd)
			if err != nil {
				closeStartupStore(stderr, store)
				fmt.Fprintf(stderr, "failed to resolve session project trust: %v\n", err)
				return 1
			}
		}
	}
	if sessionID != "" && !explicitRuntimeOverride {
		if err := applySessionConfigFromMetadata(ctx, store, sessionID, cfg); err != nil {
			closeStartupStore(stderr, store)
			fmt.Fprintf(stderr, "%v\n", err)
			return 1
		}
	}
	cfg.APIKeyOverrideProvider = llm.ResolveID(cfg.Provider)
	if err := validateAPIKeyOverride(
		cfg.APIKeyOverride,
		firstNonEmpty(cfg.Model, cfg.FastModel, cfg.SummaryModel),
	); err != nil {
		closeStartupStore(stderr, store)
		fmt.Fprintf(stderr, "%v\n", err)
		return 2
	}
	jobs := tool.NewJobManager()
	runtimeCfg, activePreset, err := startupRuntimeConfig(
		ctx,
		cfg,
		sessionID,
		explicitRuntimeOverride,
	)
	if err != nil {
		closeStartupStore(stderr, store)
		fmt.Fprintf(stderr, "failed to resolve runtime config: %v\n", err)
		return 1
	}

	persistResumedSessionModel := sessionID != ""
	b, sess, runner, err := openRuntime(
		ctx,
		store,
		jobs,
		cwd,
		branch,
		runtimeCfg,
		endpointResolver,
		sessionID,
		persistResumedSessionModel,
		cli.systemPromptOverride(),
		cli.appendSystemPromptOverride(),
		projectTrustRoot,
		catalog,
		!printRequested,
	)
	if err != nil {
		if printRequested || b == nil || b.Name() != "setup" {
			closeErr := errors.Join(jobs.Close(), store.Close())
			if closeErr != nil {
				fmt.Fprintf(stderr, "failed to close runtime: %v\n", closeErr)
			}
			if printRequested {
				fmt.Fprintf(stderr, "print mode error: %v\n", err)
			} else {
				fmt.Fprintf(stderr, "failed to initialize runtime: %v\n", err)
			}
			return 1
		}
		// Provider setup failures remain actionable in the interactive TUI.
		// The setup runtime has no runner and cannot be accepted by a runtime
		// switch; the original error is shown in the bootstrap status.
		fmt.Fprintf(stderr, "runtime setup required: %v\n", err)
	}
	// Print mode: run a single turn and exit
	if printRequested {
		if runner == nil {
			message := "print mode requires a configured provider and model"
			if b != nil {
				if status := strings.TrimSpace(b.Bootstrap().Status); status != "" {
					message = status
				}
			}
			closeErr := errors.Join(jobs.Close(), store.Close())
			if closeErr != nil {
				fmt.Fprintf(stderr, "failed to close runtime: %v\n", closeErr)
			}
			fmt.Fprintf(stderr, "print mode error: %s\n", message)
			return 1
		}
		runErr := runPrintModeWithTimeout(
			ctx,
			stdout,
			runner,
			prompt,
			cli.timeout(),
			output,
		)
		if runErr == nil {
			runErr = updatePrintSessionInfo(ctx, runner, cwd, branch, runtimeCfg, prompt)
		}
		closeErr := jobs.Close()
		closeErr = errors.Join(closeErr, closeRuntimeHandles(runner, store))
		if runErr != nil {
			fmt.Fprintf(stderr, "print mode error: %v\n", runErr)
			return 1
		}
		if closeErr != nil {
			fmt.Fprintf(stderr, "failed to close runtime: %v\n", closeErr)
			return 1
		}
		return 0
	}

	startupLines := startupBannerLines(displayVersion)
	if toolLine := startupToolLine(b); toolLine != "" {
		startupLines = append(startupLines, toolLine)
	}
	if keyboardLine := startupKeyboardLine(); keyboardLine != "" {
		startupLines = append(startupLines, keyboardLine)
	}
	var startupEntries []session.Entry
	if sess != nil {
		entries, err := sess.Entries(ctx)
		if err != nil {
			fmt.Fprintf(stderr, "failed to load startup history: %v\n", err)
		} else {
			startupEntries = entries
		}
	}
	switcher := func(ctx context.Context, cfg *config.Config, sessionID string) (app.RuntimeInfo, agent.Runtime, app.RuntimeStorage, error) {
		switchedBackend, switchedSession, switchedRunner, err := openRuntime(
			ctx,
			store,
			jobs,
			cwd,
			currentBranch(),
			cfg,
			endpointResolver,
			sessionID,
			true,
			cli.systemPromptOverride(),
			cli.appendSystemPromptOverride(),
			projectTrustRoot,
			catalog,
			true,
		)
		if err != nil {
			return nil, nil, nil, err
		}
		return switchedBackend, switchedRunner, switchedSession, nil
	}
	reloadConfig := func() (*config.Config, error) {
		// Startup flags are process-lifetime authority. Reapply them after
		// loading disk/state/env so /reload cannot silently discard --provider,
		// --model, --thinking, --trust-mode, or --api-key.
		return loadEffectiveConfigForReload(
			config.Load,
			providerOverride,
			modelOverride,
			cli.thinkingOverride(),
			cli.trustModeOverride(),
			cli.apiKeyOverride(),
		)
	}

	width, height := terminalSize(stdout)
	memoryPath, memoryPathErr := defaultMemoryPath()
	if memoryPathErr != nil {
		fmt.Fprintf(stderr, "warning: workspace memory unavailable: %v\n", memoryPathErr)
	}
	checkpointPath, checkpointPathErr := ionworkspace.DefaultCheckpointPath()
	if checkpointPathErr != nil {
		fmt.Fprintf(stderr, "warning: workspace checkpoints unavailable: %v\n", checkpointPathErr)
	}

	model := app.New(b, sess, store, cwd, branch, displayVersion, switcher).
		WithRunner(runner).
		WithModelCatalog(catalog).
		WithEndpointResolver(endpointResolver).
		WithJobs(tuiJobController{manager: jobs}).
		WithMemory(tuiMemoryController{path: memoryPath, scope: cwd}).
		WithConfigLoader(reloadConfig).
		WithConfigForRuntimePreset(cfg, runtimeCfg, activePreset).
		WithSize(width, height)
	if checkpointPathErr == nil {
		model = model.WithCheckpoints(tuiCheckpointController{path: checkpointPath, workspace: cwd})
	}
	if openResumePicker {
		model = model.WithSessionPicker()
	} else if startupSetupRequired(b) || startupProviderMissing(b) {
		model = model.WithProviderPicker()
	} else if startupModelMissing(b) {
		model = model.WithModelPicker()
	}
	// Skip startup banner when opening the resume picker — the resume flow
	// will print its own header after the user selects a session.
	if !openResumePicker {
		printStartup(
			stdout,
			startupLines,
			workspaceHeader(cwd, branch),
			sessionID != "",
			model.RenderEntries(startupEntries...),
		)
		model = model.WithPrintedTranscript(len(startupEntries) > 0)
	}
	timing.Record("tui-init")
	timing.Print()
	p := tea.NewProgram(&model)
	finalModel, runErr := p.Run()
	closeAppModel(finalModel)
	agentToClose := runtimeHandlesForClose(finalModel, runner)
	closeErr := jobs.Close()
	closeErr = errors.Join(closeErr, closeRuntimeHandles(agentToClose, store))
	if runErr != nil {
		if closeErr != nil {
			fmt.Fprintf(stderr, "failed to close runtime: %v\n", closeErr)
		}
		fmt.Fprintf(stderr, "ion error: %v\n", runErr)
		return 1
	}
	if closeErr != nil {
		fmt.Fprintf(stderr, "failed to close runtime: %v\n", closeErr)
		return 1
	}
	if sessionID := resumeHintSessionID(finalModel); sessionID != "" && !cli.noSessionRequested() {
		printResumeHint(stdout, sessionID)
	}
	return 0
}

func closeStartupStore(stderr io.Writer, store interface{ Close() error }) {
	if store == nil {
		return
	}
	if err := store.Close(); err != nil {
		fmt.Fprintf(stderr, "failed to close storage: %v\n", err)
	}
}

func closeAppModel(model tea.Model) {
	if appModel, ok := model.(*app.Model); ok && appModel != nil {
		appModel.Close()
	}
}

func closeSessionPickerResources(model tea.Model, store interface{ Close() error }) error {
	closeAppModel(model)
	if store == nil {
		return nil
	}
	return store.Close()
}

func terminalSize(w io.Writer) (int, int) {
	file, ok := w.(*os.File)
	if !ok {
		return 80, 24
	}
	width, height, err := term.GetSize(file.Fd())
	if err != nil || width <= 0 {
		return 80, 24
	}
	return width, height
}

func startupProviderMissing(b app.RuntimeInfo) bool {
	return b != nil && strings.TrimSpace(b.Provider()) == ""
}

func printVersion(w io.Writer, value string) {
	value = strings.TrimSpace(value)
	if value == "" {
		value = "v0.0.0"
	}
	fmt.Fprintf(w, "ion %s\n", value)
}

func resolvedVersion() string {
	buildInfoVersion := ""
	if info, ok := debug.ReadBuildInfo(); ok && info != nil {
		buildInfoVersion = info.Main.Version
	}
	return resolveVersion(version, buildInfoVersion)
}

func resolveVersion(ldflagVersion, buildInfoVersion string) string {
	if value := strings.TrimSpace(ldflagVersion); value != "" && value != "v0.0.0" {
		return value
	}
	if value := strings.TrimSpace(buildInfoVersion); value != "" && value != "(devel)" {
		return value
	}
	return "v0.0.0"
}

func validateVersionSelection(args, conflictingFlags []string) error {
	if len(args) > 0 {
		return fmt.Errorf("--version cannot be combined with positional arguments")
	}
	if len(conflictingFlags) > 0 {
		return fmt.Errorf("--version cannot be combined with %s", strings.Join(conflictingFlags, ", "))
	}
	return nil
}

func startupSetupRequired(b app.RuntimeInfo) bool {
	return b != nil && b.Name() == "setup"
}

func startupModelMissing(b app.RuntimeInfo) bool {
	return b != nil &&
		strings.TrimSpace(b.Provider()) != "" &&
		strings.TrimSpace(b.Model()) == ""
}

func runtimeHandlesForClose(
	finalModel tea.Model,
	fallbackRunner agent.Runtime,
) agent.Runtime {
	if model, ok := finalModel.(*app.Model); ok && model != nil {
		return model.Model.Runner
	}
	return fallbackRunner
}

func runtimeSessionID(runner agent.Runtime) string {
	if runner == nil {
		return ""
	}
	reader, ok := runner.(agent.SessionReader)
	if !ok {
		return ""
	}
	return strings.TrimSpace(reader.SessionID())
}

func runtimeLeafID(ctx context.Context, runner agent.Runtime) (string, error) {
	reader, ok := runner.(agent.SessionReader)
	if !ok {
		return "", fmt.Errorf("runtime does not expose a session tree")
	}
	tree, err := reader.SessionTree(ctx)
	if err != nil {
		return "", fmt.Errorf("read runtime session tree: %w", err)
	}
	return strings.TrimSpace(tree.LeafID), nil
}

type printResult struct {
	SessionID    string   `json:"session_id,omitempty"`
	Response     string   `json:"response"`
	InputTokens  int      `json:"input_tokens,omitempty"`
	OutputTokens int      `json:"output_tokens,omitempty"`
	Cost         float64  `json:"cost,omitempty"`
	ToolCalls    []string `json:"tool_calls,omitempty"`
}

func resolvePrintFlags(
	printFlag bool,
	printShort bool,
	promptLong string,
	args []string,
	output string,
	jsonOutput bool,
) (bool, string, string, error) {
	output = strings.ToLower(strings.TrimSpace(output))
	if output == "" {
		output = "text"
	}
	if output != "text" && output != "json" && output != "events" {
		return false, "", "", fmt.Errorf("unsupported print output %q (want text, json, or events)", output)
	}
	if jsonOutput {
		output = "json"
	}

	promptLong = strings.TrimSpace(promptLong)
	prompt := promptLong

	printRequested := printFlag || printShort || prompt != "" || jsonOutput
	if printRequested && prompt == "" && len(args) > 0 {
		prompt = strings.Join(args, " ")
	}
	if printRequested && prompt != "" && len(args) > 0 && promptLong != "" {
		return false, "", "", fmt.Errorf(
			"unexpected arguments after --prompt: %s",
			strings.Join(args, " "),
		)
	}
	if !printRequested && len(args) > 0 {
		return false, "", "", fmt.Errorf("unexpected arguments: %s", strings.Join(args, " "))
	}

	return printRequested, prompt, output, nil
}

func updatePrintSessionInfo(
	ctx context.Context,
	runner agent.Runtime,
	cwd string,
	branch string,
	cfg *config.Config,
	prompt string,
) error {
	if runner == nil || cfg == nil {
		return nil
	}
	id, err := runtimeLeafID(ctx, runner)
	if err != nil {
		return err
	}
	if id == "" {
		return nil
	}
	preview := strings.TrimSpace(prompt)
	if preview == "" {
		return nil
	}
	catalog, ok := runner.(agent.SessionCatalog)
	if !ok {
		return fmt.Errorf("runtime does not support session catalog")
	}
	now := time.Now()
	return catalog.UpdateSession(ctx, session.SessionInfoEntry{
		EntryBase:   session.EntryBase{ID: id, Timestamp: now},
		Workdir:     cwd,
		Model:       sessionModelName(cfg.Provider, cfg.Model),
		Branch:      branch,
		Name:        truncatePrintPreview(preview, 80),
		LastPreview: truncatePrintPreview(preview, 120),
		UpdatedAt:   now,
	})
}

func truncatePrintPreview(s string, max int) string {
	s = strings.Join(strings.Fields(s), " ")
	if len([]rune(s)) <= max {
		return s
	}
	r := []rune(s)
	return string(r[:max]) + "…"
}

func writePrintResult(w io.Writer, result printResult, output string) error {
	switch strings.ToLower(strings.TrimSpace(output)) {
	case "", "text":
		_, err := fmt.Fprintln(w, result.Response)
		return err
	case "json":
		enc := json.NewEncoder(w)
		return enc.Encode(result)
	default:
		return fmt.Errorf("unsupported print output %q (want text, json, or events)", output)
	}
}

func promptWithStdinContext(prompt, stdinText string) string {
	if prompt == "-" {
		return stdinText
	}
	if prompt == "" {
		return stdinText
	}
	if strings.TrimSpace(stdinText) == "" {
		return prompt
	}
	combined := prompt + "\n\n<stdin>\n" + stdinText
	if !strings.HasSuffix(combined, "\n") {
		combined += "\n"
	}
	combined += "</stdin>"
	return combined
}

// runPrintModeWithTimeout wraps runPrintMode with a configurable timeout.
func runPrintModeWithTimeout(
	ctx context.Context,
	w io.Writer,
	runner agent.Runtime,
	prompt string,
	timeout time.Duration,
	output string,
) error {
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()

	err := runPrintModeWithWriter(ctx, w, runner, prompt, output)
	if err != nil && errors.Is(err, context.DeadlineExceeded) {
		return ctxerr.Timeout("print mode", timeout, err)
	}
	return err
}

// isStdinPipe returns true if stdin is a pipe (not a terminal).
func isStdinPipe() bool {
	stat, err := os.Stdin.Stat()
	if err != nil {
		return false
	}
	return (stat.Mode() & os.ModeCharDevice) == 0
}

func runTopLevelCommand(args []string, stdout, stderr io.Writer) (bool, int) {
	if len(args) == 0 {
		return false, 0
	}
	if args[0] == "actions" {
		err := runActionCommand(args[1:], stdout)
		if err == nil {
			return true, 0
		}
		var actionErr *actionCommandError
		if errors.As(err, &actionErr) && actionErr.JSON {
			_ = json.NewEncoder(stdout).Encode(struct {
				Error string `json:"error"`
			}{Error: actionErr.Error()})
		} else {
			fmt.Fprintf(stderr, "%v\n", err)
		}
		return true, 1
	}
	if args[0] != "skill" {
		return false, 0
	}
	if err := runSkillCommand(args[1:], stdout); err != nil {
		fmt.Fprintf(stderr, "%v\n", err)
		return true, 1
	}
	return true, 0
}

func runSkillCommand(args []string, stdout io.Writer) error {
	if len(args) == 0 {
		return errors.New(skillCommandUsage())
	}
	switch args[0] {
	case "list", "ls":
		return runSkillList(args[1:], stdout)
	case "install":
		return runSkillInstall(args[1:], stdout)
	default:
		return errors.New(skillCommandUsage())
	}
}

func runSkillList(args []string, stdout io.Writer) error {
	dir, err := config.DefaultSkillsDir()
	if err != nil {
		return fmt.Errorf("resolve skills dir: %w", err)
	}
	out, err := ionskills.NoticeContext(context.Background(), []string{dir}, strings.Join(args, " "))
	if err != nil {
		return fmt.Errorf("load skills: %w", err)
	}
	_, err = fmt.Fprintln(stdout, out)
	return err
}

func runSkillInstall(args []string, stdout io.Writer) error {
	source, confirm, err := parseSkillInstallArgs(args)
	if err != nil {
		return err
	}
	dir, err := config.DefaultSkillsDir()
	if err != nil {
		return fmt.Errorf("resolve skills dir: %w", err)
	}
	if !confirm {
		preview, err := ionskills.PreviewInstall(source, dir)
		if err != nil {
			return err
		}
		printSkillInstallPreview(stdout, preview, false)
		return nil
	}
	installed, err := ionskills.Install(source, dir)
	if err != nil {
		return err
	}
	printSkillInstallPreview(stdout, installed, true)
	return nil
}

func parseSkillInstallArgs(args []string) (string, bool, error) {
	var source string
	var confirm bool
	for _, arg := range args {
		switch arg {
		case "--confirm", "-y":
			confirm = true
		default:
			if strings.HasPrefix(arg, "-") {
				return "", false, fmt.Errorf("unknown skill install flag: %s", arg)
			}
			if source != "" {
				return "", false, fmt.Errorf("usage: ion skill install [--confirm] <path>")
			}
			source = arg
		}
	}
	if source == "" {
		return "", false, fmt.Errorf("usage: ion skill install [--confirm] <path>")
	}
	return source, confirm, nil
}

func printSkillInstallPreview(out io.Writer, preview ionskills.InstallPreview, installed bool) {
	title := "Skill install preview"
	if installed {
		title = "Skill installed"
	}
	fmt.Fprintln(out, title)
	fmt.Fprintf(out, "name: %s\n", preview.Name)
	if preview.Description != "" {
		fmt.Fprintf(out, "description: %s\n", preview.Description)
	}
	if len(preview.AllowedTools) > 0 {
		fmt.Fprintf(out, "allowed tools: %s\n", strings.Join(preview.AllowedTools, ", "))
	}
	fmt.Fprintf(out, "source: %s\n", preview.Source)
	fmt.Fprintf(out, "target: %s\n", preview.Target)
	fmt.Fprintf(out, "files: %d\n", preview.Files)
	if !installed {
		fmt.Fprintf(out, "run: ion skill install --confirm %s\n", preview.Source)
	}
}

func skillCommandUsage() string {
	return strings.Join([]string{
		"usage:",
		"  ion skill list [query]",
		"  ion skill install [--confirm] <path>",
	}, "\n")
}
