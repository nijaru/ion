// Package agent implements context compaction for the harness.
//
// Ported from Pi's pi-agent-core/dist/harness/compaction/compaction.js (527 lines).
// The algorithm:
//  1. Estimate context tokens from session messages
//  2. If over threshold, find a cut point that keeps recent tokens
//  3. Call LLM to summarize the messages before the cut
//  4. Append a CompactionEntry to the session
//
// Key invariants (from Pi):
//   - Compaction creates a CompactionEntry with summary + first kept entry ID
//   - buildContext skips pre-compaction entries (already in summary)
//   - The summary includes file operations (read/modified files)
package agent

import (
	"context"
	"fmt"
	"strings"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// CompactionSettings controls when and how compaction runs.
type CompactionSettings struct {
	Enabled          bool
	ReserveTokens   int // Tokens reserved for output (default: 16384)
	KeepRecentTokens int // Tokens to keep from recent messages (default: 20000)
}

// DefaultCompactionSettings returns sensible defaults.
func DefaultCompactionSettings() CompactionSettings {
	return CompactionSettings{
		Enabled:          true,
		ReserveTokens:   16384,
		KeepRecentTokens: 20000,
	}
}

// ContextTokenEstimate holds token estimation results.
type ContextTokenEstimate struct {
	Tokens         int
	TrailingTokens int
}

// EstimateTokens estimates the token count for a message.
// Uses a conservative character heuristic (1 token ≈ 4 chars).
//
// Reference: Pi compaction.js estimateTokens (line 140)
func EstimateTokens(msg session.Message) int {
	var chars int
	switch m := msg.(type) {
	case *session.UserMessage:
		for _, c := range m.Content {
			chars += contentChars(c)
		}
	case *session.AssistantMessage:
		for _, c := range m.Content {
			chars += contentChars(c)
		}
	case *session.ToolResultMessage:
		for _, c := range m.Content {
			chars += contentChars(c)
		}
	}
	if chars == 0 {
		return 0
	}
	return (chars + 3) / 4 // ceil(chars / 4)
}

func contentChars(c session.Content) int {
	switch v := c.(type) {
	case session.TextContent:
		return len(v.Text)
	case session.ThinkingContent:
		return len(v.Text)
	case *session.ToolCall:
		return len(v.Name) + len(fmt.Sprintf("%v", v.Arguments))
	case session.ImageContent:
		return 4800 // estimated image chars
	default:
		return 0
	}
}

// EstimateContextTokens estimates total context tokens from session messages.
//
// Reference: Pi compaction.js estimateContextTokens (line 95)
func EstimateContextTokens(messages []session.Message) ContextTokenEstimate {
	total := 0
	for _, msg := range messages {
		total += EstimateTokens(msg)
	}
	return ContextTokenEstimate{
		Tokens:         total,
		TrailingTokens: total,
	}
}

// ShouldCompact returns whether context usage exceeds the configured threshold.
//
// Reference: Pi compaction.js shouldCompact (line 130)
func ShouldCompact(contextTokens, contextWindow int, settings CompactionSettings) bool {
	if !settings.Enabled {
		return false
	}
	return contextTokens > contextWindow-settings.ReserveTokens
}

// CutPoint represents where to cut the conversation for compaction.
type CutPoint struct {
	FirstKeptEntryIndex int
	TurnStartIndex      int
	IsSplitTurn         bool
}

// FindCutPoint finds the cut point that keeps approximately keepRecentTokens
// worth of recent messages.
//
// Reference: Pi compaction.js findCutPoint (line 220)
func FindCutPoint(entries []session.Entry, startIndex, endIndex, keepRecentTokens int) CutPoint {
	// Find valid cut points (user messages, branch summaries)
	cutPoints := findValidCutPoints(entries, startIndex, endIndex)
	if len(cutPoints) == 0 {
		return CutPoint{FirstKeptEntryIndex: startIndex, TurnStartIndex: -1}
	}

	// Accumulate tokens from the end until we have enough
	accumulatedTokens := 0
	cutIndex := cutPoints[0]
	for i := endIndex - 1; i >= startIndex; i-- {
		me, ok := entries[i].(*session.MessageEntry)
		if !ok {
			continue
		}
		tokens := EstimateTokens(me.Message)
		accumulatedTokens += tokens
		if accumulatedTokens >= keepRecentTokens {
			// Find the nearest valid cut point at or after i
			for _, c := range cutPoints {
				if c >= i {
					cutIndex = c
					break
				}
			}
			break
		}
	}

	// Back up to avoid splitting metadata
	for cutIndex > startIndex {
		prev := entries[cutIndex-1]
		if _, ok := prev.(*session.CompactionEntry); ok {
			break
		}
		if _, ok := prev.(*session.MessageEntry); ok {
			break
		}
		cutIndex--
	}

	// Check if the cut splits a turn
	cutEntry := entries[cutIndex]
	isUserMessage := false
	if me, ok := cutEntry.(*session.MessageEntry); ok {
		if _, ok := me.Message.(*session.UserMessage); ok {
			isUserMessage = true
		}
	}

	turnStartIndex := -1
	if !isUserMessage {
		turnStartIndex = findTurnStartIndex(entries, cutIndex, startIndex)
	}

	return CutPoint{
		FirstKeptEntryIndex: cutIndex,
		TurnStartIndex:      turnStartIndex,
		IsSplitTurn:         !isUserMessage && turnStartIndex != -1,
	}
}

func findValidCutPoints(entries []session.Entry, startIndex, endIndex int) []int {
	var cutPoints []int
	for i := startIndex; i < endIndex; i++ {
		switch entries[i].(type) {
		case *session.MessageEntry:
			cutPoints = append(cutPoints, i)
		case *session.BranchSummaryEntry:
			cutPoints = append(cutPoints, i)
		}
	}
	return cutPoints
}

func findTurnStartIndex(entries []session.Entry, entryIndex, startIndex int) int {
	for i := entryIndex; i >= startIndex; i-- {
		switch e := entries[i].(type) {
		case *session.BranchSummaryEntry, *session.CompactionEntry:
			return i
		case *session.MessageEntry:
			switch e.Message.(type) {
			case *session.UserMessage:
				return i
			}
		}
	}
	return -1
}

// CompactionResult holds the result of a compaction operation.
type CompactionResult struct {
	Summary          string
	FirstKeptEntryID string
	TokensBefore     int
}

// SummarizationSystemPrompt instructs the LLM how to summarize.
//
// Reference: Pi compaction.js SUMMARIZATION_SYSTEM_PROMPT (line 280)
const SummarizationSystemPrompt = `You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.

Do NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.`

// SummarizationPrompt is the user prompt for summarization.
//
// Reference: Pi compaction.js SUMMARIZATION_PROMPT (line 285)
const SummarizationPrompt = `The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.

Use this EXACT format:

## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or "(none)" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [Ordered list of what should happen next]

## Critical Context
- [Any data, examples, or references needed to continue]
- [Or "(none)" if not applicable]

Keep each section concise. Preserve exact file paths, function names, and error messages.`

// UpdateSummarizationPrompt is the prompt for updating an existing summary.
//
// Reference: Pi compaction.js UPDATE_SUMMARIZATION_PROMPT (line 320)
const UpdateSummarizationPrompt = `The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.

Update the existing structured summary with new information. RULES:
- PRESERVE all existing information from the previous summary
- ADD new progress, decisions, and context from the new messages
- UPDATE the Progress section: move items from "In Progress" to "Done" when completed
- UPDATE "Next Steps" based on what was accomplished
- PRESERVE exact file paths, function names, and error messages
- If something is no longer relevant, you may remove it

Use this EXACT format:

## Goal
[Preserve existing goals, add new ones if the task expanded]

## Constraints & Preferences
- [Preserve existing, add new ones discovered]

## Progress
### Done
- [x] [Include previously done items AND newly completed items]

### In Progress
- [ ] [Current work - update based on progress]

### Blocked
- [Current blockers - remove if resolved]

## Key Decisions
- **[Decision]**: [Brief rationale] (preserve all previous, add new)

## Next Steps
1. [Update based on current state]

## Critical Context
- [Preserve important context, add new if needed]

Keep each section concise. Preserve exact file paths, function names, and error messages.`

// GenerateSummary calls the LLM to generate a conversation summary.
//
// Reference: Pi compaction.js generateSummary (line 350)
func GenerateSummary(ctx context.Context, messages []session.Message, streamFn func(ctx context.Context, req *llm.Request) (llm.Stream, error), model string, previousSummary string) (string, error) {
	// Build the conversation text
	var conversation strings.Builder
	for _, msg := range messages {
		switch m := msg.(type) {
		case *session.UserMessage:
			conversation.WriteString("User: ")
			for _, c := range m.Content {
				if tc, ok := c.(session.TextContent); ok {
					conversation.WriteString(tc.Text)
				}
			}
			conversation.WriteString("\n\n")
		case *session.AssistantMessage:
			conversation.WriteString("Assistant: ")
			for _, c := range m.Content {
				if tc, ok := c.(session.TextContent); ok {
					conversation.WriteString(tc.Text)
				}
			}
			conversation.WriteString("\n\n")
		case *session.ToolResultMessage:
			conversation.WriteString("Tool Result: ")
			for _, c := range m.Content {
				if tc, ok := c.(session.TextContent); ok {
					conversation.WriteString(tc.Text)
				}
			}
			conversation.WriteString("\n\n")
		}
	}

	// Build the prompt
	var prompt strings.Builder
	prompt.WriteString("<conversation>\n")
	prompt.WriteString(conversation.String())
	prompt.WriteString("</conversation>\n\n")

	if previousSummary != "" {
		prompt.WriteString("<previous-summary>\n")
		prompt.WriteString(previousSummary)
		prompt.WriteString("</previous-summary>\n\n")
		prompt.WriteString(UpdateSummarizationPrompt)
	} else {
		prompt.WriteString(SummarizationPrompt)
	}

	// Call the LLM
	req := &llm.Request{
		Model: model,
		Messages: []llm.Message{
			{Role: "system", Content: SummarizationSystemPrompt},
			{Role: "user", Content: prompt.String()},
		},
		MaxTokens: 4096,
	}

	stream, err := streamFn(ctx, req)
	if err != nil {
		return "", fmt.Errorf("summarization failed: %w", err)
	}
	defer stream.Close()

	// Collect the response
	var summary strings.Builder
	for {
		chunk, ok := stream.Next()
		if !ok {
			break
		}
		if chunk.Content != "" {
			summary.WriteString(chunk.Content)
		}
	}

	if err := stream.Err(); err != nil {
		return "", fmt.Errorf("summarization stream error: %w", err)
	}

	return summary.String(), nil
}

// PrepareCompaction prepares the session for compaction.
//
// Reference: Pi compaction.js prepareCompaction (line 395)
func PrepareCompaction(ctx context.Context, sess session.Session, settings CompactionSettings) ([]session.Entry, []session.Message, string, error) {
	snap, err := sess.BuildContext(ctx)
	if err != nil {
		return nil, nil, "", fmt.Errorf("build context: %w", err)
	}

	entries, err := sess.Branch(ctx)
	if err != nil {
		return nil, nil, "", fmt.Errorf("branch: %w", err)
	}

	if len(entries) == 0 {
		return nil, nil, "", nil
	}

	// Check if last entry is already a compaction
	if _, ok := entries[len(entries)-1].(*session.CompactionEntry); ok {
		return nil, nil, "", nil
	}

	// Find previous compaction
	prevCompactionIndex := -1
	var previousSummary string
	for i := len(entries) - 1; i >= 0; i-- {
		if ce, ok := entries[i].(*session.CompactionEntry); ok {
			prevCompactionIndex = i
			previousSummary = ce.Summary
			break
		}
	}

	// Determine boundary
	boundaryStart := 0
	if prevCompactionIndex >= 0 {
		// Find the first kept entry from the previous compaction
		ce := entries[prevCompactionIndex].(*session.CompactionEntry)
		for i, entry := range entries {
			if entry.ID() == ce.FirstKeptID {
				boundaryStart = i
				break
			}
		}
	}
	boundaryEnd := len(entries)

	// Find cut point
	cutPoint := FindCutPoint(entries, boundaryStart, boundaryEnd, settings.KeepRecentTokens)

	if cutPoint.FirstKeptEntryIndex >= len(entries) {
		return nil, nil, "", nil
	}

	firstKeptEntry := entries[cutPoint.FirstKeptEntryIndex]
	firstKeptEntryID := firstKeptEntry.ID()
	if firstKeptEntryID == "" {
		return nil, nil, "", fmt.Errorf("first kept entry has no ID")
	}

	// Extract messages to summarize
	historyEnd := cutPoint.FirstKeptEntryIndex
	if cutPoint.IsSplitTurn {
		historyEnd = cutPoint.TurnStartIndex
	}

	var messagesToSummarize []session.Message
	for i := boundaryStart; i < historyEnd; i++ {
		if me, ok := entries[i].(*session.MessageEntry); ok {
			messagesToSummarize = append(messagesToSummarize, me.Message)
		}
	}

	_ = snap // used for token estimation
	return entries, messagesToSummarize, previousSummary, nil
}

// Compact performs compaction on the session.
//
// Reference: Pi compaction.js compact (line 460)
func Compact(ctx context.Context, sess session.Session, streamFn func(ctx context.Context, req *llm.Request) (llm.Stream, error), model string, settings CompactionSettings) (*CompactionResult, error) {
	entries, messagesToSummarize, previousSummary, err := PrepareCompaction(ctx, sess, settings)
	if err != nil {
		return nil, err
	}
	if len(messagesToSummarize) == 0 {
		return nil, nil
	}

	// Count tokens before compaction
	tokensBefore := EstimateContextTokens(messagesToSummarize).Tokens

	// Generate summary
	summary, err := GenerateSummary(ctx, messagesToSummarize, streamFn, model, previousSummary)
	if err != nil {
		return nil, err
	}

	// Find first kept entry ID
	firstKeptEntryID := ""
	if len(entries) > 0 {
		// Find the cut point again to get the first kept entry
		cutPoint := FindCutPoint(entries, 0, len(entries), settings.KeepRecentTokens)
		if cutPoint.FirstKeptEntryIndex < len(entries) {
			firstKeptEntryID = entries[cutPoint.FirstKeptEntryIndex].ID()
		}
	}

	// Append compaction to session
	compactionData := session.CompactionData{
		Summary:      summary,
		FirstKeptID:  firstKeptEntryID,
		TokensBefore: tokensBefore,
	}

	_, err = sess.AppendCompaction(ctx, compactionData)
	if err != nil {
		return nil, fmt.Errorf("append compaction: %w", err)
	}

	return &CompactionResult{
		Summary:          summary,
		FirstKeptEntryID: firstKeptEntryID,
		TokensBefore:     tokensBefore,
	}, nil
}

// ShouldCompactAfterTurn checks if compaction should run after a turn.
// This is the auto-compaction trigger.
//
// Reference: Pi agent-harness.js shouldCompact (line 500)
func ShouldCompactAfterTurn(ctx context.Context, sess session.Session, contextWindow int, settings CompactionSettings) bool {
	if !settings.Enabled {
		return false
	}

	snap, err := sess.BuildContext(ctx)
	if err != nil {
		return false
	}

	estimate := EstimateContextTokens(snap.Messages)
	return ShouldCompact(estimate.Tokens, contextWindow, settings)
}

// IsContextOverflowError checks if an error is a context overflow error.
// These errors should trigger compaction and retry.
func IsContextOverflowError(err error) bool {
	if err == nil {
		return false
	}
	errStr := err.Error()
	return strings.Contains(errStr, "context_length_exceeded") ||
		strings.Contains(errStr, "context window") ||
		strings.Contains(errStr, "maximum context length") ||
		strings.Contains(errStr, "too many tokens")
}
