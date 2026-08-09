package main

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"strings"

	"github.com/nijaru/ion/app"
	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/internal/agent"
	ionexport "github.com/nijaru/ion/internal/export"
	"github.com/nijaru/ion/internal/instructions"
	ionskills "github.com/nijaru/ion/internal/skills"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/llm/providers"
	ionmemory "github.com/nijaru/ion/memory"
	"github.com/nijaru/ion/session"
	"github.com/nijaru/ion/tool"
	ionmcp "github.com/nijaru/ion/tool/mcp"
)

func closeRuntimeHandles(
	runner agent.Runtime,
	store session.Store,
) error {
	var errs []error
	if runner != nil {
		errs = append(errs, runner.Close())
		if resources, ok := runner.(agent.ResourceOwner); ok {
			errs = append(errs, resources.CloseResources())
		}
	}
	if store != nil {
		errs = append(errs, store.Close())
	}
	return errors.Join(errs...)
}

func closeRuntimeResourcesAfterError(openErr error, closeResources func() error) error {
	if closeErr := closeResources(); closeErr != nil {
		return errors.Join(openErr, fmt.Errorf("close runtime resources after failed setup: %w", closeErr))
	}
	return openErr
}

// loadPromptTemplates reads trusted global and optional project-local .md prompt
// templates. Global templates retain precedence on name collisions.
func loadPromptTemplates(projectTrustRoot string) (map[string]string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return nil, fmt.Errorf("resolve home directory: %w", err)
	}
	dirs := []string{filepath.Join(home, ".ion", "prompts")}
	if projectTrustRoot != "" {
		dirs = append(dirs, filepath.Join(projectTrustRoot, ".ion", "prompts"))
	}
	return loadPromptTemplatesFromDirs(dirs)
}

func loadPromptTemplatesFromDirs(dirs []string) (map[string]string, error) {
	var templates map[string]string
	for _, dir := range dirs {
		entries, err := os.ReadDir(dir)
		if err != nil {
			if os.IsNotExist(err) {
				continue
			}
			return nil, fmt.Errorf("read prompt directory %q: %w", dir, err)
		}
		if templates == nil {
			templates = make(map[string]string)
		}
		for _, e := range entries {
			if e.IsDir() || !strings.HasSuffix(e.Name(), ".md") {
				continue
			}
			name := strings.TrimSuffix(e.Name(), ".md")
			if _, exists := templates[name]; exists {
				continue
			}
			path := filepath.Join(dir, e.Name())
			data, err := os.ReadFile(path)
			if err != nil {
				return nil, fmt.Errorf("read prompt template %q: %w", path, err)
			}
			templates[name] = string(data)
		}
	}
	return templates, nil
}

func recentSessionForContinue(
	ctx context.Context,
	catalog agent.SessionCatalog,
	cwd string,
) (*session.SessionInfoEntry, error) {
	sessions, err := catalog.ListSessions(ctx, cwd)
	if err != nil {
		return nil, err
	}
	for i := range sessions {
		if !app.IsConversationSessionInfo(&sessions[i]) {
			continue
		}
		return &sessions[i], nil
	}
	return nil, nil
}

func resolveSessionDir(override string) (string, error) {
	override = strings.TrimSpace(override)
	if override == "" {
		return config.DefaultDataDir()
	}
	if override == "~" || strings.HasPrefix(override, "~/") {
		home, err := os.UserHomeDir()
		if err != nil {
			return "", fmt.Errorf("resolve home directory: %w", err)
		}
		override = filepath.Join(home, strings.TrimPrefix(override, "~/"))
	}
	return filepath.Clean(override), nil
}

func openStartupStore(noSession bool, sessionDirOverride string) (*session.SQLiteStore, error) {
	if noSession {
		return session.NewSQLiteStore(":memory:", "ion")
	}
	dataDir, err := resolveSessionDir(sessionDirOverride)
	if err != nil {
		return nil, fmt.Errorf("resolve session directory: %w", err)
	}
	if err := os.MkdirAll(dataDir, 0o700); err != nil {
		return nil, fmt.Errorf("create session directory: %w", err)
	}
	return session.NewSQLiteStore(dataDir, "ion")
}

func startupSessionID(
	ctx context.Context,
	store session.Store,
	cwd string,
	sessionID string,
	resumeID string,
	resumeShortID string,
	continueRequested bool,
) (string, error) {
	if sessionID != "" {
		return sessionID, nil
	}
	if resumeID != "" {
		return resumeID, nil
	}
	if resumeShortID != "" {
		return resumeShortID, nil
	}
	if !continueRequested {
		return "", nil
	}
	catalog, ok := store.(agent.SessionCatalog)
	if !ok {
		return "", fmt.Errorf("session store does not support session catalog")
	}
	recent, err := recentSessionForContinue(ctx, catalog, cwd)
	if err != nil {
		return "", fmt.Errorf("failed to find recent session: %w", err)
	}
	if recent == nil {
		return "", fmt.Errorf("no conversation session to continue in this directory")
	}
	return recent.ID(), nil
}

// runtimeLocationForSession resolves the workspace captured with an explicitly
// selected session. A resumed session must not silently execute tools and
// project-local policy in the directory from which the launcher happened to
// start. Missing catalog metadata is tolerated for direct leaf IDs created
// before catalog publication; the selected session still resumes on cwd.
func runtimeLocationForSession(
	ctx context.Context,
	store session.Store,
	sessionID, cwd, branch string,
) (string, string, error) {
	if strings.TrimSpace(sessionID) == "" {
		return cwd, branch, nil
	}
	catalog, ok := store.(agent.SessionCatalog)
	if !ok {
		return cwd, branch, nil
	}
	info, err := catalog.GetSessionInfo(ctx, sessionID)
	if err != nil {
		if errors.Is(err, sql.ErrNoRows) || errors.Is(err, os.ErrNotExist) {
			return cwd, branch, nil
		}
		return "", "", fmt.Errorf("load session metadata %q: %w", sessionID, err)
	}
	resolvedCWD := cwd
	if storedCWD := strings.TrimSpace(info.Workdir); storedCWD != "" {
		resolvedCWD, err = filepath.Abs(storedCWD)
		if err != nil {
			return "", "", fmt.Errorf("resolve stored workdir %q: %w", storedCWD, err)
		}
		stat, statErr := os.Stat(resolvedCWD)
		if statErr != nil {
			return "", "", fmt.Errorf("stored workdir %q is unavailable: %w", resolvedCWD, statErr)
		}
		if !stat.IsDir() {
			return "", "", fmt.Errorf("stored workdir %q is not a directory", resolvedCWD)
		}
	}
	resolvedBranch := branch
	if storedBranch := strings.TrimSpace(info.Branch); storedBranch != "" {
		resolvedBranch = storedBranch
	}
	return resolvedCWD, resolvedBranch, nil
}

func thinkingLevelForRuntime(value string) session.ThinkingLevel {
	value = strings.ToLower(strings.TrimSpace(value))
	if value == "" {
		return ""
	}
	if value == config.DefaultReasoningEffort {
		return session.ThinkingAuto
	}
	return session.ThinkingLevel(value)
}

func defaultActiveToolNames(registry *tool.Registry) []string {
	return activeToolNamesForMode(registry, "coding")
}

func activeToolNamesForMode(registry *tool.Registry, mode string) []string {
	return activeToolNamesForModeWithSkills(registry, mode, false)
}

func activeToolNamesForModeWithSkills(
	registry *tool.Registry,
	mode string,
	skillsEnabled bool,
) []string {
	var preferred []string
	normalizedMode := config.NormalizeToolMode(mode)
	switch normalizedMode {
	case "all":
		return registry.Names()
	case "read":
		preferred = []string{"find", "grep", "ls", "read", tool.SearchToolName}
	default:
		preferred = []string{"bash", "edit", "read", tool.SearchToolName, "write"}
	}
	if skillsEnabled {
		preferred = append(preferred, "read_skill")
	}
	// Configured external tools are explicit user additions, so expose them in
	// every normal mode. The model can still discover the complete registry via
	// search_tools when a session's persisted ActiveTools narrows the surface.
	for _, entry := range registry.Entries() {
		switch entry.Metadata.Category {
		case "mcp":
			preferred = append(preferred, entry.Spec.Name)
		case "memory":
			if config.NormalizeToolMode(mode) != "read" || entry.Metadata.ReadOnly {
				preferred = append(preferred, entry.Spec.Name)
			}
		}
	}
	active := make([]string, 0, len(preferred))
	for _, name := range preferred {
		if _, ok := registry.Get(name); ok {
			active = append(active, name)
		}
	}
	return active
}

func skillDirsForRuntime(projectTrustRoot string) ([]string, error) {
	dir, err := config.DefaultSkillsDir()
	if err != nil {
		return nil, fmt.Errorf("resolve skills dir: %w", err)
	}
	dirs := []string{dir}
	if projectTrustRoot != "" {
		// Project skills take precedence over global skills with the same name.
		dirs = append(dirs, filepath.Join(projectTrustRoot, ".ion", "skills"))
	}
	return dirs, nil
}

func runtimeCodingToolsConfig(
	cfg *config.Config,
	cwd string,
	projectTrustRoot string,
	jobs *tool.JobManager,
) (tool.CodingToolsConfig, error) {
	if cfg == nil {
		return tool.CodingToolsConfig{}, errors.New("runtime config is nil")
	}

	var skillDirs []string
	if cfg.SkillToolMode() == "read" || cfg.ActiveToolMode() == "all" {
		var err error
		skillDirs, err = skillDirsForRuntime(projectTrustRoot)
		if err != nil {
			return tool.CodingToolsConfig{}, err
		}
	}

	return tool.CodingToolsConfig{
		Workdir:     cwd,
		Environment: tool.NewEnvironmentPolicy(cfg.ToolEnvMode(), llm.CredentialEnvVars(cfg)),
		SkillDirs:   skillDirs,
		Jobs:        jobs,
	}, nil
}

func openMCPRuntime(
	ctx context.Context,
	workdir string,
	configs []config.MCPServerConfig,
) (*ionmcp.Runtime, error) {
	servers := make([]ionmcp.ServerConfig, 0, len(configs))
	for _, server := range configs {
		servers = append(servers, ionmcp.ServerConfig{
			Name:           server.Name,
			Command:        server.Command,
			Args:           append([]string(nil), server.Args...),
			Directory:      server.Directory,
			Env:            cloneStringMap(server.Env),
			ProtectedPaths: append([]string(nil), server.ProtectedPaths...),
			ReadPaths:      append([]string(nil), server.ReadPaths...),
			WritablePaths:  append([]string(nil), server.WritablePaths...),
			AllowNetwork:   server.AllowNetwork,
		})
	}
	return ionmcp.Open(ctx, workdir, servers)
}

func cloneStringMap(values map[string]string) map[string]string {
	if len(values) == 0 {
		return nil
	}
	cloned := make(map[string]string, len(values))
	for key, value := range values {
		cloned[key] = value
	}
	return cloned
}

func resolvedContextWindow(cfg *config.Config, info app.RuntimeInfo, catalog *llm.ModelCatalog) int {
	if cfg != nil && cfg.ContextLimit > 0 {
		return cfg.ContextLimit
	}
	if catalog != nil && cfg != nil {
		if metadata, ok := catalog.GetCachedMetadata(cfg.Provider, cfg.Model); ok && metadata.ContextLimit > 0 {
			return metadata.ContextLimit
		}
	}
	if info != nil {
		return info.ContextLimit()
	}
	return 0
}

type sessionActivator interface {
	ActivateSession(context.Context, string, string) error
	ActivationOwner() uint64
	RestoreSessionIfOwner(context.Context, uint64, string, string) error
}

type sessionActivationCommitter interface {
	CommitSessionActivation()
}

func commitSessionActivation(runner agent.Runtime) {
	if committer, ok := runner.(sessionActivationCommitter); ok {
		committer.CommitSessionActivation()
	}
}

func resolveSessionActivation(
	ctx context.Context,
	store session.Store,
	selectedID string,
) (identity, leafID string, err error) {
	selectedID = strings.TrimSpace(selectedID)
	if selectedID == "" {
		return session.NewSessionID(), "", nil
	}
	identity = selectedID
	leafID = selectedID
	catalog, ok := store.(agent.SessionCatalog)
	if !ok {
		if selectedID == store.Meta().ID && store.GetLeafID() != "" {
			return selectedID, store.GetLeafID(), nil
		}
		if _, entryErr := store.GetEntry(ctx, selectedID); entryErr == nil {
			return session.NewSessionID(), selectedID, nil
		}
		return identity, leafID, nil
	}
	info, lookupErr := catalog.GetSessionInfo(ctx, selectedID)
	if lookupErr != nil {
		if errors.Is(lookupErr, sql.ErrNoRows) || errors.Is(lookupErr, os.ErrNotExist) {
			if selectedID == store.Meta().ID && store.GetLeafID() != "" {
				return selectedID, store.GetLeafID(), nil
			}
			if _, entryErr := store.GetEntry(ctx, selectedID); entryErr == nil {
				return session.NewSessionID(), selectedID, nil
			} else if !errors.Is(entryErr, sql.ErrNoRows) && !errors.Is(entryErr, os.ErrNotExist) {
				return "", "", fmt.Errorf("load selected leaf %q: %w", selectedID, entryErr)
			}
			return identity, leafID, nil
		}
		return "", "", fmt.Errorf("load selected session %q: %w", selectedID, lookupErr)
	}
	identity = info.ID()
	if selectedID == identity && info.LeafID != "" {
		leafID = info.LeafID
	}
	return identity, leafID, nil
}

func openRuntime(
	ctx context.Context,
	store session.Store,
	jobs *tool.JobManager,
	cwd, branch string,
	cfg *config.Config,
	endpointResolver *llm.EndpointResolver,
	sessionID string,
	persistResumedSessionModel bool,
	systemPromptOverride string,
	appendSystemPromptOverride string,
	projectTrustRoot string,
	catalog *llm.ModelCatalog,
	approvalInteractive ...bool,
) (app.RuntimeInfo, session.Session, agent.Runtime, error) {
	interactive := true
	if len(approvalInteractive) > 0 {
		interactive = approvalInteractive[0]
	}
	sess := session.NewSession(store, 64)
	runtimeCfg := *cfg
	if err := resolveStartupConfig(ctx, &runtimeCfg, endpointResolver); err != nil {
		return app.NewSetupRuntime(&runtimeCfg, err.Error()), nil, nil, nil
	}
	durableStore, ok := store.(session.DurableStore)
	if !ok {
		return app.NewSetupRuntime(&runtimeCfg, "session store does not support durable turns"), nil, nil,
			fmt.Errorf("session store does not support durable turns")
	}
	actionJournal, ok := store.(session.ActionJournal)
	if !ok {
		return app.NewSetupRuntime(&runtimeCfg, "session store does not support durable action journaling"), nil, nil,
			fmt.Errorf("session store does not support durable action journaling")
	}
	activator, ok := store.(sessionActivator)
	if !ok {
		return app.NewSetupRuntime(&runtimeCfg, "session store does not support atomic session activation"), nil, nil,
			fmt.Errorf("session store does not support atomic session activation")
	}
	identity, leafID, activationErr := resolveSessionActivation(ctx, store, sessionID)
	if activationErr != nil {
		return nil, nil, nil, activationErr
	}
	previousMeta := store.Meta()
	previousLeafID := store.GetLeafID()
	var activationOwner uint64
	activation := agent.NewActivationLease(func() error {
		if activationOwner == 0 {
			return nil
		}
		return activator.RestoreSessionIfOwner(
			context.Background(), activationOwner, previousMeta.ID, previousLeafID,
		)
	})
	info, err := runtimeInfoForProvider(runtimeCfg.Provider, &runtimeCfg)
	if err != nil {
		return nil, nil, nil, err
	}
	modelName := sessionModelName(runtimeCfg.Provider, runtimeCfg.Model)
	if modelName == "" {
		return nil, nil, nil, fmt.Errorf(
			"provider and model must be set (e.g. provider=\"openrouter\" model=\"openai/gpt-5.4\")",
		)
	}

	// Create a Provider and Controller for turn execution.
	provider, err := providers.NewProviderFromConfig(ctx, &runtimeCfg, endpointResolver)
	if err != nil {
		// Keep startup recoverable for the TUI, but never present an incomplete
		// runtime as accepted. Callers must handle the error before installing
		// or persisting this setup runtime.
		return app.NewSetupRuntime(&runtimeCfg, err.Error()), nil, nil,
			fmt.Errorf("initialize provider: %w", err)
	}
	provider = providerWithRetryPolicy(provider, &runtimeCfg)

	mcpRuntime, err := openMCPRuntime(ctx, cwd, runtimeCfg.MCPServers)
	if err != nil {
		return app.NewSetupRuntime(&runtimeCfg, err.Error()), nil, nil, err
	}
	var memoryStore *ionmemory.Store
	closeRuntimeResources := func() error {
		var closeErrs []error
		if memoryStore != nil {
			closeErrs = append(closeErrs, memoryStore.Close())
		}
		if mcpRuntime != nil {
			closeErrs = append(closeErrs, mcpRuntime.Close())
		}
		return errors.Join(closeErrs...)
	}
	cleanupOpenError := func(openErr error) error {
		return closeRuntimeResourcesAfterError(openErr, closeRuntimeResources)
	}
	setupFailure := func(openErr error) (app.RuntimeInfo, session.Session, agent.Runtime, error) {
		openErr = cleanupOpenError(openErr)
		return app.NewSetupRuntime(&runtimeCfg, openErr.Error()), nil, nil, openErr
	}
	if runtimeCfg.MemoryToolMode() == "on" {
		dataDir, err := config.DefaultDataDir()
		if err != nil {
			return setupFailure(err)
		}
		memoryStore, err = ionmemory.Open(filepath.Join(dataDir, "memory.db"))
		if err != nil {
			return setupFailure(err)
		}
	}

	// Provider identity belongs to the runtime's resolved provider, not to an
	// adapter response: several wire APIs omit that field in streaming chunks.
	// Persisting the resolved identity keeps assistant messages and replayed
	// context provider-neutral and attributable. Leave API empty until the
	// provider exposes its actual wire family; a provider slug is not an API
	// type.
	contextWindow := resolvedContextWindow(&runtimeCfg, info, catalog)
	caps := provider.Capabilities(runtimeCfg.Model)
	model := llm.Model{
		ID:            runtimeCfg.Model,
		Provider:      provider.ID(),
		ContextWindow: contextWindow,
		Reasoning:     caps.Reasoning.Kind != llm.ReasoningKindNone,
		Capabilities:  &caps,
	}

	// Register coding tools and convert to agent.Tool. Runtime policy belongs
	// here so the shell and optional skill surface cannot silently diverge from
	// the normalized config used to build the provider.
	toolRegistry := tool.NewRegistry()
	codingToolsConfig, err := runtimeCodingToolsConfig(&runtimeCfg, cwd, projectTrustRoot, jobs)
	if err != nil {
		return setupFailure(err)
	}
	if err := tool.RegisterCodingTools(toolRegistry, codingToolsConfig); err != nil {
		return setupFailure(fmt.Errorf("register coding tools: %w", err))
	}
	if memoryStore != nil {
		if err := tool.RegisterMemoryTools(toolRegistry, memoryStore, cwd); err != nil {
			return setupFailure(err)
		}
	}
	for _, external := range mcpRuntime.Tools() {
		if _, exists := toolRegistry.Get(external.Spec().Name); exists {
			err := fmt.Errorf("MCP tool name %q collides with an existing tool", external.Spec().Name)
			return setupFailure(err)
		}
		toolRegistry.Register(external)
	}
	var agentTools []agent.Tool
	for _, entry := range toolRegistry.Entries() {
		entry := entry // capture for closure
		executionMode := executionModeFor(entry.Metadata)
		var approvalRequirement func(json.RawMessage) (agent.ApprovalRequirement, bool, error)
		if provider, ok := entry.Tool.(tool.RequirementProvider); ok {
			approvalRequirement = func(args json.RawMessage) (agent.ApprovalRequirement, bool, error) {
				requirement, required, err := provider.ApprovalRequirement(string(args))
				if err != nil {
					return agent.ApprovalRequirement{}, false, err
				}
				return agent.ApprovalRequirement{
					Category:      requirement.Category,
					Operation:     requirement.Operation,
					Resource:      requirement.Resource,
					Paths:         append([]string(nil), requirement.Paths...),
					Environment:   append([]string(nil), requirement.Environment...),
					NetworkIntent: requirement.NetworkIntent,
					MCPIdentity:   requirement.MCPIdentity,
					Metadata:      requirement.Metadata,
					AlwaysConfirm: requirement.AlwaysConfirm,
				}, required, nil
			}
		}
		agentTools = append(agentTools, agent.Tool{
			Name:                entry.Spec.Name,
			Description:         entry.Spec.Description,
			Parameters:          entry.Spec.Parameters,
			ReadOnly:            entry.Metadata.ReadOnly,
			RequiresAction:      !entry.Metadata.ReadOnly,
			ApprovalRequirement: approvalRequirement,
			Execute: func(ctx context.Context, id string, args json.RawMessage, signal <-chan struct{}, progress func(session.ToolPartial)) (session.ToolResultMessage, error) {
				toolCtx, cancel := contextWithToolSignal(ctx, signal)
				defer cancel()
				return executeRegisteredTool(toolCtx, entry, id, args, progress), nil
			},
			ExecutionMode: executionMode,
		})
	}
	activeToolNames := activeToolNamesForModeWithSkills(
		toolRegistry,
		runtimeCfg.ActiveToolMode(),
		runtimeCfg.SkillToolMode() == "read",
	)
	if runtime, ok := info.(*runtimeInfo); ok {
		runtime.surface = app.ToolSurface{
			Count:       len(toolRegistry.Names()),
			Names:       toolRegistry.Names(),
			ActiveNames: append([]string(nil), activeToolNames...),
			Mode:        runtimeCfg.ActiveToolMode(),
		}
	}
	registeredSearch, _ := toolRegistry.Get(tool.SearchToolName)
	searchTool, _ := registeredSearch.(*tool.SearchTool)

	// Pi only advertises invocable skills when the read tool is available.
	skillsText := ""
	for _, registered := range agentTools {
		if registered.Name != "read" {
			continue
		}
		skillDirs, err := skillDirsForRuntime(projectTrustRoot)
		if err != nil {
			return setupFailure(fmt.Errorf("resolve skill directories: %w", err))
		}
		skillsText, err = ionskills.FormatSkillsForPromptContext(ctx, skillDirs...)
		if err != nil {
			return setupFailure(fmt.Errorf("load skills: %w", err))
		}
		break
	}

	// Load global and project-local prompt templates; global names win collisions.
	promptTemplates, err := loadPromptTemplates(projectTrustRoot)
	if err != nil {
		return setupFailure(fmt.Errorf("load prompt templates: %w", err))
	}

	log := runtimeLogger(interactive, os.Stderr)

	// Build the complete system prompt once at startup. The instructions package
	// owns base policy, project-context layering, resources, and runtime metadata;
	// the harness receives one immutable prompt snapshot for each runtime.
	sysPrompt, err := instructions.BuildSystemPrompt(
		systemPromptOverride,
		appendSystemPromptOverride,
		skillsText,
		cwd,
		projectTrustRoot,
	)
	if err != nil {
		return nil, nil, nil, cleanupOpenError(fmt.Errorf("build system prompt: %w", err))
	}

	harness := agent.NewController(agent.ControllerConfig{
		Session:             sess,
		Store:               store,
		Durable:             durableStore,
		RequireDurable:      true,
		Model:               model,
		Thinking:            thinkingLevelForRuntime(runtimeCfg.ReasoningEffort),
		Tools:               agentTools,
		Active:              activeToolNames,
		StreamFn:            provider.Stream,
		ContextOverflow:     provider.IsContextOverflow,
		PromptTemplates:     promptTemplates,
		SysPrompt:           sysPrompt,
		ApprovalMode:        agent.ApprovalMode(runtimeCfg.ToolTrustMode()),
		ApprovalInteractive: interactive,
		ActionJournal:       actionJournal,
		Workdir:             cwd,
		ProcessReconciler:   tool.NewProcessReconciler(),
		CloseResources:      []func() error{closeRuntimeResources},
		Activation:          activation,
		Logger:              log,
		Compaction:          agent.DefaultCompactionSettings(),
		SummaryRetry: llm.StreamRetryPolicy{
			Config:      retryPolicyForConfig(&runtimeCfg),
			IsTransient: provider.IsTransient,
		},
		ContextWindow: contextWindow,
	})
	closeUnusableRuntime := func(openErr error) error {
		closeErr := errors.Join(harness.Close(), closeRuntimeResources())
		if closeErr != nil {
			return errors.Join(openErr, fmt.Errorf("close runtime after failed open: %w", closeErr))
		}
		return openErr
	}
	var recovery agent.ActionRecovery = harness
	processRecovery, ok := any(harness).(agent.ProcessRecovery)
	if !ok {
		return nil, nil, nil, closeUnusableRuntime(
			errors.New("runtime does not support process-backed action recovery"),
		)
	}
	if err := processRecovery.RecoverProcessActions(ctx); err != nil {
		return nil, nil, nil, closeUnusableRuntime(fmt.Errorf("reconcile process-backed actions: %w", err))
	}
	unsettled, recoveryErr := recovery.UnsettledActions(ctx)
	if recoveryErr != nil {
		return nil, nil, nil, closeUnusableRuntime(fmt.Errorf("load unsettled actions: %w", recoveryErr))
	}
	if runtime, ok := info.(*runtimeInfo); ok {
		runtime.recovery = append([]session.ActionRecord(nil), unsettled...)
	}
	turnRecovery, ok := any(harness).(agent.TurnRecovery)
	if !ok {
		return nil, nil, nil, closeUnusableRuntime(errors.New("runtime does not support interrupted-turn recovery"))
	}
	interruptedTurns, interruptedErr := turnRecovery.InterruptedTurns(ctx)
	if interruptedErr != nil {
		return nil, nil, nil, closeUnusableRuntime(fmt.Errorf("load interrupted turns: %w", interruptedErr))
	}
	if runtime, ok := info.(*runtimeInfo); ok {
		runtime.interruptedTurns = append([]session.TurnRecord(nil), interruptedTurns...)
	}
	if len(unsettled) > 0 && !interactive {
		return nil, nil, nil, closeUnusableRuntime(fmt.Errorf(
			"%d unsettled external action(s) require interactive verification before print mode",
			len(unsettled),
		))
	}
	if len(interruptedTurns) > 0 && !interactive {
		return nil, nil, nil, closeUnusableRuntime(fmt.Errorf(
			"%d interrupted turn(s) require interactive recovery before print mode; use /turns",
			len(interruptedTurns),
		))
	}
	// Activate only after every fallible runtime-materialization and recovery
	// check has completed. The selection remains provisional until the host
	// accepts this runtime; Controller.Close restores the previous identity and
	// leaf if validation, persistence, or subscription rejects it. An
	// unqualified launch explicitly starts a fresh conversation at the virtual
	// root instead of inheriting the last selected leaf from the shared store.
	if err := activator.ActivateSession(ctx, identity, leafID); err != nil {
		return nil, nil, nil, closeUnusableRuntime(fmt.Errorf("activate session %s: %w", identity, err))
	}
	activationOwner = activator.ActivationOwner()
	if persistResumedSessionModel {
		if err := harness.SetModel(model); err != nil {
			return nil, nil, nil, closeUnusableRuntime(fmt.Errorf("persist runtime model: %w", err))
		}
	}
	if searchTool != nil {
		searchTool.SetActivator(harness.ActivateTools)
	}

	return info, sess, harness, nil
}

func runtimeLogger(interactive bool, stderr io.Writer) *slog.Logger {
	if interactive {
		// Bubble Tea owns the terminal in interactive mode. Internal lifecycle
		// logs must not interleave with its inline renderer; /debug is the
		// explicit user-facing diagnostic path.
		return slog.New(slog.NewTextHandler(io.Discard, &slog.HandlerOptions{Level: slog.LevelInfo}))
	}
	return slog.New(slog.NewTextHandler(stderr, &slog.HandlerOptions{Level: slog.LevelInfo}))
}

func closeRuntimeOpenError(
	label string,
	err error,
	runner agent.Runtime,
	store interface{ Close() error },
) error {
	var closeErrs []error
	if runner != nil {
		closeErrs = append(closeErrs, runner.Close())
		if resources, ok := runner.(agent.ResourceOwner); ok {
			closeErrs = append(closeErrs, resources.CloseResources())
		}
	}
	if store != nil {
		closeErrs = append(closeErrs, store.Close())
	}
	if closeErr := errors.Join(closeErrs...); closeErr != nil {
		err = errors.Join(err, fmt.Errorf("close runtime after failed open: %w", closeErr))
	}
	return fmt.Errorf("%s: %w", label, err)
}

type exportedSessionBundle struct {
	Bundle ionexport.SessionBundle
	Path   string
}

func exportSessionBundleFile(
	ctx context.Context,
	store session.Store,
	sessionID string,
	path string,
) (exportedSessionBundle, error) {
	path = strings.TrimSpace(path)
	if path == "" || path == "-" {
		return exportedSessionBundle{}, fmt.Errorf("export path must be a file")
	}
	bundle, err := ionexport.ExportSessionBundle(ctx, store, sessionID)
	if err != nil {
		return exportedSessionBundle{}, err
	}
	raw, err := json.MarshalIndent(bundle, "", "  ")
	if err != nil {
		return exportedSessionBundle{}, fmt.Errorf("marshal session bundle: %w", err)
	}
	raw = append(raw, '\n')
	if err := os.WriteFile(path, raw, 0o600); err != nil {
		return exportedSessionBundle{}, fmt.Errorf("write %s: %w", path, err)
	}
	return exportedSessionBundle{Bundle: bundle, Path: path}, nil
}

func importSessionBundleFile(
	ctx context.Context,
	store session.Store,
	path string,
) ([]session.SessionInfoEntry, error) {
	path = strings.TrimSpace(path)
	if path == "" || path == "-" {
		return nil, fmt.Errorf("import path must be a file")
	}
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("read %s: %w", path, err)
	}
	var bundle ionexport.SessionBundle
	if err := json.Unmarshal(raw, &bundle); err != nil {
		return nil, fmt.Errorf("decode Ion session bundle %s: %w", path, err)
	}
	bundle.RootSessionID = ""
	id, err := ionexport.ImportSessionBundle(ctx, store, bundle)
	if err != nil {
		return nil, err
	}
	return []session.SessionInfoEntry{{EntryBase: session.EntryBase{ID: id}}}, nil
}

func printSessionBundleExport(w io.Writer, exported exportedSessionBundle) {
	fmt.Fprintf(
		w,
		"Exported session %s to %s (%d sessions)\n",
		exported.Bundle.RootSessionID,
		exported.Path,
		len(exported.Bundle.Sessions),
	)
}

func printSessionBundleImport(w io.Writer, imported []session.SessionInfoEntry) {
	switch len(imported) {
	case 0:
		fmt.Fprintln(w, "Imported 0 sessions")
	case 1:
		fmt.Fprintf(w, "Imported session %s\n", imported[0].ID())
	default:
		fmt.Fprintf(w, "Imported %d sessions:\n", len(imported))
		for _, info := range imported {
			fmt.Fprintf(w, "- %s\n", info.ID())
		}
	}
}
