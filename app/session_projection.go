package app

import (
	"context"
	"fmt"

	"github.com/nijaru/ion/internal/agent"
	"github.com/nijaru/ion/session"
)

// loadSessionProjection is the app's single semantic read boundary for the
// active session. A live runtime owns the projection, including staged durable
// entries. Storage is used only before a runtime exists, when the host is
// bootstrapping or loading a session for the first time.
func loadSessionProjection(
	ctx context.Context,
	runner agent.Runtime,
	storage RuntimeStorage,
) (agent.SessionProjection, error) {
	if reader, ok := runner.(agent.SessionProjectionReader); ok {
		projection, err := reader.SessionProjection(ctx)
		if err != nil {
			return agent.SessionProjection{}, fmt.Errorf("read active session projection: %w", err)
		}
		return projection, nil
	}
	if runner != nil {
		return agent.SessionProjection{}, fmt.Errorf("active runtime does not support session projection")
	}
	if storage == nil {
		return agent.SessionProjection{}, nil
	}

	entries, err := storage.Entries(ctx)
	if err != nil {
		return agent.SessionProjection{}, fmt.Errorf("load bootstrap session entries: %w", err)
	}
	usage, err := storage.Usage(ctx)
	if err != nil {
		return agent.SessionProjection{}, fmt.Errorf("load bootstrap session usage: %w", err)
	}
	projection := agent.SessionProjection{
		ID:             storage.ID(),
		Branch:         append([]session.Entry(nil), entries...),
		Usage:          usage,
		WorktreeBranch: storage.Meta().Branch,
	}
	if len(projection.Branch) > 0 {
		projection.LeafID = projection.Branch[len(projection.Branch)-1].ID()
	}
	return projection, nil
}
