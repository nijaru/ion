package tool

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"sync"

	"github.com/nijaru/ion/llm"
)

const (
	CompactToolName            = "compact"
	MaxCompactInstructionChars = 4000
)

// CompactRequest is the payload for model-directed compaction.
type CompactRequest struct {
	CustomInstructions string `json:"custom_instructions,omitempty"`
	ContinueAfter      bool   `json:"continue_after"`
}

// CompactRunner is implemented by the runtime controller.
type CompactRunner interface {
	CompactSession(ctx context.Context, customInstructions string) (string, error)
}

// CompactTool lets the model request context compaction at a natural task boundary.
type CompactTool struct {
	mu     sync.RWMutex
	runner CompactRunner
}

func NewCompactTool() *CompactTool {
	return &CompactTool{}
}

func (t *CompactTool) SetRunner(runner CompactRunner) {
	t.mu.Lock()
	t.runner = runner
	t.mu.Unlock()
}

func (t *CompactTool) Spec() llm.Spec {
	return llm.Spec{
		Name:        CompactToolName,
		Description: "Request context compaction at a natural task boundary. Summarizes earlier conversation history while preserving key context, decisions, and active task state.",
		Parameters: map[string]any{
			"type": "object",
			"properties": map[string]any{
				"custom_instructions": map[string]any{
					"type":        "string",
					"description": "Specific focus areas, architectural decisions, or active state to retain in the summary.",
				},
				"continue_after": map[string]any{
					"type":        "boolean",
					"description": "Whether to continue execution after compaction (default: true).",
				},
			},
		},
	}
}

func (t *CompactTool) Metadata() Metadata {
	return Metadata{
		Category:    "session",
		Concurrency: Serialized,
	}
}

func (t *CompactTool) Execute(ctx context.Context, args string) (string, error) {
	t.mu.RLock()
	runner := t.runner
	t.mu.RUnlock()
	if runner == nil {
		return "", fmt.Errorf("compact tool: runtime runner is unavailable")
	}

	var input struct {
		CustomInstructions string `json:"custom_instructions"`
		ContinueAfter      *bool  `json:"continue_after"`
	}
	if strings.TrimSpace(args) != "" && strings.TrimSpace(args) != "{}" {
		if err := json.Unmarshal([]byte(args), &input); err != nil {
			return "", fmt.Errorf("decode compact arguments: %w", err)
		}
	}
	if len([]rune(input.CustomInstructions)) > MaxCompactInstructionChars {
		return "", fmt.Errorf("custom_instructions exceeds %d characters", MaxCompactInstructionChars)
	}

	summary, err := runner.CompactSession(ctx, input.CustomInstructions)
	if err != nil {
		return "", fmt.Errorf("compaction failed: %w", err)
	}
	if summary == "" {
		return "Compacted conversation history into a new summary. Context consolidated.", nil
	}
	return fmt.Sprintf("Context compacted successfully. Summary created:\n%s", summary), nil
}

var (
	_ Tool         = (*CompactTool)(nil)
	_ MetadataTool = (*CompactTool)(nil)
)
