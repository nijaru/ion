package session

import (
	"context"
)

// AgentSession represents the canonical interface between the Go host and
// an underlying agent runtime (either native ion or external ACP).
type AgentSession interface {
	// Open initializes or creates a new session.
	Open(ctx context.Context) error

	// Resume loads an existing session.
	Resume(ctx context.Context, sessionID string) error

	// SubmitTurn sends a new user turn to the active session.
	SubmitTurn(ctx context.Context, turn string) error

	// CancelTurn interrupts an in-flight turn if the backend supports it.
	CancelTurn(ctx context.Context) error

	// Close terminates the session and cleans up resources.
	Close() error

	// Events returns a read-only channel of typed events emitted by the session.
	// The host UI consumes these events and translates them into UI commands.
	Events() <-chan Event
}
