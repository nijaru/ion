package main

import (
	"context"
	"flag"
	"fmt"
	"io"
	"os"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/app"
	"github.com/nijaru/ion/internal/backend"
	ionacp "github.com/nijaru/ion/internal/backend/acp"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

// version is set at build time via -ldflags "-X main.version=vX.Y.Z".
var version = "v0.0.0"

func main() {
	if handled, code := runTopLevelCommand(os.Args[1:], os.Stdout, os.Stderr); handled {
		os.Exit(code)
	}

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

	ctx := context.Background()
	cwd, _ := os.Getwd()
	branch := currentBranch()
	acpCommandOverride := strings.TrimSpace(os.Getenv("ION_ACP_COMMAND"))

	store, err := openStartupStore(cli.noSessionRequested())
	if err != nil {
		fmt.Fprintf(os.Stderr, "failed to initialize storage: %v\n", err)
		os.Exit(1)
	}
	if cli.agentRequested() {
		runErr := runACPAgent(
			ctx,
			os.Stdin,
			os.Stdout,
			store,
			cfg,
			branch,
			ionacp.ModeYolo,
			acpCommandOverride,
		)
		closeErr := store.Close()
		if runErr != nil {
			if closeErr != nil {
				fmt.Fprintf(os.Stderr, "failed to close storage: %v\n", closeErr)
			}
			fmt.Fprintf(os.Stderr, "ACP agent error: %v\n", runErr)
			os.Exit(1)
		}
		if closeErr != nil {
			fmt.Fprintf(os.Stderr, "failed to close storage: %v\n", closeErr)
			os.Exit(1)
		}
		return
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
		cli.sessionID(),
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

	persistResumedSessionModel := !(sessionID != "" && explicitRuntimeOverride)
	b, sess, err := openRuntime(
		ctx,
		store,
		cwd,
		branch,
		runtimeCfg,
		acpCommandOverride,
		sessionID,
		persistResumedSessionModel,
	)
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
		runErr := runPrintModeWithTimeout(
			ctx,
			os.Stdout,
			agent,
			prompt,
			cli.timeout(),
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
	switcher := func(ctx context.Context, cfg *config.Config, sessionID string) (backend.Backend, session.AgentSession, storage.Session, error) {
		switchedBackend, switchedSession, err := openRuntime(
			ctx,
			store,
			cwd,
			currentBranch(),
			cfg,
			acpCommandOverride,
			sessionID,
			true,
		)
		if err != nil {
			return nil, nil, nil, err
		}
		return switchedBackend, switchedBackend.Session(), switchedSession, nil
	}

	model := app.New(b, sess, store, cwd, branch, version, switcher).
		WithConfigForRuntime(cfg, runtimeCfg).
		WithActivePreset(activePreset)
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
	finalModel, runErr := p.Run()
	agentToClose, sessionToClose := runtimeHandlesForClose(finalModel, b.Session(), sess)
	closeErr := closeRuntimeHandles(agentToClose, sessionToClose, store)
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

func runtimeHandlesForClose(
	finalModel tea.Model,
	fallbackAgent session.AgentSession,
	fallbackSession storage.Session,
) (session.AgentSession, storage.Session) {
	if model, ok := finalModel.(app.Model); ok {
		return model.Model.Session, model.Model.Storage
	}
	return fallbackAgent, fallbackSession
}
