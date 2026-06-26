package main

import (
	"context"
	"errors"
	"fmt"

	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/app"
	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/llm/providers"
	"github.com/nijaru/ion/internal/runtime"
	"github.com/nijaru/ion/session"
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
