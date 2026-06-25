package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"

	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/app"
	"github.com/nijaru/ion/config"
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
		if !session.IsConversationSessionInfo(&sessions[i]) {
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
		b := app.NewUnconfigured(&runtimeCfg, err)
		b.SetStore(store)
		return b, nil, nil, nil
	}

	b, err := backendForProvider(runtimeCfg.Provider)
	if err != nil {
		return nil, nil, nil, err
	}
	b.SetStore(store)
	b.SetConfig(&runtimeCfg)

	if sessionID != "" {
		_, _, err := session.ResumeSession(ctx, store, sessionID)
		if err != nil {
			return nil, nil, nil, fmt.Errorf("failed to resume session %s: %w", sessionID, err)
		}
		return b, nil, nil, nil
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
	harness := agent.NewHarness(agent.HarnessConfig{
		Session:  sess,
		Store:    store,
		Model:    model,
		Events:   sess.EventSender(),
		StreamFn: provider.Stream,
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

func syncSessionMetadata(
	ctx context.Context,
	store session.Store,
	sessionID, modelName, branch string,
) error {
	if store == nil || sessionID == "" {
		return nil
	}
	return store.UpdateSession(ctx, session.SessionInfoEntry{
		// ID: sessionID,
		// Model: modelName,
		// Branch: branch,
		// PreserveUpdatedAt: true,
	})
}

// toolExecutorFromRegistry creates an agent.ToolExecutor that dispatches
// tool calls to an Ion tool.Registry.
func toolExecutorFromRegistry(registry *tool.Registry) agent.ToolExecutor {
	return func(ctx context.Context, tc agent.AgentToolCall) (agent.AgentToolResult, error) {
		t, ok := registry.Get(tc.Name)
		if !ok {
			return agent.AgentToolResult{
				Content: []llm.ContentPart{
					{Type: "text", Text: fmt.Sprintf("Unknown tool: %s", tc.Name)},
				},
				IsError: true,
			}, nil
		}

		argsJSON, err := json.Marshal(tc.Arguments)
		if err != nil {
			return agent.AgentToolResult{
				Content: []llm.ContentPart{
					{Type: "text", Text: fmt.Sprintf("Failed to marshal arguments: %v", err)},
				},
				IsError: true,
			}, nil
		}

		// Try ContentTool first for richer output (images, etc.)
		if ct, ok := t.(tool.ContentTool); ok {
			parts, execErr := ct.ExecuteContent(ctx, string(argsJSON))
			if execErr != nil {
				return agent.AgentToolResult{
					Content: []llm.ContentPart{{Type: "text", Text: execErr.Error()}},
					IsError: true,
				}, nil
			}
			ionParts := make([]llm.ContentPart, len(parts))
			for i, p := range parts {
				ionParts[i] = llm.ContentPart{Type: llm.ContentPartType(p.Type), Text: p.Text}
			}
			return agent.AgentToolResult{Content: ionParts}, nil
		}

		// Try DetailedTool for structured details
		if dt, ok := t.(tool.DetailedTool); ok {
			content, details, execErr := dt.ExecuteDetailed(ctx, string(argsJSON))
			if execErr != nil {
				return agent.AgentToolResult{
					Content: []llm.ContentPart{{Type: "text", Text: execErr.Error()}},
					IsError: true,
				}, nil
			}
			return agent.AgentToolResult{
				Content: []llm.ContentPart{{Type: "text", Text: content}},
				Details: details,
			}, nil
		}

		// Fall back to plain text Execute
		result, execErr := t.Execute(ctx, string(argsJSON))
		if execErr != nil {
			return agent.AgentToolResult{
				Content: []llm.ContentPart{{Type: "text", Text: execErr.Error()}},
				IsError: true,
			}, nil
		}
		return agent.AgentToolResult{
			Content: []llm.ContentPart{{Type: "text", Text: result}},
		}, nil
	}
}

// agentToolsFromRegistry returns agent.AgentTool definitions for all tools
// in the registry, suitable for LLM tool-spec requests.
func agentToolsFromRegistry(registry *tool.Registry) []agent.AgentTool {
	var result []agent.AgentTool
	for _, entry := range registry.Entries() {
		at := agent.AgentTool{
			Name:        entry.Spec.Name,
			Description: entry.Spec.Description,
			Parameters:  entry.Spec.Parameters,
		}
		if _, ok := entry.Tool.(tool.ContentTool); ok {
			at.ReadOnly = true
			at.ExecutionMode = agent.ToolExecutionParallel
		}
		result = append(result, at)
	}
	return result
}
