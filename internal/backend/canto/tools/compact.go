package tools

import (
	"context"
	"encoding/json"
	"fmt"
	"path/filepath"

	ccontext "github.com/nijaru/canto/context"
	"github.com/nijaru/canto/llm"
	"github.com/nijaru/canto/session"
	"github.com/nijaru/ion/internal/config"
)

type Compact struct {
	Store    session.Store
	Provider llm.Provider
	Model    func() string
	Limit    func() int
	Session  func() string
}

func NewCompact(store session.Store, provider llm.Provider, model func() string, limit func() int, sessionID func() string) *Compact {
	return &Compact{
		Store:    store,
		Provider: provider,
		Model:    model,
		Limit:    limit,
		Session:  sessionID,
	}
}

func (c *Compact) Spec() llm.Spec {
	return llm.Spec{
		Name:        "compact",
		Description: "Compact the conversation context by summarizing older messages. Use when switching topics, after completing a task, or when earlier context is no longer needed. You can provide a message to guide what the summary should preserve.",
		Parameters: map[string]any{
			"type": "object",
			"properties": map[string]any{
				"message": map[string]any{
					"type":        "string",
					"description": "Optional guidance for the summarizer on what to preserve or emphasize in the summary.",
				},
			},
		},
	}
}

func (c *Compact) Execute(ctx context.Context, args string) (string, error) {
	var input struct {
		Message string `json:"message"`
	}
	if args != "" {
		if err := json.Unmarshal([]byte(args), &input); err != nil {
			return "", fmt.Errorf("compact: invalid args: %w", err)
		}
	}

	model := c.Model()
	sessionID := c.Session()
	maxTokens := c.Limit()

	if model == "" {
		return "", fmt.Errorf("compact: model not configured")
	}
	if sessionID == "" {
		return "", fmt.Errorf("compact: session not initialized")
	}
	if maxTokens <= 0 {
		return "", fmt.Errorf("compact: context limit unavailable")
	}

	sess, err := c.Store.Load(ctx, sessionID)
	if err != nil {
		return "", fmt.Errorf("compact: load session: %w", err)
	}

	dataDir, err := config.DefaultDataDir()
	if err != nil {
		return "", fmt.Errorf("compact: data dir: %w", err)
	}

	result, err := ccontext.CompactSession(ctx, c.Provider, model, sess, ccontext.CompactOptions{
		MaxTokens:  maxTokens,
		OffloadDir: filepath.Join(dataDir, "artifacts"),
		Message:    input.Message,
	})
	if err != nil {
		return "", fmt.Errorf("compact: %w", err)
	}

	if !result.Compacted {
		return "No compaction needed — context is within limits.", nil
	}

	return fmt.Sprintf("Context compacted successfully.%s", compactSuffix(input.Message)), nil
}

func compactSuffix(msg string) string {
	if msg == "" {
		return ""
	}
	return fmt.Sprintf(" Guidance: %s", msg)
}
