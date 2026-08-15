package agent

import (
	"fmt"
	"strings"

	"github.com/nijaru/ion/session"
)

// MicroCompactionOptions controls in-memory context pruning for historical tool results.
type MicroCompactionOptions struct {
	// Enabled controls whether historical tool outputs are pruned.
	Enabled bool
	// KeepRecentTurns specifies how many recent user turns to keep unpruned (default: 2).
	KeepRecentTurns int
	// MaxLinesPerHistoricalResult is the maximum number of lines allowed in a historical tool result (default: 8).
	MaxLinesPerHistoricalResult int
	// MaxBytesPerHistoricalResult is the byte limit above which a historical tool result is pruned (default: 1024).
	MaxBytesPerHistoricalResult int
}

// DefaultMicroCompactionOptions returns the standard configuration for context pruning.
func DefaultMicroCompactionOptions() MicroCompactionOptions {
	return MicroCompactionOptions{
		Enabled:                     true,
		KeepRecentTurns:             2,
		MaxLinesPerHistoricalResult: 8,
		MaxBytesPerHistoricalResult: 1024,
	}
}

func normalizeMicroCompaction(opts MicroCompactionOptions) MicroCompactionOptions {
	defaults := DefaultMicroCompactionOptions()
	if !opts.Enabled && opts.KeepRecentTurns == 0 {
		return defaults
	}
	if opts.KeepRecentTurns <= 0 {
		opts.KeepRecentTurns = defaults.KeepRecentTurns
	}
	if opts.MaxLinesPerHistoricalResult <= 0 {
		opts.MaxLinesPerHistoricalResult = defaults.MaxLinesPerHistoricalResult
	}
	if opts.MaxBytesPerHistoricalResult <= 0 {
		opts.MaxBytesPerHistoricalResult = defaults.MaxBytesPerHistoricalResult
	}
	return opts
}

// PruneHistoricalToolOutputs returns a copy of msgs where bulky tool results from
// older turns are pruned down to concise head/tail summaries.
// The underlying durable session entries are never modified.
func PruneHistoricalToolOutputs(msgs []session.Message, opts MicroCompactionOptions) []session.Message {
	if !opts.Enabled || len(msgs) == 0 {
		return msgs
	}
	if opts.KeepRecentTurns <= 0 {
		opts.KeepRecentTurns = 2
	}
	if opts.MaxLinesPerHistoricalResult <= 0 {
		opts.MaxLinesPerHistoricalResult = 8
	}
	if opts.MaxBytesPerHistoricalResult <= 0 {
		opts.MaxBytesPerHistoricalResult = 1024
	}

	totalUserMsgs := 0
	for _, m := range msgs {
		if _, ok := m.(*session.UserMessage); ok {
			totalUserMsgs++
		}
	}

	// If all messages fall within the recent turns window, nothing to prune.
	if totalUserMsgs <= opts.KeepRecentTurns {
		return msgs
	}

	cutoffUserIdx := totalUserMsgs - opts.KeepRecentTurns
	result := make([]session.Message, len(msgs))
	currentUserIdx := 0

	for i, m := range msgs {
		if _, ok := m.(*session.UserMessage); ok {
			currentUserIdx++
		}

		if currentUserIdx <= cutoffUserIdx {
			if tm, ok := m.(*session.ToolResultMessage); ok && !tm.IsError {
				result[i] = pruneToolResultMessage(tm, opts)
				continue
			}
		}
		result[i] = m
	}

	return result
}

func pruneToolResultMessage(tm *session.ToolResultMessage, opts MicroCompactionOptions) *session.ToolResultMessage {
	hasPrunableText := false
	for _, c := range tm.Content {
		if tc, ok := c.(session.TextContent); ok {
			lines := strings.Split(tc.Text, "\n")
			if len(lines) > opts.MaxLinesPerHistoricalResult || len(tc.Text) > opts.MaxBytesPerHistoricalResult {
				hasPrunableText = true
				break
			}
		}
	}

	if !hasPrunableText {
		return tm
	}

	newContent := make([]session.Content, len(tm.Content))
	for j, c := range tm.Content {
		tc, ok := c.(session.TextContent)
		if !ok {
			newContent[j] = c
			continue
		}

		lines := strings.Split(tc.Text, "\n")
		if len(lines) <= opts.MaxLinesPerHistoricalResult && len(tc.Text) <= opts.MaxBytesPerHistoricalResult {
			newContent[j] = tc
			continue
		}

		headCount := opts.MaxLinesPerHistoricalResult / 2
		if headCount < 1 {
			headCount = 1
		}
		tailCount := opts.MaxLinesPerHistoricalResult - headCount
		if tailCount < 1 {
			tailCount = 1
		}

		if len(lines) > headCount+tailCount {
			prunedCount := len(lines) - headCount - tailCount
			head := strings.Join(lines[:headCount], "\n")
			tail := strings.Join(lines[len(lines)-tailCount:], "\n")
			summary := fmt.Sprintf(
				"%s\n... [%d lines pruned for context efficiency (%d bytes original)] ...\n%s",
				head,
				prunedCount,
				len(tc.Text),
				tail,
			)
			newContent[j] = session.TextContent{Text: summary}
		} else {
			// Lines count is small but bytes exceeded — truncate length.
			cut := opts.MaxBytesPerHistoricalResult
			if cut > len(tc.Text) {
				cut = len(tc.Text)
			}
			newContent[j] = session.TextContent{
				Text: tc.Text[:cut] + fmt.Sprintf("\n... [output pruned (%d bytes original)] ...", len(tc.Text)),
			}
		}
	}

	cloned := *tm
	cloned.Content = newContent
	return &cloned
}
