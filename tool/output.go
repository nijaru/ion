package tool

import (
	"fmt"
	"strings"
	"unicode/utf8"
)

const MaxToolOutputSize = 50 * 1024

func limitToolOutput(output string) string {
	return limitToolOutputBytes(output, MaxToolOutputSize)
}

func limitToolOutputBytes(output string, limit int) string {
	if limit <= 0 || len(output) <= limit {
		return output
	}
	cut := limit
	for cut > 0 && !utf8.ValidString(output[:cut]) {
		cut--
	}
	omitted := len(output) - cut
	marker := toolOutputTruncationMarker(cut, omitted)
	prefix := strings.TrimRight(output[:cut], "\n")
	if prefix == "" {
		return strings.TrimLeft(marker, "\n")
	}
	return prefix + marker
}

func truncateToolOutputHead(output string, limit int) (string, bool) {
	if limit <= 0 || len(output) <= limit {
		return output, false
	}
	cut := limit
	for cut > 0 && !utf8.ValidString(output[:cut]) {
		cut--
	}
	return strings.TrimRight(output[:cut], "\n"), true
}

func toolOutputLimitLabel(limit int) string {
	if limit > 0 && limit%1024 == 0 {
		return fmt.Sprintf("%dKB", limit/1024)
	}
	return fmt.Sprintf("%d bytes", limit)
}

func toolOutputTruncationMarker(cut, omitted int) string {
	return fmt.Sprintf(
		"\n\n[tool output truncated after %d bytes; %d bytes omitted. Use a narrower command, path, or line range to inspect the rest.]",
		cut,
		omitted,
	)
}

func toolOutputSafeAppendLen(prefix, chunk string, limit int) int {
	remaining := limit - len(prefix)
	if remaining <= 0 {
		return 0
	}
	cut := min(len(chunk), remaining)
	for cut > 0 && !utf8.ValidString(prefix+chunk[:cut]) {
		cut--
	}
	return cut
}
