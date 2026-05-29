package main

import (
	"context"
	"errors"
	"fmt"

	"github.com/nijaru/canto/tool"
	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/internal/agenttools"
	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
	"github.com/nijaru/ion/internal/tools"
)

func closeRuntimeHandles(
	agent session.AgentSession,
	sess storage.Session,
	store storage.Store,
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
	store storage.Store,
	cwd string,
) (*storage.SessionInfo, error) {
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

func openStartupStore(noSession bool) (storage.Store, error) {
	if noSession {
		return storage.NewEphemeralCantoStore()
	}
	dataDir, err := config.DefaultDataDir()
	if err != nil {
		return nil, fmt.Errorf("resolve data dir: %w", err)
	}
	return storage.NewCantoStore(dataDir)
}

func startupSessionID(
	ctx context.Context,
	store storage.Store,
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
	return recent.ID, nil
}

func openRuntime(
	ctx context.Context,
	store storage.Store,
	cwd, branch string,
	cfg *config.Config,
	sessionID string,
	persistResumedSessionModel bool,
) (backend.Backend, storage.Session, error) {
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

	// Wire up coding tools for agent backends
	if ab, ok := b.(*agent.Backend); ok {
		registry := tool.NewRegistry()
		if regErr := tools.RegisterCodingTools(registry, tools.CodingToolsConfig{
			Workdir: cwd,
		}); regErr == nil {
			ab.SetToolExecutor(agenttools.ExecutorFromRegistry(registry))
			ab.SetTools(agenttools.ToolsFromRegistry(registry))
		}
	}

	if sessionID != "" {
		sess, err := store.ResumeSession(ctx, sessionID)
		if err != nil {
			return nil, nil, fmt.Errorf("failed to resume session %s: %w", sessionID, err)
		}
		b.SetSession(sess)
		if err := b.Session().Resume(ctx, sessionID); err != nil {
			return nil, nil, closeRuntimeOpenError("backend resume error", err, b.Session(), sess)
		}
		modelName := ""
		if persistResumedSessionModel {
			modelName = sessionModelName(runtimeCfg.Provider, runtimeCfg.Model)
		}
		if err := syncSessionMetadata(ctx, store, sessionID, modelName, branch); err != nil {
			return nil, nil, closeRuntimeOpenError(
				"failed to update resumed session metadata",
				err,
				b.Session(),
				sess,
			)
		}
		return b, sess, nil
	}

	modelName := sessionModelName(runtimeCfg.Provider, runtimeCfg.Model)
	if modelName == "" {
		return nil, nil, fmt.Errorf(
			"provider and model must be set (e.g. provider=\"openrouter\" model=\"openai/gpt-5.4\")",
		)
	}

	sess := storage.NewLazySession(store, cwd, modelName, branch)
	b.SetSession(sess)
	if err := b.Session().Open(ctx); err != nil {
		return nil, nil, closeRuntimeOpenError(
			"backend initialization error",
			err,
			b.Session(),
			sess,
		)
	}
	return b, sess, nil
}

func closeRuntimeOpenError(
	label string,
	err error,
	agent session.AgentSession,
	sess storage.Session,
) error {
	if closeErr := closeRuntimeHandles(agent, sess, nil); closeErr != nil {
		err = errors.Join(err, fmt.Errorf("close runtime after failed open: %w", closeErr))
	}
	return fmt.Errorf("%s: %w", label, err)
}

func syncSessionMetadata(
	ctx context.Context,
	store storage.Store,
	sessionID, modelName, branch string,
) error {
	if store == nil || sessionID == "" {
		return nil
	}
	return store.UpdateSession(ctx, storage.SessionInfo{
		ID:                sessionID,
		Model:             modelName,
		Branch:            branch,
		PreserveUpdatedAt: true,
	})
}
