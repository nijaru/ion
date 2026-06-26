// Package runtime provides the integration layer between the agent harness
// and the TUI/CLI. It owns runtime state management, model switching,
// auto-compaction, and turn lifecycle.
//
// Pi equivalent: pi-coding-agent/core/agent-session.js (2,534 lines)
package runtime

import (
	"context"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/session"
)

// AgentSession bridges the harness and the TUI/CLI.
// It owns model management, session switching, auto-compaction, and retry.
//
// Pi equivalent: pi-coding-agent/core/agent-session.js
type AgentSession struct {
	// Config is the current configuration.
	Config *config.Config

	// Session is the current session tree.
	Session session.Session

	// Store is the persistence layer.
	Store session.Store
}

// NewAgentSession creates a new AgentSession.
func NewAgentSession(cfg *config.Config, sess session.Session, store session.Store) *AgentSession {
	return &AgentSession{
		Config:  cfg,
		Session: sess,
		Store:   store,
	}
}

// SubmitTurn submits a turn to the session.
// This is the business logic that was previously in session_controller.go.
func (a *AgentSession) SubmitTurn(ctx context.Context, text string) error {
	// TODO: Implement turn submission logic
	// This should create a UserMessage, append it to the session,
	// and start the agent loop.
	return nil
}

// CancelTurn cancels the current turn.
func (a *AgentSession) CancelTurn(ctx context.Context, reason string) error {
	// TODO: Implement turn cancellation logic
	return nil
}

// SwitchModel switches to a different model.
func (a *AgentSession) SwitchModel(ctx context.Context, model string) error {
	// TODO: Implement model switching logic
	return nil
}

// Compact triggers compaction on the session.
func (a *AgentSession) Compact(ctx context.Context) error {
	// TODO: Implement compaction logic
	return nil
}
