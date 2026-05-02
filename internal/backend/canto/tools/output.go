package tools

import (
	"fmt"
	"strings"
	"unicode/utf8"
)

const maxToolOutputSize = 1024 * 1024 // 1MB

func limitToolOutput(output string) string {
	return limitToolOutputBytes(output, maxToolOutputSize)
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
	marker := fmt.Sprintf(
		"\n\n[tool output truncated after %d bytes; %d bytes omitted. Use a narrower command, path, or line range to inspect the rest.]",
		cut,
		omitted,
	)
	prefix := strings.TrimRight(output[:cut], "\n")
	if prefix == "" {
		return strings.TrimLeft(marker, "\n")
	}
	return prefix + marker
}
