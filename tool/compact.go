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
	CompactSession(ctx context.Context, instructions string, continueAfter bool) (string, error)
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
		Name: CompactToolName,
		Description: "Trigger context compaction and choose whether a follow-up turn should run afterward. " +
			"DEFER mid-task: If you have a clear next step in the current work — a file to write, a change to verify, a bug to finish fixing — do NOT compact. " +
			"A [ctx] hint is informational, not a trigger. Keep working. " +
			"Compact at genuine boundaries: the task is complete and verified, or you're switching to unrelated work. " +
			"If no [ctx] hint has fired, you have room; do not bother. " +
			"Set continueAfterCompaction=true when unfinished work remains or a final response is still needed; set it false only when no follow-up turn should occur. " +
			"Include instructions for what to preserve: current task, changed files, decisions, blockers, and next command. " +
			"After compacting, re-read active files before continuing.",
		Parameters: map[string]any{
			"type": "object",
			"properties": map[string]any{
				"instructions": map[string]any{
					"type":        "string",
					"description": "What to preserve in the summary (e.g., 'current task, changed files, decisions, blockers, next command').",
				},
				"continueAfterCompaction": map[string]any{
					"type":        "boolean",
					"description": "Set true when unfinished work remains or a final response is needed; false only when no follow-up turn should occur.",
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
		Instructions            string `json:"instructions"`
		CustomInstructions      string `json:"custom_instructions"`
		ContinueAfterCompaction *bool  `json:"continueAfterCompaction"`
		ContinueAfter           *bool  `json:"continue_after"`
	}
	if strings.TrimSpace(args) != "" && strings.TrimSpace(args) != "{}" {
		if err := json.Unmarshal([]byte(args), &input); err != nil {
			return "", fmt.Errorf("decode compact arguments: %w", err)
		}
	}

	instructions := strings.TrimSpace(input.Instructions)
	if instructions == "" {
		instructions = strings.TrimSpace(input.CustomInstructions)
	}
	if len([]rune(instructions)) > MaxCompactInstructionChars {
		return "", fmt.Errorf("instructions exceeds %d characters", MaxCompactInstructionChars)
	}

	continueAfter := true
	if input.ContinueAfterCompaction != nil {
		continueAfter = *input.ContinueAfterCompaction
	} else if input.ContinueAfter != nil {
		continueAfter = *input.ContinueAfter
	}

	summary, err := runner.CompactSession(ctx, instructions, continueAfter)
	if err != nil {
		return "", fmt.Errorf("compaction failed: %w", err)
	}
	if !continueAfter {
		if summary != "" {
			return fmt.Sprintf("Compaction scheduled; no follow-up turn will be started.\nSummary: %s", summary), nil
		}
		return "Compaction scheduled; no follow-up turn will be started.", nil
	}
	if summary != "" {
		return fmt.Sprintf(
			"Compaction scheduled; unfinished work will resume after it finishes.\nSummary: %s",
			summary,
		), nil
	}
	return "Compaction scheduled; unfinished work will resume after it finishes.", nil
}

var (
	_ Tool         = (*CompactTool)(nil)
	_ MetadataTool = (*CompactTool)(nil)
)
