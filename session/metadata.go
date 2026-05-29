package session

import (
	"context"

	"github.com/oklog/ulid/v2"
)

type contextKey int

const (
	metadataKey contextKey = iota
	turnIDKey
)

// WithMetadata attaches metadata to the context. This metadata will be
// automatically added to all events appended to a session using this context.
func WithMetadata(ctx context.Context, md map[string]any) context.Context {
	if len(md) == 0 {
		return ctx
	}
	existing, _ := ctx.Value(metadataKey).(map[string]any)
	if len(existing) == 0 {
		return context.WithValue(ctx, metadataKey, md)
	}

	merged := make(map[string]any, len(existing)+len(md))
	for k, v := range existing {
		merged[k] = v
	}
	for k, v := range md {
		merged[k] = v
	}
	return context.WithValue(ctx, metadataKey, merged)
}

// MetadataFromContext retrieves metadata from the context.
func MetadataFromContext(ctx context.Context) map[string]any {
	md, _ := ctx.Value(metadataKey).(map[string]any)
	return md
}

// WithTurnID attaches the current turn identity to the context. Session
// appends with this context inherit the turn ID unless the event already has
// one.
func WithTurnID(ctx context.Context, turnID string) context.Context {
	if turnID == "" {
		return ctx
	}
	return context.WithValue(ctx, turnIDKey, turnID)
}

// TurnIDFromContext retrieves the current turn identity from the context.
func TurnIDFromContext(ctx context.Context) string {
	turnID, _ := ctx.Value(turnIDKey).(string)
	return turnID
}

// EnsureTurnID returns a context carrying a turn identity and the identity
// itself. Existing turn IDs are preserved so nested framework calls stay in one
// transaction.
func EnsureTurnID(ctx context.Context) (context.Context, string) {
	if turnID := TurnIDFromContext(ctx); turnID != "" {
		return ctx, turnID
	}
	turnID := ulid.Make().String()
	return WithTurnID(ctx, turnID), turnID
}
