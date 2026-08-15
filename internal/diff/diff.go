package diff

import (
	"strings"
	"unicode"
)

// DiffPart represents a fragment of a line in an intra-line diff.
type DiffPart struct {
	Text    string
	Removed bool
	Added   bool
}

// ComputeIntraLineDiff computes common prefix, changed sections, and common suffix between oldLine and newLine.
func ComputeIntraLineDiff(oldLine, newLine string) (oldParts []DiffPart, newParts []DiffPart) {
	oldRunes := []rune(oldLine)
	newRunes := []rune(newLine)

	prefixLen := 0
	for prefixLen < len(oldRunes) && prefixLen < len(newRunes) && oldRunes[prefixLen] == newRunes[prefixLen] {
		prefixLen++
	}

	suffixLen := 0
	for suffixLen < (len(oldRunes)-prefixLen) && suffixLen < (len(newRunes)-prefixLen) &&
		oldRunes[len(oldRunes)-1-suffixLen] == newRunes[len(newRunes)-1-suffixLen] {
		suffixLen++
	}

	commonPrefix := string(oldRunes[:prefixLen])
	commonSuffix := string(oldRunes[len(oldRunes)-suffixLen:])

	oldMid := string(oldRunes[prefixLen : len(oldRunes)-suffixLen])
	newMid := string(newRunes[prefixLen : len(newRunes)-suffixLen])

	if commonPrefix != "" {
		oldParts = append(oldParts, DiffPart{Text: commonPrefix})
		newParts = append(newParts, DiffPart{Text: commonPrefix})
	}
	if oldMid != "" {
		oldParts = append(oldParts, DiffPart{Text: oldMid, Removed: true})
	}
	if newMid != "" {
		newParts = append(newParts, DiffPart{Text: newMid, Added: true})
	}
	if commonSuffix != "" {
		oldParts = append(oldParts, DiffPart{Text: commonSuffix})
		newParts = append(newParts, DiffPart{Text: commonSuffix})
	}

	return oldParts, newParts
}

// IsSingleLineReplacement returns true if the lines represent a unified diff with a single '-' followed by a single '+'.
func IsSingleLineReplacement(lines []string, index int) bool {
	if index < 0 || index >= len(lines) {
		return false
	}
	if !strings.HasPrefix(lines[index], "-") || strings.HasPrefix(lines[index], "---") {
		return false
	}
	// Check previous is not '-'
	if index > 0 && strings.HasPrefix(lines[index-1], "-") && !strings.HasPrefix(lines[index-1], "---") {
		return false
	}
	// Check next is '+' and next+1 is not '+'
	if index+1 >= len(lines) || !strings.HasPrefix(lines[index+1], "+") || strings.HasPrefix(lines[index+1], "+++") {
		return false
	}
	if index+2 < len(lines) && strings.HasPrefix(lines[index+2], "+") && !strings.HasPrefix(lines[index+2], "+++") {
		return false
	}
	return true
}

// StripLinePrefix strips diff marker character (+ or -) and optional leading line numbers.
func StripLinePrefix(line string) (prefix string, content string) {
	if line == "" {
		return "", ""
	}
	r := rune(line[0])
	if r == '+' || r == '-' || r == ' ' {
		prefix = string(r)
		rest := line[1:]
		// Skip optional line numbers e.g. " 123 " or "123 "
		idx := 0
		for idx < len(rest) && (unicode.IsDigit(rune(rest[idx])) || unicode.IsSpace(rune(rest[idx]))) {
			idx++
		}
		return prefix, rest[idx:]
	}
	return "", line
}
