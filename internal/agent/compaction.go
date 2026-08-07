// Package agent implements context compaction for the harness.
//
// Ported from Pi's pi-agent-core/dist/harness/compaction/compaction.js (527 lines).
// The algorithm:
//  1. Estimate context tokens from session messages (usage-aware)
//  2. If over threshold, find a cut point that keeps recent tokens
//  3. Call LLM to summarize the messages before the cut (with auth/headers/signal)
//  4. Append a CompactionEntry to the session with file operation details
//
// Key invariants (from Pi):
//   - Compaction creates a CompactionEntry with summary + first kept entry ID
//   - buildContext skips pre-compaction entries (already in summary)
//   - The summary includes file operations (read/modified files)
//   - Token estimation uses provider usage when available (usage-aware)
//   - Split-turn prefix summarization preserves turn context across the cut
package agent

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strings"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// CompactionSettings controls when and how compaction runs.
type CompactionSettings struct {
	Enabled          bool
	ReserveTokens    int // Tokens reserved for output (default: 16384)
	KeepRecentTokens int // Tokens to keep from recent messages (default: 20000)
}

// DefaultCompactionSettings returns sensible defaults.
func DefaultCompactionSettings() CompactionSettings {
	return CompactionSettings{
		Enabled:          true,
		ReserveTokens:    16384,
		KeepRecentTokens: 20000,
	}
}

// CompactionFileOps holds file operation details extracted during compaction.
type CompactionFileOps struct {
	ReadFiles     []string `json:"readFiles,omitzero"`
	ModifiedFiles []string `json:"modifiedFiles,omitzero"`
}

// ContextTokenEstimate holds token estimation results.
type ContextTokenEstimate struct {
	Tokens         int
	UsageTokens    int
	TrailingTokens int
	LastUsageIndex *int
}

// CalculateContextTokens computes total token count from provider usage.
// Pi: totalTokens || input + output + cacheRead + cacheWrite
func CalculateContextTokens(usage session.Usage) int {
	if usage.TotalTokens > 0 {
		return usage.TotalTokens
	}
	return usage.Input + usage.Output + usage.CacheRead + usage.CacheWrite
}

// GetLastAssistantUsage returns usage from the last successful assistant message.
// Skips aborted/errored messages.
func GetLastAssistantUsage(messages []session.Message) *session.Usage {
	for i := len(messages) - 1; i >= 0; i-- {
		if am, ok := messages[i].(*session.AssistantMessage); ok {
			if am.StopReason != session.StopReasonAborted &&
				am.StopReason != session.StopReasonError {
				return &am.Usage
			}
		}
	}
	return &session.Usage{}
}

// GetLastAssistantUsageInfo returns usage data and its index in the message list.
func GetLastAssistantUsageInfo(messages []session.Message) (usage *session.Usage, index int) {
	for i := len(messages) - 1; i >= 0; i-- {
		if am, ok := messages[i].(*session.AssistantMessage); ok {
			if am.StopReason != session.StopReasonAborted &&
				am.StopReason != session.StopReasonError {
				return &am.Usage, i
			}
		}
	}
	return &session.Usage{}, -1
}

// EstimateTokens estimates the token count for a message.
// Uses a conservative character heuristic (1 token ≈ 4 chars).
// Pi: compaction.js estimateTokens (line 140)
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
	case *session.CustomMessage:
		if len(m.Content) > 0 {
			chars += len(m.Content)
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
		args, _ := json.Marshal(v.Arguments)
		return len(v.Name) + len(args)
	case session.ImageContent:
		return 4800 // estimated image chars
	default:
		return 0
	}
}

// EstimateContextTokens estimates total context tokens from session messages.
// Uses provider usage data from the last successful assistant message when available,
// falling back to character heuristics for messages after the usage-carrying message.
// Pi: compaction.js estimateContextTokens (line 95)
func EstimateContextTokens(messages []session.Message) ContextTokenEstimate {
	usage, usageIndex := GetLastAssistantUsageInfo(messages)
	idx := &usageIndex
	if usageIndex < 0 {
		// No usage data — use pure heuristic.
		total := 0
		for _, msg := range messages {
			total += EstimateTokens(msg)
		}
		return ContextTokenEstimate{
			Tokens:         total,
			UsageTokens:    0,
			TrailingTokens: total,
			LastUsageIndex: nil,
		}
	}

	// Usage-aware: provider-reported tokens + heuristic for trailing messages.
	usageTokens := CalculateContextTokens(*usage)
	trailingTokens := 0
	for i := usageIndex + 1; i < len(messages); i++ {
		trailingTokens += EstimateTokens(messages[i])
	}
	return ContextTokenEstimate{
		Tokens:         usageTokens + trailingTokens,
		UsageTokens:    usageTokens,
		TrailingTokens: trailingTokens,
		LastUsageIndex: idx,
	}
}

// ShouldCompact returns whether context usage exceeds the configured threshold.
// Pi: compaction.js shouldCompact (line 130)
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
// Pi: compaction.js findCutPoint (line 220)
func FindCutPoint(entries []session.Entry, startIndex, endIndex, keepRecentTokens int) CutPoint {
	cutPoints := findValidCutPoints(entries, startIndex, endIndex)
	if len(cutPoints) == 0 {
		return CutPoint{FirstKeptEntryIndex: startIndex, TurnStartIndex: -1}
	}

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

// --- file operation extraction (Pi: compaction/utils.js) ---

// ExtractFileOperationsFromMessage scans a message for file operation tool calls
// and adds them to the fileOps tracker.
func ExtractFileOperationsFromMessage(msg session.Message, fileOps *CompactionFileOps) {
	switch m := msg.(type) {
	case *session.AssistantMessage:
		for _, c := range m.Content {
			if tc, ok := c.(*session.ToolCall); ok {
				extractFromToolCall(tc, fileOps)
			}
		}
	case *session.ToolResultMessage:
		if m.IsError {
			return
		}
		for _, c := range m.Content {
			if tc, ok := c.(session.TextContent); ok {
				extractFromToolResultText(m.ToolName, tc.Text, fileOps)
			}
		}
	}
}

func extractFromToolCall(tc *session.ToolCall, fileOps *CompactionFileOps) {
	args := tc.Arguments
	if args == nil {
		return
	}

	// Common file operation tools: read, write, edit, bash
	switch tc.Name {
	case "read", "bash":
		// Track reads only — writes/edits are tracked from tool results
	case "edit":
		if path, ok := args["path"].(string); ok && path != "" {
			addOrReplace(fileOps, "modified", path)
		}
	case "write":
		if path, ok := args["path"].(string); ok && path != "" {
			addOrReplace(fileOps, "modified", path)
		}
	}
}

func extractFromToolResultText(toolName, text string, fileOps *CompactionFileOps) {
	switch toolName {
	case "read":
		// Pi: extract file paths from read tool results
		for _, line := range strings.Split(text, "\n") {
			line = strings.TrimSpace(line)
			if strings.HasPrefix(line, "File: ") || strings.HasPrefix(line, "Reading: ") {
				path := strings.TrimPrefix(line, "File: ")
				path = strings.TrimPrefix(path, "Reading: ")
				path = strings.TrimSpace(path)
				if path != "" {
					addIfNew(fileOps, "read", path)
				}
			}
		}
	case "bash":
		// Pi: track paths operated on via bash commands
		// Simple heuristic: look for file paths in output
	case "edit", "write":
		// Already tracked from tool call args
	}
}

func addIfNew(fileOps *CompactionFileOps, kind, path string) {
	list := &fileOps.ReadFiles
	if kind == "modified" {
		list = &fileOps.ModifiedFiles
	}
	for _, f := range *list {
		if f == path {
			return
		}
	}
	*list = append(*list, path)
}

func addOrReplace(fileOps *CompactionFileOps, kind, path string) {
	list := &fileOps.ReadFiles
	if kind == "modified" {
		list = &fileOps.ModifiedFiles
	}
	for i, f := range *list {
		if f == path {
			return
		}
		_ = i
	}
	*list = append(*list, path)
}

// ExtractFileOperations scans messages and previous compaction for file ops.
// Pi: compaction.js extractFileOperations (line 35)
func ExtractFileOperations(
	messages []session.Message,
	entries []session.Entry,
	prevCompactionIndex int,
) *CompactionFileOps {
	fileOps := &CompactionFileOps{}

	// Inherit from previous compaction.
	if prevCompactionIndex >= 0 {
		ce, ok := entries[prevCompactionIndex].(*session.CompactionEntry)
		if ok && len(ce.Details) > 0 {
			var prev CompactionFileOps
			if json.Unmarshal(ce.Details, &prev) == nil {
				fileOps.ReadFiles = append(fileOps.ReadFiles, prev.ReadFiles...)
				fileOps.ModifiedFiles = append(fileOps.ModifiedFiles, prev.ModifiedFiles...)
			}
		}
	}

	for _, msg := range messages {
		ExtractFileOperationsFromMessage(msg, fileOps)
	}
	return fileOps
}

// FormatFileOperations returns a formatted string for appending to the compaction summary.
// Pi: compaction/utils.js formatFileOperations
func FormatFileOperations(fileOps *CompactionFileOps) string {
	var b strings.Builder
	if len(fileOps.ReadFiles) > 0 || len(fileOps.ModifiedFiles) > 0 {
		b.WriteString("\n\nFiles referenced in this conversation:\n")
	}
	if len(fileOps.ReadFiles) > 0 {
		b.WriteString("- Read: ")
		for i, f := range fileOps.ReadFiles {
			if i > 0 {
				b.WriteString(", ")
			}
			b.WriteString(f)
		}
		b.WriteString("\n")
	}
	if len(fileOps.ModifiedFiles) > 0 {
		b.WriteString("- Modified: ")
		for i, f := range fileOps.ModifiedFiles {
			if i > 0 {
				b.WriteString(", ")
			}
			b.WriteString(f)
		}
		b.WriteString("\n")
	}
	return b.String()
}

// ComputeFileLists deduplicates and trims file operation lists.
func ComputeFileLists(fileOps *CompactionFileOps) (readFiles, modifiedFiles []string) {
	return deduplicateSlice(fileOps.ReadFiles), deduplicateSlice(fileOps.ModifiedFiles)
}

func deduplicateSlice(s []string) []string {
	seen := make(map[string]bool)
	var result []string
	for _, item := range s {
		if !seen[item] {
			seen[item] = true
			result = append(result, item)
		}
	}
	return result
}

// --- summarization ---

// SummaryResult holds generated summary text and the provider-reported usage
// for the summarization request.
type SummaryResult struct {
	Text  string
	Usage session.Usage
}

func summaryUsage(usage llm.Usage) session.Usage {
	return session.Usage{
		Input:       usage.InputTokens,
		Output:      usage.OutputTokens,
		CacheRead:   usage.CacheReadTokens,
		CacheWrite:  usage.CacheCreationTokens,
		TotalTokens: usage.TotalTokens,
		Cost:        session.Cost{Total: usage.Cost},
	}
}

// CompactionResult holds the result of a compaction operation.
type CompactionResult struct {
	Summary          string
	FirstKeptEntryID string
	TokensBefore     int
	Usage            session.Usage
	Details          CompactionFileOps
}

// SummarizationSystemPrompt instructs the LLM how to summarize.
// Pi: compaction.js SUMMARIZATION_SYSTEM_PROMPT (line 280)
const SummarizationSystemPrompt = `You are a context summarization assistant. Your task is to read a conversation between a user and an AI assistant, then produce a structured summary following the exact format specified.

Do NOT continue the conversation. Do NOT respond to any questions in the conversation. ONLY output the structured summary.`

// SummarizationPrompt is the user prompt for summarization.
// Pi: compaction.js SUMMARIZATION_PROMPT (line 285)
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
// Pi: compaction.js UPDATE_SUMMARIZATION_PROMPT (line 320)
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

// TurnPrefixSummarizationPrompt is the prompt for summarizing the prefix of a split turn.
// Pi: compaction.js TURN_PREFIX_SUMMARIZATION_PROMPT (line 470)
const TurnPrefixSummarizationPrompt = `This is the PREFIX of a turn that was too large to keep. The SUFFIX (recent work) is retained.

Summarize the prefix to provide context for the retained suffix:

## Original Request
[What did the user ask for in this turn?]

## Early Progress
- [Key decisions and work done in the prefix]

## Context for Suffix
- [Information needed to understand the retained recent work]

Be concise. Focus on what's needed to understand the kept suffix.`

// GenerateSummary calls the LLM to generate a conversation summary.
// Uses auth/headers/signal and computes maxTokens from reserve settings for Pi fidelity.
// Pi: compaction.js generateSummary (line 350)
func GenerateSummary(
	ctx context.Context,
	messages []session.Message,
	model string,
	reserveTokens, modelMaxTokens int,
	apiKey string,
	headers map[string]string,
	signal <-chan struct{},
	customInstructions string,
	previousSummary string,
	thinkingLevel session.ThinkingLevel,
	convert func([]session.Message) []llm.Message,
	streamFn func(ctx context.Context, req *llm.Request) (llm.Stream, error),
) (SummaryResult, error) {
	if streamFn == nil {
		return SummaryResult{}, errors.New("compaction stream is not configured")
	}
	// Pi: maxTokens = min(floor(0.8 * reserveTokens), model.maxTokens)
	maxTokens := int(0.8 * float64(reserveTokens))
	if modelMaxTokens > 0 && modelMaxTokens < maxTokens {
		maxTokens = modelMaxTokens
	}
	if maxTokens < 256 {
		maxTokens = 256 // floor
	}

	// Select prompt based on whether we're updating.
	basePrompt := SummarizationPrompt
	if previousSummary != "" {
		basePrompt = UpdateSummarizationPrompt
	}
	if customInstructions != "" {
		basePrompt = fmt.Sprintf("%s\n\nAdditional focus: %s", basePrompt, customInstructions)
	}

	// Build conversation text from messages.
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

	// Build prompt.
	var prompt strings.Builder
	prompt.WriteString("<conversation>\n")
	prompt.WriteString(conversation.String())
	prompt.WriteString("</conversation>\n\n")

	if previousSummary != "" {
		prompt.WriteString("<previous-summary>\n")
		prompt.WriteString(previousSummary)
		prompt.WriteString("</previous-summary>\n\n")
	}
	prompt.WriteString(basePrompt)

	// Build LLM request with auth/headers/signal.
	req := &llm.Request{
		Model: model,
		Messages: []llm.Message{
			{Role: "system", Content: SummarizationSystemPrompt},
			{Role: "user", Content: prompt.String()},
		},
		MaxTokens:       maxTokens,
		Headers:         headers,
		ReasoningEffort: providerReasoningEffort(thinkingLevel),
		ThinkingBudget:  0, // summarization doesn't need deep thinking
	}

	if apiKey != "" {
		if req.Headers == nil {
			req.Headers = make(map[string]string)
		}
		req.Headers["Authorization"] = "Bearer " + apiKey
	}

	// Check abort signal before calling.
	select {
	case <-signal:
		return SummaryResult{}, fmt.Errorf("compaction summarization aborted: context cancelled")
	default:
	}

	stream, err := streamFn(ctx, req)
	if err != nil {
		return SummaryResult{}, fmt.Errorf("summarization failed: %w", err)
	}
	defer stream.Close()

	// Collect the response. Usage is cumulative; retain the latest provider
	// report rather than summing repeated chunks.
	var summary strings.Builder
	var usage llm.Usage
	for {
		select {
		case <-signal:
			return SummaryResult{}, fmt.Errorf("compaction summarization aborted during streaming")
		default:
		}
		chunk, ok := stream.Next()
		if !ok {
			break
		}
		if chunk == nil {
			continue
		}
		if chunk.Content != "" {
			summary.WriteString(chunk.Content)
		}
		if chunk.Usage != nil {
			usage = *chunk.Usage
		}
	}

	if err := stream.Err(); err != nil {
		return SummaryResult{}, fmt.Errorf("summarization stream error: %w", err)
	}

	return SummaryResult{Text: summary.String(), Usage: summaryUsage(usage)}, nil
}

// GenerateTurnPrefixSummary generates a summary for the prefix messages of a split turn.
// Pi: compaction.js generateTurnPrefixSummary (line 480)
func GenerateTurnPrefixSummary(
	ctx context.Context,
	messages []session.Message,
	model string,
	reserveTokens, modelMaxTokens int,
	apiKey string,
	headers map[string]string,
	signal <-chan struct{},
	thinkingLevel session.ThinkingLevel,
	streamFn func(ctx context.Context, req *llm.Request) (llm.Stream, error),
) (SummaryResult, error) {
	if streamFn == nil {
		return SummaryResult{}, errors.New("compaction stream is not configured")
	}
	// Pi: maxTokens = min(floor(0.5 * reserveTokens), model.maxTokens)
	maxTokens := int(0.5 * float64(reserveTokens))
	if modelMaxTokens > 0 && modelMaxTokens < maxTokens {
		maxTokens = modelMaxTokens
	}
	if maxTokens < 128 {
		maxTokens = 128
	}

	// Build conversation text.
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

	promptText := fmt.Sprintf(
		"<conversation>\n%s</conversation>\n\n%s",
		conversation.String(),
		TurnPrefixSummarizationPrompt,
	)

	req := &llm.Request{
		Model: model,
		Messages: []llm.Message{
			{Role: "system", Content: SummarizationSystemPrompt},
			{Role: "user", Content: promptText},
		},
		MaxTokens:       maxTokens,
		Headers:         headers,
		ReasoningEffort: providerReasoningEffort(thinkingLevel),
	}

	if apiKey != "" {
		if req.Headers == nil {
			req.Headers = make(map[string]string)
		}
		req.Headers["Authorization"] = "Bearer " + apiKey
	}

	select {
	case <-signal:
		return SummaryResult{}, fmt.Errorf("turn prefix summarization aborted: context cancelled")
	default:
	}

	stream, err := streamFn(ctx, req)
	if err != nil {
		return SummaryResult{}, fmt.Errorf("turn prefix summarization failed: %w", err)
	}
	defer stream.Close()

	var summary strings.Builder
	var usage llm.Usage
	for {
		select {
		case <-signal:
			return SummaryResult{}, fmt.Errorf("turn prefix summarization aborted during streaming")
		default:
		}
		chunk, ok := stream.Next()
		if !ok {
			break
		}
		if chunk == nil {
			continue
		}
		if chunk.Content != "" {
			summary.WriteString(chunk.Content)
		}
		if chunk.Usage != nil {
			usage = *chunk.Usage
		}
	}

	if err := stream.Err(); err != nil {
		return SummaryResult{}, fmt.Errorf("turn prefix summarization stream error: %w", err)
	}

	return SummaryResult{Text: summary.String(), Usage: summaryUsage(usage)}, nil
}

// CompactionPreparation holds all data needed to execute compaction.
type CompactionPreparation struct {
	Entries             []session.Entry
	MessagesToSummarize []session.Message
	TurnPrefixMessages  []session.Message
	IsSplitTurn         bool
	TokensBefore        int
	PreviousSummary     string
	FileOps             *CompactionFileOps
	FirstKeptEntryID    string
	Settings            CompactionSettings
}

// PrepareCompaction prepares the session for compaction, returning
// all data needed by Compact.
// Pi: compaction.js prepareCompaction (line 395)
func PrepareCompaction(
	ctx context.Context,
	sess session.Session,
	settings CompactionSettings,
) (*CompactionPreparation, error) {
	snap, err := sess.BuildContext(ctx)
	if err != nil {
		return nil, fmt.Errorf("build context: %w", err)
	}

	entries, err := sess.Branch(ctx)
	if err != nil {
		return nil, fmt.Errorf("branch: %w", err)
	}

	if len(entries) == 0 {
		return nil, nil
	}

	// Check if last entry is already a compaction.
	if _, ok := entries[len(entries)-1].(*session.CompactionEntry); ok {
		return nil, nil
	}

	// Find previous compaction.
	prevCompactionIndex := -1
	var previousSummary string
	for i := len(entries) - 1; i >= 0; i-- {
		if _, ok := entries[i].(*session.CompactionEntry); ok {
			prevCompactionIndex = i
			previousSummary = entries[i].(*session.CompactionEntry).Summary
			break
		}
	}

	// Determine boundary.
	boundaryStart := 0
	if prevCompactionIndex >= 0 {
		ce := entries[prevCompactionIndex].(*session.CompactionEntry)
		for i, entry := range entries {
			if entry.ID() == ce.FirstKeptID {
				boundaryStart = i
				break
			}
		}
	}
	boundaryEnd := len(entries)

	// Pi: TokensBefore is computed from full context, not just summarized messages.
	tokensBefore := EstimateContextTokens(snap.Messages).Tokens

	// Find cut point.
	cutPoint := FindCutPoint(entries, boundaryStart, boundaryEnd, settings.KeepRecentTokens)

	if cutPoint.FirstKeptEntryIndex >= len(entries) {
		return nil, nil
	}

	firstKeptEntry := entries[cutPoint.FirstKeptEntryIndex]
	firstKeptEntryID := firstKeptEntry.ID()
	if firstKeptEntryID == "" {
		return nil, fmt.Errorf("first kept entry has no ID")
	}

	// Extract messages to summarize.
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

	// Extract turn prefix messages (split-turn support).
	var turnPrefixMessages []session.Message
	if cutPoint.IsSplitTurn {
		for i := cutPoint.TurnStartIndex; i < cutPoint.FirstKeptEntryIndex; i++ {
			if me, ok := entries[i].(*session.MessageEntry); ok {
				turnPrefixMessages = append(turnPrefixMessages, me.Message)
			}
		}
	}

	// Extract file operations.
	fileOps := ExtractFileOperations(messagesToSummarize, entries, prevCompactionIndex)
	if cutPoint.IsSplitTurn {
		for _, msg := range turnPrefixMessages {
			ExtractFileOperationsFromMessage(msg, fileOps)
		}
	}

	return &CompactionPreparation{
		Entries:             entries,
		MessagesToSummarize: messagesToSummarize,
		TurnPrefixMessages:  turnPrefixMessages,
		IsSplitTurn:         cutPoint.IsSplitTurn,
		TokensBefore:        tokensBefore,
		PreviousSummary:     previousSummary,
		FileOps:             fileOps,
		FirstKeptEntryID:    firstKeptEntryID,
		Settings:            settings,
	}, nil
}

// CompactOptions carries the model-call configuration for compaction summarization.
type CompactOptions struct {
	Model          string
	ModelMaxTokens int // 0 = not known
	APIKey         string
	Headers        map[string]string
	ThinkingLevel  session.ThinkingLevel
	// CustomInstructions is appended to the summarization prompt.
	CustomInstructions string
	// Convert transforms domain messages to provider messages (for the stream).
	Convert func([]session.Message) []llm.Message
	// StreamFn is the provider stream function.
	StreamFn func(ctx context.Context, req *llm.Request) (llm.Stream, error)
}

// Compact performs compaction on the session.
// Pi: compaction.js compact (line 460)
func Compact(
	ctx context.Context,
	sess session.Session,
	opts CompactOptions,
	settings CompactionSettings,
) (*CompactionResult, error) {
	prep, err := PrepareCompaction(ctx, sess, settings)
	if err != nil {
		return nil, err
	}
	if prep == nil || len(prep.MessagesToSummarize) == 0 {
		return nil, nil
	}

	// The context's done channel is already the cancellation signal. Do not
	// proxy it through a goroutine: a proxy needs a second owner and can either
	// leak on successful compaction or panic when cancellation races return.
	signal := ctx.Done()

	var summary string
	var usage session.Usage
	if prep.IsSplitTurn && len(prep.TurnPrefixMessages) > 0 {
		// Split-turn: summarize history and turn prefix in parallel.
		type result struct {
			summary SummaryResult
			err     error
		}
		historyCh := make(chan result, 1)
		prefixCh := make(chan result, 1)

		go func() {
			var generated SummaryResult
			var e error
			if len(prep.MessagesToSummarize) > 0 {
				generated, e = GenerateSummary(ctx, prep.MessagesToSummarize,
					opts.Model, settings.ReserveTokens, opts.ModelMaxTokens,
					opts.APIKey, opts.Headers, signal,
					opts.CustomInstructions, prep.PreviousSummary, opts.ThinkingLevel,
					opts.Convert, opts.StreamFn)
			} else {
				generated.Text = "No prior history."
			}
			historyCh <- result{generated, e}
		}()

		go func() {
			generated, e := GenerateTurnPrefixSummary(ctx, prep.TurnPrefixMessages,
				opts.Model, settings.ReserveTokens, opts.ModelMaxTokens,
				opts.APIKey, opts.Headers, signal,
				opts.ThinkingLevel, opts.StreamFn)
			prefixCh <- result{generated, e}
		}()

		historyResult := <-historyCh
		prefixResult := <-prefixCh

		if historyResult.err != nil {
			return nil, historyResult.err
		}
		if prefixResult.err != nil {
			return nil, prefixResult.err
		}

		summary = fmt.Sprintf(
			"%s\n\n---\n\n**Turn Context (split turn):**\n\n%s",
			historyResult.summary.Text,
			prefixResult.summary.Text,
		)
		usage = session.AddUsage(historyResult.summary.Usage, prefixResult.summary.Usage)
	} else {
		// Normal compaction: summarize history messages.
		generated, summaryErr := GenerateSummary(ctx, prep.MessagesToSummarize,
			opts.Model, settings.ReserveTokens, opts.ModelMaxTokens,
			opts.APIKey, opts.Headers, signal,
			opts.CustomInstructions, prep.PreviousSummary, opts.ThinkingLevel,
			opts.Convert, opts.StreamFn)
		if summaryErr != nil {
			return nil, summaryErr
		}
		summary = generated.Text
		usage = generated.Usage
	}

	// Append formatted file operations to summary.
	readFiles, modifiedFiles := ComputeFileLists(prep.FileOps)
	summary += FormatFileOperations(prep.FileOps)

	// Persist compaction entry.
	details := CompactionFileOps{
		ReadFiles:     readFiles,
		ModifiedFiles: modifiedFiles,
	}
	detailsJSON, err := json.Marshal(details)
	if err != nil {
		return nil, fmt.Errorf("marshal compaction details: %w", err)
	}

	_, err = sess.AppendCompaction(ctx, session.CompactionData{
		Summary:      summary,
		FirstKeptID:  prep.FirstKeptEntryID,
		TokensBefore: prep.TokensBefore,
		Usage:        usage,
		Details:      detailsJSON,
	})
	if err != nil {
		return nil, fmt.Errorf("append compaction: %w", err)
	}

	return &CompactionResult{
		Summary:          summary,
		FirstKeptEntryID: prep.FirstKeptEntryID,
		TokensBefore:     prep.TokensBefore,
		Usage:            usage,
		Details:          details,
	}, nil
}

// ShouldCompactAfterTurn checks if compaction should run after a turn.
// Pi: agent-harness.js shouldCompact (line 500)
func ShouldCompactAfterTurn(
	ctx context.Context,
	sess session.Session,
	contextWindow int,
	settings CompactionSettings,
) bool {
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
