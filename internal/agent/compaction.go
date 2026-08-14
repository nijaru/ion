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
	"sort"
	"strings"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
)

// CompactionSettings controls when and how compaction runs.
type CompactionSettings struct {
	Enabled          bool
	ReserveTokens    int // Tokens reserved for output (default: 16384)
	KeepRecentTokens int // Tokens to keep from recent messages (default: 20000)

	// Micro-compaction (tier-1 historical tool output pruning)
	MicroEnabled         bool // When true, trims historical tool outputs on older turns
	MicroKeepRecentTurns int  // Number of recent turns kept at full fidelity (default: 3)
	MicroMaxToolChars    int  // Max character length for historical tool outputs (default: 2000)
}

// DefaultCompactionSettings returns sensible defaults.
func DefaultCompactionSettings() CompactionSettings {
	return CompactionSettings{
		Enabled:              true,
		ReserveTokens:        16384,
		KeepRecentTokens:     20000,
		MicroEnabled:         false,
		MicroKeepRecentTurns: DefaultMicroCompactKeepTurns,
		MicroMaxToolChars:    DefaultMicroCompactMaxToolChars,
	}
}

func normalizeCompactionSettings(settings CompactionSettings, contextWindow int) CompactionSettings {
	defaults := DefaultCompactionSettings()
	if settings.ReserveTokens <= 0 {
		settings.ReserveTokens = defaults.ReserveTokens
	}
	if settings.KeepRecentTokens <= 0 {
		settings.KeepRecentTokens = defaults.KeepRecentTokens
	}
	if settings.MicroKeepRecentTurns <= 0 {
		settings.MicroKeepRecentTurns = defaults.MicroKeepRecentTurns
	}
	if settings.MicroMaxToolChars <= 0 {
		settings.MicroMaxToolChars = defaults.MicroMaxToolChars
	}
	if contextWindow > 0 {
		// Keep both the summary response and recent conversation inside the
		// resolved model window. Without this clamp a small model is permanently
		// over threshold and receives a summary request larger than its context.
		maxReserve := contextWindow / 2
		if maxReserve < 1 {
			maxReserve = 1
		}
		if settings.ReserveTokens > maxReserve {
			settings.ReserveTokens = maxReserve
		}
		maxRecent := contextWindow - settings.ReserveTokens
		if maxRecent < 1 {
			maxRecent = 1
		}
		if settings.KeepRecentTokens > maxRecent {
			settings.KeepRecentTokens = maxRecent
		}
	}
	return settings
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
				am.StopReason != session.StopReasonError && CalculateContextTokens(am.Usage) > 0 {
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
				am.StopReason != session.StopReasonError && CalculateContextTokens(am.Usage) > 0 {
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
		for _, content := range m.Content {
			chars += contentChars(content)
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
	if !settings.Enabled || contextWindow <= 0 {
		// An unknown model window must not turn into an always-compact runtime:
		// contextTokens > (0 - reserve) is true for every non-empty session.
		// The caller can still request explicit compaction, and the host should
		// supply a resolved model window for automatic recovery.
		return false
	}
	settings = normalizeCompactionSettings(settings, contextWindow)
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
		for _, message := range session.ContextMessagesForEntry(entries[i]) {
			accumulatedTokens += EstimateTokens(message)
		}
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

	// Back up to include adjacent metadata, but never cross a compaction
	// boundary or another context-visible entry. This keeps the durable cut
	// aligned with the same provider-visible boundaries used by replay.
	for cutIndex > startIndex {
		prev := entries[cutIndex-1]
		if _, ok := prev.(*session.CompactionEntry); ok {
			break
		}
		if len(session.ContextMessagesForEntry(prev)) > 0 {
			break
		}
		cutIndex--
	}

	cutEntry := entries[cutIndex]
	startsTurn := isTurnStartEntry(cutEntry)
	turnStartIndex := -1
	if !startsTurn {
		turnStartIndex = findTurnStartIndex(entries, cutIndex, startIndex)
	}

	return CutPoint{
		FirstKeptEntryIndex: cutIndex,
		TurnStartIndex:      turnStartIndex,
		IsSplitTurn:         !startsTurn && turnStartIndex != -1,
	}
}

func findValidCutPoints(entries []session.Entry, startIndex, endIndex int) []int {
	var cutPoints []int
	for i := startIndex; i < endIndex; i++ {
		for _, message := range session.ContextMessagesForEntry(entries[i]) {
			// A tool result is only valid after its assistant tool call. Cutting
			// at it would orphan the result in the retained context.
			if _, ok := message.(*session.ToolResultMessage); !ok {
				cutPoints = append(cutPoints, i)
				break
			}
		}
	}
	return cutPoints
}

func isTurnStartEntry(entry session.Entry) bool {
	for _, message := range session.ContextMessagesForEntry(entry) {
		switch message.(type) {
		case *session.UserMessage, *session.CustomMessage:
			return true
		}
	}
	return false
}

func findTurnStartIndex(entries []session.Entry, entryIndex, startIndex int) int {
	for i := entryIndex; i >= startIndex; i-- {
		for _, message := range session.ContextMessagesForEntry(entries[i]) {
			switch message.(type) {
			case *session.UserMessage, *session.CustomMessage:
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

	path, _ := args["path"].(string)
	if path == "" {
		return
	}

	// Match Pi's durable summary provenance: read/write/edit tools identify
	// paths from their assistant tool-call arguments. Bash is intentionally not
	// inferred because arbitrary shell commands do not have a canonical path.
	switch tc.Name {
	case "read":
		addIfNew(fileOps, "read", path)
	case "edit", "write":
		addIfNew(fileOps, "modified", path)
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

// FormatFileOperations returns a stable formatted string for appending to the
// compaction summary. Ion keeps its established human-readable form while
// preserving Pi's read-only versus modified distinction.
func FormatFileOperations(fileOps *CompactionFileOps) string {
	readFiles, modifiedFiles := ComputeFileLists(fileOps)
	var b strings.Builder
	if len(readFiles) > 0 || len(modifiedFiles) > 0 {
		b.WriteString("\n\nFiles referenced in this conversation:\n")
	}
	if len(readFiles) > 0 {
		b.WriteString("- Read: ")
		for i, f := range readFiles {
			if i > 0 {
				b.WriteString(", ")
			}
			b.WriteString(f)
		}
		b.WriteString("\n")
	}
	if len(modifiedFiles) > 0 {
		b.WriteString("- Modified: ")
		for i, f := range modifiedFiles {
			if i > 0 {
				b.WriteString(", ")
			}
			b.WriteString(f)
		}
		b.WriteString("\n")
	}
	return b.String()
}

// ComputeFileLists deduplicates file operation lists, removes modified files
// from the read-only list, and sorts both lists for stable durable summaries.
func ComputeFileLists(fileOps *CompactionFileOps) (readFiles, modifiedFiles []string) {
	if fileOps == nil {
		return nil, nil
	}
	modifiedFiles = deduplicateSlice(fileOps.ModifiedFiles)
	modifiedSet := make(map[string]struct{}, len(modifiedFiles))
	for _, path := range modifiedFiles {
		modifiedSet[path] = struct{}{}
	}
	for _, path := range deduplicateSlice(fileOps.ReadFiles) {
		if _, modified := modifiedSet[path]; !modified {
			readFiles = append(readFiles, path)
		}
	}
	return readFiles, modifiedFiles
}

func deduplicateSlice(s []string) []string {
	seen := make(map[string]struct{}, len(s))
	result := make([]string, 0, len(s))
	for _, item := range s {
		if item == "" {
			continue
		}
		if _, ok := seen[item]; !ok {
			seen[item] = struct{}{}
			result = append(result, item)
		}
	}
	sort.Strings(result)
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

func summaryResponseError(chunks []*llm.Chunk, operation string) error {
	for _, chunk := range chunks {
		if chunk == nil {
			continue
		}
		switch chunk.StopReason {
		case llm.StopReasonError, llm.StopReasonAborted:
			detail := strings.TrimSpace(chunk.ErrorMessage)
			if detail == "" {
				detail = string(chunk.StopReason)
			}
			return fmt.Errorf("%s response failed: %s", operation, detail)
		}
	}
	return nil
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

const summaryToolResultMaxChars = 2000

func cloneCompactionHeaders(headers map[string]string) map[string]string {
	if headers == nil {
		return nil
	}
	cloned := make(map[string]string, len(headers)+1)
	for key, value := range headers {
		cloned[key] = value
	}
	return cloned
}

func truncateSummaryText(text string, maxChars int) string {
	if len(text) <= maxChars {
		return text
	}
	return fmt.Sprintf("%s\n\n[... %d more characters truncated]", text[:maxChars], len(text)-maxChars)
}

func serializedCompactionContent(msg llm.Message) string {
	if msg.Content != "" {
		return msg.Content
	}
	var content strings.Builder
	for _, part := range msg.Parts {
		if part.Type == llm.ContentPartImage {
			content.WriteString("[image")
			if part.MIMEType != "" {
				content.WriteByte(' ')
				content.WriteString(part.MIMEType)
			}
			content.WriteByte(']')
			continue
		}
		content.WriteString(part.Text)
	}
	return content.String()
}

func serializeCompactionMessages(messages []session.Message, convert func([]session.Message) []llm.Message) string {
	if convert == nil {
		convert = DefaultConvert
	}
	converted := convert(messages)
	var conversation strings.Builder
	for _, msg := range converted {
		conversation.WriteString(string(msg.Role))
		if msg.Name != "" {
			conversation.WriteString(" (")
			conversation.WriteString(msg.Name)
			conversation.WriteString(")")
		}
		if msg.ToolID != "" {
			conversation.WriteString(" [")
			conversation.WriteString(msg.ToolID)
			conversation.WriteString("]")
		}
		conversation.WriteString(": ")
		content := serializedCompactionContent(msg)
		if msg.Role == llm.RoleTool {
			content = truncateSummaryText(content, summaryToolResultMaxChars)
		}
		conversation.WriteString(content)
		if msg.Reasoning != "" {
			conversation.WriteString("\nReasoning: ")
			conversation.WriteString(msg.Reasoning)
		}
		for _, call := range msg.Calls {
			conversation.WriteString("\nTool call ")
			conversation.WriteString(call.Function.Name)
			conversation.WriteString("(")
			conversation.WriteString(call.Function.Arguments)
			conversation.WriteByte(')')
		}
		conversation.WriteString("\n\n")
	}
	return conversation.String()
}

func summaryRetryPolicy(policy llm.StreamRetryPolicy) llm.StreamRetryPolicy {
	isTransient := policy.IsTransient
	policy.IsTransient = func(err error) bool {
		if isHookError(err) || llm.IsStreamCleanupError(err) {
			return false
		}
		return isTransient != nil && isTransient(err)
	}
	return policy
}

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
	retryPolicy llm.StreamRetryPolicy,
) (SummaryResult, error) {
	if streamFn == nil {
		return SummaryResult{}, errors.New("compaction stream is not configured")
	}
	// Pi: maxTokens = min(floor(0.8 * reserveTokens), model.maxTokens)
	maxTokens := int(0.8 * float64(reserveTokens))
	if maxTokens < 256 {
		maxTokens = 256 // floor
	}
	if modelMaxTokens > 0 && modelMaxTokens < maxTokens {
		maxTokens = modelMaxTokens
	}

	// Select prompt based on whether we're updating.
	basePrompt := SummarizationPrompt
	if previousSummary != "" {
		basePrompt = UpdateSummarizationPrompt
	}
	if customInstructions != "" {
		basePrompt = fmt.Sprintf("%s\n\nAdditional focus: %s", basePrompt, customInstructions)
	}

	// Build prompt from the provider-neutral conversion so tool calls,
	// reasoning, custom messages, and image placeholders remain visible to the
	// summarizer instead of being reduced to assistant text only.
	var prompt strings.Builder
	prompt.WriteString("<conversation>\n")
	prompt.WriteString(serializeCompactionMessages(messages, convert))
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
		Headers:         cloneCompactionHeaders(headers),
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
		return SummaryResult{}, fmt.Errorf("compaction summarization aborted: %w", context.Canceled)
	default:
	}

	chunks, err := llm.CollectStreamWithRetry(ctx, req, streamFn, summaryRetryPolicy(retryPolicy))
	if err != nil {
		if signal != nil {
			select {
			case <-signal:
				if llm.IsStreamCleanupError(err) {
					return SummaryResult{}, fmt.Errorf(
						"compaction summarization aborted during streaming: %w",
						errors.Join(context.Canceled, err),
					)
				}
				return SummaryResult{}, fmt.Errorf(
					"compaction summarization aborted during streaming: %w",
					context.Canceled,
				)
			default:
			}
		}
		return SummaryResult{}, fmt.Errorf("summarization failed: %w", err)
	}

	if err := summaryResponseError(chunks, "summarization"); err != nil {
		return SummaryResult{}, err
	}

	// Usage is cumulative; retain the latest provider report rather than
	// summing repeated chunks. Failed replay-safe attempts were discarded by
	// CollectStreamWithRetry before these chunks became visible here.
	var summary strings.Builder
	var usage llm.Usage
	for _, chunk := range chunks {
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
	if strings.TrimSpace(summary.String()) == "" {
		return SummaryResult{}, errors.New("summarization returned an empty summary")
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
	convert func([]session.Message) []llm.Message,
	streamFn func(ctx context.Context, req *llm.Request) (llm.Stream, error),
	retryPolicy llm.StreamRetryPolicy,
) (SummaryResult, error) {
	if streamFn == nil {
		return SummaryResult{}, errors.New("compaction stream is not configured")
	}
	// Pi: maxTokens = min(floor(0.5 * reserveTokens), model.maxTokens)
	maxTokens := int(0.5 * float64(reserveTokens))
	if maxTokens < 128 {
		maxTokens = 128
	}
	if modelMaxTokens > 0 && modelMaxTokens < maxTokens {
		maxTokens = modelMaxTokens
	}

	promptText := fmt.Sprintf(
		"<conversation>\n%s</conversation>\n\n%s",
		serializeCompactionMessages(messages, convert),
		TurnPrefixSummarizationPrompt,
	)

	req := &llm.Request{
		Model: model,
		Messages: []llm.Message{
			{Role: "system", Content: SummarizationSystemPrompt},
			{Role: "user", Content: promptText},
		},
		MaxTokens:       maxTokens,
		Headers:         cloneCompactionHeaders(headers),
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
		return SummaryResult{}, fmt.Errorf("turn prefix summarization aborted: %w", context.Canceled)
	default:
	}

	chunks, err := llm.CollectStreamWithRetry(ctx, req, streamFn, summaryRetryPolicy(retryPolicy))
	if err != nil {
		if signal != nil {
			select {
			case <-signal:
				if llm.IsStreamCleanupError(err) {
					return SummaryResult{}, fmt.Errorf(
						"turn prefix summarization aborted during streaming: %w",
						errors.Join(context.Canceled, err),
					)
				}
				return SummaryResult{}, fmt.Errorf(
					"turn prefix summarization aborted during streaming: %w",
					context.Canceled,
				)
			default:
			}
		}
		return SummaryResult{}, fmt.Errorf("turn prefix summarization failed: %w", err)
	}

	if err := summaryResponseError(chunks, "turn prefix summarization"); err != nil {
		return SummaryResult{}, err
	}

	var summary strings.Builder
	var usage llm.Usage
	for _, chunk := range chunks {
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
	if strings.TrimSpace(summary.String()) == "" {
		return SummaryResult{}, errors.New("turn prefix summarization returned an empty summary")
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
	settings = normalizeCompactionSettings(settings, 0)
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
		messagesToSummarize = append(messagesToSummarize, session.ContextMessagesForEntry(entries[i])...)
	}

	// Extract turn prefix messages (split-turn support).
	var turnPrefixMessages []session.Message
	if cutPoint.IsSplitTurn {
		for i := cutPoint.TurnStartIndex; i < cutPoint.FirstKeptEntryIndex; i++ {
			turnPrefixMessages = append(turnPrefixMessages, session.ContextMessagesForEntry(entries[i])...)
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
	// SummaryRetry permits replay of incomplete summary requests only. Summary
	// output is discarded until completion, unlike visible assistant streams.
	SummaryRetry  llm.StreamRetryPolicy
	ContextWindow int // 0 = unknown; used to bound compaction settings
}

// Compact performs compaction on the session.
// Pi: compaction.js compact (line 460)
func Compact(
	ctx context.Context,
	sess session.Session,
	opts CompactOptions,
	settings CompactionSettings,
) (*CompactionResult, error) {
	settings = normalizeCompactionSettings(settings, opts.ContextWindow)
	prep, err := PrepareCompaction(ctx, sess, settings)
	if err != nil {
		return nil, err
	}
	if prep == nil ||
		(len(prep.MessagesToSummarize) == 0 && len(prep.TurnPrefixMessages) == 0) {
		return nil, nil
	}

	// The context's done channel is already the cancellation signal. Do not
	// proxy it through a goroutine: a proxy needs a second owner and can either
	// leak on successful compaction or panic when cancellation races return.
	signal := ctx.Done()

	var summary string
	var usage session.Usage
	if prep.IsSplitTurn && len(prep.TurnPrefixMessages) > 0 {
		// Split-turn summaries share the runtime provider-hook boundary. Keep
		// preparation and hook execution ordered so stateful hook handlers see
		// deterministic request order; summary retry remains explicitly
		// discardable inside each request.
		var historySummary SummaryResult
		if len(prep.MessagesToSummarize) > 0 {
			var err error
			historySummary, err = GenerateSummary(ctx, prep.MessagesToSummarize,
				opts.Model, settings.ReserveTokens, opts.ModelMaxTokens,
				opts.APIKey, opts.Headers, signal,
				opts.CustomInstructions, prep.PreviousSummary, opts.ThinkingLevel,
				opts.Convert, opts.StreamFn, opts.SummaryRetry)
			if err != nil {
				return nil, err
			}
		} else {
			historySummary.Text = "No prior history."
		}

		prefixSummary, err := GenerateTurnPrefixSummary(ctx, prep.TurnPrefixMessages,
			opts.Model, settings.ReserveTokens, opts.ModelMaxTokens,
			opts.APIKey, opts.Headers, signal,
			opts.ThinkingLevel, opts.Convert, opts.StreamFn, opts.SummaryRetry)
		if err != nil {
			return nil, err
		}

		historyResult := historySummary
		prefixResult := prefixSummary

		summary = fmt.Sprintf(
			"%s\n\n---\n\n**Turn Context (split turn):**\n\n%s",
			historyResult.Text,
			prefixResult.Text,
		)
		usage = session.AddUsage(historyResult.Usage, prefixResult.Usage)
	} else {
		// Normal compaction: summarize history messages.
		generated, summaryErr := GenerateSummary(ctx, prep.MessagesToSummarize,
			opts.Model, settings.ReserveTokens, opts.ModelMaxTokens,
			opts.APIKey, opts.Headers, signal,
			opts.CustomInstructions, prep.PreviousSummary, opts.ThinkingLevel,
			opts.Convert, opts.StreamFn, opts.SummaryRetry)
		if summaryErr != nil {
			return nil, summaryErr
		}
		summary = generated.Text
		usage = generated.Usage
	}

	// Append stable, normalized file operations to the summary.
	readFiles, modifiedFiles := ComputeFileLists(prep.FileOps)
	summary += FormatFileOperations(&CompactionFileOps{
		ReadFiles: readFiles, ModifiedFiles: modifiedFiles,
	})

	// Persist compaction entry.
	details := CompactionFileOps{
		ReadFiles:     readFiles,
		ModifiedFiles: modifiedFiles,
	}
	detailsJSON, err := json.Marshal(details)
	if err != nil {
		return nil, fmt.Errorf("marshal compaction details: %w", err)
	}
	if err := ctx.Err(); err != nil {
		return nil, err
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

// DefaultMicroCompactKeepTurns is the number of recent turns kept at full fidelity.
const DefaultMicroCompactKeepTurns = 3

// DefaultMicroCompactMaxToolChars is the maximum character length for historical tool outputs.
const DefaultMicroCompactMaxToolChars = 2000

// MicroCompactMessages performs tier-1 micro-compaction on conversation messages.
// It keeps recent turns (default 3) untouched while trimming verbose historical
// tool results to prevent token bloating before full macro-compaction is triggered.
func MicroCompactMessages(messages []session.Message, keepRecentTurns int, maxToolChars int) []session.Message {
	if len(messages) == 0 {
		return messages
	}
	if keepRecentTurns <= 0 {
		keepRecentTurns = DefaultMicroCompactKeepTurns
	}
	if maxToolChars <= 0 {
		maxToolChars = DefaultMicroCompactMaxToolChars
	}

	// Identify turn boundary indices (UserMessage positions).
	var userIndices []int
	for i, msg := range messages {
		if _, ok := msg.(*session.UserMessage); ok {
			userIndices = append(userIndices, i)
		}
	}

	// If fewer turns than keepRecentTurns, no pruning needed.
	if len(userIndices) <= keepRecentTurns {
		return messages
	}

	// Cutoff index is the user message index that starts the kept recent turns.
	cutoffIdx := userIndices[len(userIndices)-keepRecentTurns]

	compacted := make([]session.Message, len(messages))
	copy(compacted, messages)

	for i := 0; i < cutoffIdx; i++ {
		if trm, ok := compacted[i].(*session.ToolResultMessage); ok {
			compacted[i] = microCompactToolResult(trm, maxToolChars)
		}
	}

	return compacted
}

func microCompactToolResult(trm *session.ToolResultMessage, maxChars int) *session.ToolResultMessage {
	if trm == nil || len(trm.Content) == 0 {
		return trm
	}

	hasLargeContent := false
	for _, c := range trm.Content {
		if tc, ok := c.(session.TextContent); ok && len(tc.Text) > maxChars {
			hasLargeContent = true
			break
		}
	}
	if !hasLargeContent {
		return trm
	}

	newContent := make([]session.Content, len(trm.Content))
	for j, c := range trm.Content {
		if tc, ok := c.(session.TextContent); ok && len(tc.Text) > maxChars {
			head := tc.Text[:500]
			tail := tc.Text[len(tc.Text)-200:]
			truncatedCount := len(tc.Text) - 700
			newContent[j] = session.TextContent{
				Text: fmt.Sprintf(
					"%s\n\n... [%d characters trimmed by micro-compaction] ...\n\n%s",
					head,
					truncatedCount,
					tail,
				),
			}
		} else {
			newContent[j] = c
		}
	}

	cloned := *trm
	cloned.Content = newContent
	return &cloned
}
