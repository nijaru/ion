package main

import (
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
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
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
	"github.com/nijaru/ion/tool"
)

// version is set at build time via -ldflags "-X main.version=vX.Y.Z".
var version = "v0.0.0"

func main() {
	if handled, code := runTopLevelCommand(os.Args[1:], os.Stdout, os.Stderr); handled {
		os.Exit(code)
	}

	timing.Record("cli-parse")

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
	for _, w := range config.Validate(cfg) {
		fmt.Fprintf(os.Stderr, "config warning: %s\n", w)
	}
	timing.Record("config-load")

	providerOverride := cli.providerOverride()
	modelOverride := cli.modelOverride()
	explicitRuntimeOverride := providerOverride != "" ||
		strings.TrimSpace(modelOverride) != "" ||
		strings.TrimSpace(os.Getenv("ION_PROVIDER")) != "" ||
		strings.TrimSpace(os.Getenv("ION_MODEL")) != ""
	applyCLIConfigOverrides(cfg, providerOverride, modelOverride, cli.thinkingOverride())
	cfg.APIKeyOverride = cli.apiKeyOverride()
	cfg.APIKeyOverrideProvider = llm.ResolveID(cfg.Provider)
	selectionRequested := cli.sessionID() != "" || cli.resumeID() != "" ||
		cli.resumeShortID() != "" || cli.continueRequested() || openResumePicker
	forkRequested := cli.forkRequested()
	if cfg.APIKeyOverride != "" && firstNonEmpty(cfg.Model, cfg.FastModel, cfg.SummaryModel) == "" &&
		!selectionRequested && cli.exportSessionPath() == "" && cli.importSessionPath() == "" &&
		!cli.listModelsRequested() {
		if err := validateAPIKeyOverride(cfg.APIKeyOverride, ""); err != nil {
			fmt.Fprintf(os.Stderr, "%v\n", err)
			os.Exit(2)
		}
	}

	ctx := context.Background()
	cwd, _ := os.Getwd()
	branch := currentBranch()

	listModelsSearch, err := resolveListModelsSearch(cli.listModelsRequested(), flag.Args())
	if err != nil {
		fmt.Fprintf(os.Stderr, "%v\n", err)
		os.Exit(2)
	}
	printArgs := flag.Args()
	if cli.listModelsRequested() {
		printArgs = nil
		if cli.printRequested() || cli.printShortRequested() || cli.prompt() != "" || cli.jsonRequested() {
			fmt.Fprintln(os.Stderr, "--list-models cannot be combined with print-mode flags")
			os.Exit(2)
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
		fmt.Fprintf(os.Stderr, "%v\n", err)
		os.Exit(2)
	}
	if err := validatePrintSelection(printRequested, openResumePicker); err != nil {
		fmt.Fprintf(os.Stderr, "%v\n", err)
		os.Exit(2)
	}
	if err := validateSessionBundleSelection(cli.exportSessionPath(), cli.importSessionPath()); err != nil {
		fmt.Fprintf(os.Stderr, "%v\n", err)
		os.Exit(2)
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
		fmt.Fprintf(os.Stderr, "%v\n", err)
		os.Exit(2)
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

	if cli.listModelsRequested() {
		if err := runListModels(ctx, os.Stdout, os.Stderr, cfg, listModelsSearch); err != nil {
			fmt.Fprintf(os.Stderr, "--list-models: %v\n", err)
			os.Exit(1)
		}
		return
	}

	store, err := openStartupStore(cli.noSessionRequested(), cli.sessionDirOverride())
	if err != nil {
		fmt.Fprintf(os.Stderr, "failed to initialize storage: %v\n", err)
		os.Exit(1)
	}
	timing.Record("store-open")

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

	var sessionID string
	if openResumePicker {
		width, height, err := term.GetSize(os.Stdout.Fd())
		if err != nil || width <= 0 {
			width = 80
			height = 24
		}
		pickerModel := app.New(nil, nil, store, cwd, branch, version, nil).
			WithConfig(cfg).
			WithSize(width, height).
			WithSessionPreStartupMode()

		p := tea.NewProgram(&pickerModel)
		finalPickerModel, pickerErr := p.Run()
		if pickerErr != nil {
			fmt.Fprintf(os.Stderr, "session picker error: %v\n", pickerErr)
			os.Exit(1)
		}

		appPickerModel, ok := finalPickerModel.(*app.Model)
		if !ok || appPickerModel == nil {
			os.Exit(0)
		}

		selectedSessionID := appPickerModel.SelectedSessionID()
		if selectedSessionID == "" {
			os.Exit(0)
		}

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
			fmt.Fprintf(os.Stderr, "%v\n", err)
			os.Exit(1)
		}
	}
	if cli.exportSessionPath() != "" {
		if sessionID == "" {
			fmt.Fprintln(
				os.Stderr,
				"--export-session requires --session <id>, --resume <id>, or --continue",
			)
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
	if forkRequested {
		forkedID, err := ionexport.ForkSession(ctx, store, sessionID)
		if err != nil {
			store.Close()
			fmt.Fprintf(os.Stderr, "failed to fork session %s: %v\n", sessionID, err)
			os.Exit(1)
		}
		sessionID = forkedID
	}
	if sessionID != "" && !explicitRuntimeOverride {
		if err := applySessionConfigFromMetadata(ctx, store, cwd, sessionID, cfg); err != nil {
			fmt.Fprintf(os.Stderr, "%v\n", err)
			os.Exit(1)
		}
	}
	cfg.APIKeyOverrideProvider = llm.ResolveID(cfg.Provider)
	if err := validateAPIKeyOverride(cfg.APIKeyOverride, firstNonEmpty(cfg.Model, cfg.FastModel, cfg.SummaryModel)); err != nil {
		fmt.Fprintf(os.Stderr, "%v\n", err)
		os.Exit(2)
	}
	jobs := tool.NewJobManager()
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

	persistResumedSessionModel := !(sessionID != "" && explicitRuntimeOverride)
	b, sess, runner, err := openRuntime(
		ctx,
		store,
		jobs,
		cwd,
		branch,
		runtimeCfg,
		sessionID,
		persistResumedSessionModel,
		cli.systemPromptOverride(),
		cli.appendSystemPromptOverride(),
	)
	if err != nil {
		fmt.Fprintf(os.Stderr, "failed to initialize runtime: %v\n", err)
		os.Exit(1)
	}
	// Print mode: run a single turn and exit
	if printRequested {
		if runner == nil {
			fmt.Fprintf(os.Stderr, "print mode requires a configured provider and model\n")
			os.Exit(1)
		}
		runErr := runPrintModeWithTimeout(
			ctx,
			os.Stdout,
			runner,
			prompt,
			cli.timeout(),
			output,
		)
		if runErr == nil {
			runErr = updatePrintSessionInfo(ctx, store, runner, cwd, branch, runtimeCfg, prompt)
		}
		closeErr := closeRuntimeHandles(runner, nil, store)
		closeErr = errors.Join(closeErr, jobs.Close())
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

	startupLines := startupBannerLines(version)
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
			fmt.Fprintf(os.Stderr, "failed to load startup history: %v\n", err)
		} else {
			startupEntries = entries
		}
	}
	switcher := func(ctx context.Context, cfg *config.Config, sessionID string) (app.Backend, agent.Runner, session.Session, error) {
		switchedBackend, switchedSession, switchedRunner, err := openRuntime(
			ctx,
			store,
			jobs,
			cwd,
			currentBranch(),
			cfg,
			sessionID,
			true,
			cli.systemPromptOverride(),
			cli.appendSystemPromptOverride(),
		)
		if err != nil {
			return nil, nil, nil, err
		}
		return switchedBackend, switchedRunner, switchedSession, nil
	}

	width, height, err := term.GetSize(os.Stdout.Fd())
	if err != nil || width <= 0 {
		width = 80
		height = 24
	}

	model := app.New(b, sess, store, cwd, branch, version, switcher).
		WithRunner(runner).
		WithJobs(tuiJobController{manager: jobs}).
		WithConfigForRuntimePreset(cfg, runtimeCfg, activePreset).
		WithSize(width, height)
	if openResumePicker {
		model = model.WithSessionPicker()
	} else if startupProviderMissing(b) {
		model = model.WithProviderPicker()
	} else if startupModelMissing(b) {
		model = model.WithModelPicker()
	}
	// Skip startup banner when opening the resume picker — the resume flow
	// will print its own header after the user selects a session.
	if !openResumePicker {
		printStartup(
			os.Stdout,
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
	agentToClose := runtimeHandlesForClose(finalModel, runner)
	closeErr := closeRuntimeHandles(agentToClose, nil, store)
	closeErr = errors.Join(closeErr, jobs.Close())
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
	if sessionID := resumeHintSessionID(finalModel); sessionID != "" && !cli.noSessionRequested() {
		printResumeHint(os.Stdout, sessionID)
	}
}

func startupProviderMissing(b app.Backend) bool {
	return b != nil && strings.TrimSpace(b.Provider()) == ""
}

func startupModelMissing(b app.Backend) bool {
	return b != nil &&
		strings.TrimSpace(b.Provider()) != "" &&
		strings.TrimSpace(b.Model()) == ""
}

func runtimeHandlesForClose(
	finalModel tea.Model,
	fallbackRunner agent.Runner,
) agent.Runner {
	if model, ok := finalModel.(*app.Model); ok && model != nil {
		return model.Model.Runner
	}
	return fallbackRunner
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
	if output != "text" && output != "json" {
		return false, "", "", fmt.Errorf("unsupported print output %q (want text or json)", output)
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

func runPrintModeWithWriter(
	ctx context.Context,
	w io.Writer,
	runner agent.Runner,
	prompt string,
	output string,
) error {
	result, err := runPromptTurn(ctx, runner, prompt)
	if err != nil {
		return err
	}
	return writePrintResult(w, result, output)
}

func runPromptTurn(
	ctx context.Context,
	runner agent.Runner,
	prompt string,
) (printResult, error) {
	events := runner.Events()
	if events == nil {
		events = make(chan session.Event) // dummy, never receives
	}

	var agentText strings.Builder
	result := printResult{}
	if sess := runner.Session(); sess != nil {
		result.SessionID = sess.ID()
	}

	// Start the agent turn in a goroutine.
	type promptOutcome struct {
		msg session.Message
		err error
	}
	outcomeCh := make(chan promptOutcome, 1)
	go func() {
		msg, err := runner.Prompt(ctx, prompt)
		outcomeCh <- promptOutcome{msg, err}
	}()

	// Wait for BOTH Prompt to return AND TurnEnd to be seen.
	var (
		promptDone   bool
		promptMsg    session.Message
		turnFinished bool
	)

	for !promptDone || !turnFinished {
		select {
		case ev, ok := <-events:
			if !ok {
				// Channel closed before turn finished — wait for Prompt, then error.
				if !promptDone {
					<-outcomeCh
					promptDone = true
				}
				_, _, _ = runner.Abort()
				return printResult{},
					fmt.Errorf("event stream closed before turn finished")
			}
			switch msg := ev.(type) {
			case session.ToolExecStart:
				result.ToolCalls = append(result.ToolCalls, msg.Name)
			case session.MessageUpdate:
				if msg.BlockType == "text" {
					agentText.WriteString(session.DeltaText(msg.Delta))
				}
			case session.MessageEnd:
				if session.MessageText(msg.Message) != "" {
					agentText.Reset()
					agentText.WriteString(session.MessageText(msg.Message))
				}
				if am, ok := msg.Message.(*session.AssistantMessage); ok {
					result.InputTokens += am.Usage.Input
					result.OutputTokens += am.Usage.Output
					result.Cost += am.Usage.Cost.Total
				}
			case session.TurnEnd:
				if msg.Error != nil {
					_, _, _ = runner.Abort()
					return printResult{}, fmt.Errorf("session error: %w", msg.Error)
				}
				turnFinished = true
			}
		case outcome := <-outcomeCh:
			promptMsg = outcome.msg
			promptDone = true
			if outcome.err != nil {
				return printResult{}, fmt.Errorf("submit turn: %w", outcome.err)
			}
		case <-ctx.Done():
			_, _, _ = runner.Abort()
			return printResult{}, ctxerr.WrapContext("print turn", ctx.Err())
		}
	}

	result.Response = agentText.String()
	if strings.TrimSpace(result.Response) == "" {
		if promptMsg != nil {
			result.Response = session.MessageText(promptMsg)
		}
	}
	if strings.TrimSpace(result.Response) == "" {
		return printResult{}, fmt.Errorf("turn finished without assistant response")
	}
	if sess := runner.Session(); sess != nil {
		result.SessionID = sess.ID()
	}
	return result, nil
}

func updatePrintSessionInfo(
	ctx context.Context,
	store session.Store,
	runner agent.Runner,
	cwd string,
	branch string,
	cfg *config.Config,
	prompt string,
) error {
	if store == nil || runner == nil || runner.Session() == nil || cfg == nil {
		return nil
	}
	id := runner.Session().ID()
	if id == "" || id == "canto" {
		return nil
	}
	preview := strings.TrimSpace(prompt)
	if preview == "" {
		return nil
	}
	catalog, ok := store.(sessionCatalogWriter)
	if !ok {
		return fmt.Errorf("session store does not support session catalog")
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
		return fmt.Errorf("unsupported print output %q (want text or json)", output)
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
	runner agent.Runner,
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
	if len(args) == 0 || args[0] != "skill" {
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
	out, err := ionskills.Notice([]string{dir}, strings.Join(args, " "))
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
