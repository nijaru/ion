package main

import (
	"context"
	"errors"
	"fmt"
	"os"
	"strings"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
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

func openRuntime(
	ctx context.Context,
	store storage.Store,
	cwd, branch string,
	cfg *config.Config,
	acpCommandOverride string,
	sessionID string,
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
		return nil, nil, fmt.Errorf(
			"provider and model must be set (e.g. provider=\"openrouter\" model=\"openai/gpt-5.4\")",
		)
	}

	sess := storage.NewLazySession(store, cwd, modelName, branch)
	b.SetSession(sess)
	if err := b.Session().Open(ctx); err != nil {
		_ = sess.Close()
		return nil, nil, fmt.Errorf("backend initialization error: %w", err)
	}
	return b, sess, nil
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
		ID:     sessionID,
		Model:  modelName,
		Branch: branch,
	})
}
