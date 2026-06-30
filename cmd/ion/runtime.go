package main

import (
	"context"
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
	"github.com/nijaru/ion/internal/runtime"
	ionskills "github.com/nijaru/ion/internal/skills"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/llm/providers"
	"github.com/nijaru/ion/session"
	"github.com/nijaru/ion/tool"
)

func closeRuntimeHandles(
	agent session.Session,
	sess session.Session,
	store session.Store,
) error {
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

// loadPromptTemplates reads .md files from ~/.ion/prompts/ and returns a name→content map.
// Filenames without extension become the template name.
func loadPromptTemplates() map[string]string {
	home, err := os.UserHomeDir()
	if err != nil {
		return nil
	}
	dir := filepath.Join(home, ".ion", "prompts")
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil
	}
	templates := make(map[string]string)
	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), ".md") {
			continue
		}
		path := filepath.Join(dir, e.Name())
		data, err := os.ReadFile(path)
		if err != nil {
			continue
		}
		name := strings.TrimSuffix(e.Name(), ".md")
		templates[name] = string(data)
	}
	return templates
}

func recentSessionForContinue(
	ctx context.Context,
	store session.Store,
	cwd string,
) (*session.SessionInfoEntry, error) {
	sessions, err := store.ListSessions(ctx, cwd)
	if err != nil {
		return nil, err
	}
	for i := range sessions {
		if !runtime.IsConversationSessionInfo(&sessions[i]) {
			continue
		}
		return &sessions[i], nil
	}
	return nil, nil
}

func openStartupStore(noSession bool) (session.Store, error) {
	if noSession {
		return session.NewEphemeralCantoStore()
	}
	dataDir, err := config.DefaultDataDir()
	if err != nil {
		return nil, fmt.Errorf("resolve data dir: %w", err)
	}
	return session.NewCantoStore(dataDir)
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
	recent, err := recentSessionForContinue(ctx, store, cwd)
	if err != nil {
		return "", fmt.Errorf("failed to find recent session: %w", err)
	}
	if recent == nil {
		return "", fmt.Errorf("no conversation session to continue in this directory")
	}
	return recent.ID(), nil
}

func openRuntime(
	ctx context.Context,
	store session.Store,
	cwd, branch string,
	cfg *config.Config,
	sessionID string,
	persistResumedSessionModel bool,
) (app.Backend, session.Session, agent.Runner, error) {
	runtimeCfg := *cfg
	if err := resolveStartupConfig(&runtimeCfg); err != nil {
		return app.NewSetupBackend(&runtimeCfg, store, err.Error()), nil, nil, nil
	}

	b, err := backendForProvider(runtimeCfg.Provider)
	if err != nil {
		return nil, nil, nil, err
	}
	b.SetStore(store)
	b.SetConfig(&runtimeCfg)

	if sessionID != "" {
		if _, _, err := session.ResumeSession(ctx, store, sessionID); err != nil {
			return nil, nil, nil, fmt.Errorf("failed to resume session %s: %w", sessionID, err)
		}
	}

	modelName := sessionModelName(runtimeCfg.Provider, runtimeCfg.Model)
	if modelName == "" {
		return nil, nil, nil, fmt.Errorf(
			"provider and model must be set (e.g. provider=\"openrouter\" model=\"openai/gpt-5.4\")",
		)
	}

	// Create a Provider and Harness for turn execution.
	provider, err := providers.NewProviderFromConfig(&runtimeCfg)
	if err != nil {
		// Provider creation failed — return Backend only (no Runner).
		return b, nil, nil, nil
	}

	sess := session.NewSession(store, 64)
	model := llm.Model{ID: runtimeCfg.Model}

	// Register coding tools and convert to agent.Tool.
	toolRegistry := tool.NewRegistry()
	if err := tool.RegisterCodingTools(toolRegistry, tool.CodingToolsConfig{
		Workdir: cwd,
	}); err != nil {
		// Non-fatal: start without tools if registration fails.
		fmt.Fprintf(os.Stderr, "warning: failed to register tools: %v\n", err)
	}
	var agentTools []agent.Tool
	for _, entry := range toolRegistry.Entries() {
		entry := entry // capture for closure
		agentTools = append(agentTools, agent.Tool{
			Name:        entry.Spec.Name,
			Description: entry.Spec.Description,
			Parameters:  entry.Spec.Parameters,
			Execute: func(ctx context.Context, id string, args json.RawMessage, signal <-chan struct{}, progress func(session.ToolPartial)) (session.ToolResultMessage, error) {
				result, execErr := entry.Tool.Execute(ctx, string(args))
				if execErr != nil {
					return session.ToolResultMessage{
						ToolCallID: id,
						ToolName:   entry.Spec.Name,
						Content:    []session.Content{session.TextContent{Text: execErr.Error()}},
						IsError:    true,
					}, nil
				}
				return session.ToolResultMessage{
					ToolCallID: id,
					ToolName:   entry.Spec.Name,
					Content:    []session.Content{session.TextContent{Text: result}},
				}, nil
			},
			ExecutionMode: agent.ExecParallel,
		})
	}

	// Load skills text for system prompt.
	skillsText := ""
	if skillsDir, err := config.DefaultSkillsDir(); err == nil {
		if text, err := ionskills.FormatSkillsForPrompt(skillsDir); err == nil {
			skillsText = text
		}
	}

	// Load prompt templates from ~/.ion/prompts/.
	promptTemplates := loadPromptTemplates()

	log := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelInfo}))
	harness := agent.NewHarness(agent.HarnessConfig{
		Session:         sess,
		Store:           store,
		Model:           model,
		Tools:           agentTools,
		Events:          sess.EventSender(),
		StreamFn:        provider.Stream,
		SkillsText:      skillsText,
		PromptTemplates: promptTemplates,
		Logger:          log,
	})

	return b, sess, harness, nil
}

func closeRuntimeOpenError(
	label string,
	err error,
	agent session.Session,
	sess session.Session,
) error {
	if closeErr := closeRuntimeHandles(agent, sess, nil); closeErr != nil {
		err = errors.Join(err, fmt.Errorf("close runtime after failed open: %w", closeErr))
	}
	return fmt.Errorf("%s: %w", label, err)
}

type exportedSessionBundle struct {
	Bundle runtime.SessionBundle
	Path   string
}

func exportSessionBundleFile(
	ctx context.Context,
	store session.Store,
	sessionID string,
	path string,
) (exportedSessionBundle, error) {
	exporter, ok := store.(runtime.SessionBundleExporter)
	if !ok {
		return exportedSessionBundle{}, fmt.Errorf("session store does not support export")
	}
	path = strings.TrimSpace(path)
	if path == "" || path == "-" {
		return exportedSessionBundle{}, fmt.Errorf("export path must be a file")
	}
	bundle, err := exporter.ExportSessionBundle(ctx, sessionID)
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
	importer, ok := store.(runtime.SessionBundleImporter)
	if !ok {
		return nil, fmt.Errorf("session store does not support import")
	}
	path = strings.TrimSpace(path)
	if path == "" || path == "-" {
		return nil, fmt.Errorf("import path must be a file")
	}
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("read %s: %w", path, err)
	}
	var bundle runtime.SessionBundle
	if err := json.Unmarshal(raw, &bundle); err != nil {
		return nil, fmt.Errorf("decode session bundle %s: %w", path, err)
	}
	id, err := importer.ImportSessionBundle(ctx, bundle)
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
