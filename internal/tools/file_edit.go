package tools

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"slices"
	"strings"

	"github.com/aymanbagabas/go-udiff"
	"github.com/go-json-experiment/json"
	"github.com/nijaru/ion/llm"
	"golang.org/x/text/unicode/norm"
)

// Edit tool
type Edit struct {
	FileTool
}

func (e *Edit) Spec() llm.Spec {
	return llm.Spec{
		Name:        "edit",
		Description: "Apply one or more targeted exact text replacements to one file after validating every operation against the original content.",
		Parameters:  editParameters(),
	}
}

func (e *Edit) Execute(ctx context.Context, args string) (string, error) {
	input, err := decodeToolArgs[editInput]("edit", args)
	if err != nil {
		return "", err
	}
	if len(input.Edits) == 0 {
		return "", fmt.Errorf("edits must contain at least one operation")
	}

	absPath, err := e.mutationPath(input.Path)
	if err != nil {
		return "", err
	}
	if err := ctx.Err(); err != nil {
		return "", toolContextErr("edit", err)
	}

	content, err := os.ReadFile(absPath)
	if err != nil {
		return "", fmt.Errorf("failed to read %s: %w", input.Path, err)
	}
	info, err := os.Stat(absPath)
	if err != nil {
		return "", fmt.Errorf("failed to stat %s: %w", input.Path, err)
	}
	original := string(content)
	newContent, replacements, err := applyEditReplacements(input.Path, original, input.Edits)
	if err != nil {
		return "", err
	}

	if _, err := e.checkpointPaths(ctx, input.Path); err != nil {
		return "", toolContextErr("edit", err)
	}
	if err := ctx.Err(); err != nil {
		return "", toolContextErr("edit", err)
	}

	tmpPath, err := writeEditTempFile(absPath, []byte(newContent), info.Mode().Perm())
	if err != nil {
		return "", err
	}
	if err := ctx.Err(); err != nil {
		_ = os.Remove(tmpPath)
		return "", toolContextErr("edit", err)
	}
	if err := os.Rename(tmpPath, absPath); err != nil {
		_ = os.Remove(tmpPath)
		return "", err
	}

	diff := udiff.Unified("a/"+input.Path, "b/"+input.Path, original, newContent)
	return limitToolOutput(fmt.Sprintf(
		"Applied %d edit(s) with %d replacement(s) in %s.\n\n%s",
		len(input.Edits),
		replacements,
		input.Path,
		diff,
	)), nil
}

type editReplacement struct {
	OldString  string `json:"old_string"`
	NewString  string `json:"new_string"`
	ReplaceAll bool   `json:"replace_all"`
	Expected   int    `json:"expected_replacements"`
}

type editReplacementArg struct {
	OldString  string `json:"old_string"`
	NewString  string `json:"new_string"`
	OldText    string `json:"oldText"`
	NewText    string `json:"newText"`
	ReplaceAll bool   `json:"replace_all"`
	Expected   int    `json:"expected_replacements"`
}

type editReplacementArgs []editReplacementArg

type matchedReplacement struct {
	editIndex int
	start     int
	end       int
	newString string
}

func (i *editInput) UnmarshalJSON(data []byte) error {
	var raw struct {
		Path     string              `json:"path"`
		FilePath string              `json:"file_path"`
		Edits    editReplacementArgs `json:"edits"`
		OldText  string              `json:"oldText"`
		NewText  string              `json:"newText"`
	}
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	i.Path = raw.Path
	if i.Path == "" {
		i.Path = raw.FilePath
	}
	i.Edits = make([]editReplacement, 0, len(raw.Edits)+1)
	for _, edit := range raw.Edits {
		i.Edits = append(i.Edits, edit.replacement())
	}
	if raw.OldText != "" || raw.NewText != "" {
		i.Edits = append(i.Edits, editReplacementArg{
			OldText: raw.OldText,
			NewText: raw.NewText,
		}.replacement())
	}
	return nil
}

func (r *editReplacementArgs) UnmarshalJSON(data []byte) error {
	data = bytes.TrimSpace(data)
	if len(data) == 0 || bytes.Equal(data, []byte("null")) {
		*r = nil
		return nil
	}
	if data[0] == '"' {
		var encoded string
		if err := json.Unmarshal(data, &encoded); err != nil {
			return err
		}
		return r.UnmarshalJSON([]byte(encoded))
	}

	var replacements []editReplacementArg
	if err := json.Unmarshal(data, &replacements); err != nil {
		return err
	}
	*r = replacements
	return nil
}

func (a editReplacementArg) replacement() editReplacement {
	edit := editReplacement{
		OldString:  a.OldString,
		NewString:  a.NewString,
		ReplaceAll: a.ReplaceAll,
		Expected:   a.Expected,
	}
	if edit.OldString == "" {
		edit.OldString = a.OldText
	}
	if edit.NewString == "" {
		edit.NewString = a.NewText
	}
	return edit
}

func writeEditTempFile(path string, data []byte, mode os.FileMode) (string, error) {
	dir := filepath.Dir(path)
	base := filepath.Base(path)
	for attempt := 0; attempt < 16; attempt++ {
		suffix, err := randomHexSuffix()
		if err != nil {
			return "", err
		}
		name := filepath.Join(dir, "."+base+"."+suffix+".tmp")
		file, err := os.OpenFile(name, os.O_WRONLY|os.O_CREATE|os.O_EXCL, mode)
		if errors.Is(err, os.ErrExist) {
			continue
		}
		if err != nil {
			return "", err
		}
		if _, err := file.Write(data); err != nil {
			_ = file.Close()
			_ = os.Remove(name)
			return "", err
		}
		if err := file.Close(); err != nil {
			_ = os.Remove(name)
			return "", err
		}
		return name, nil
	}
	return "", fmt.Errorf("could not create temporary file for %s", path)
}

func applyEditReplacements(
	filePath, content string,
	edits []editReplacement,
) (string, int, error) {
	bom, strippedContent := stripBom(content)
	originalEnding := detectLineEnding(strippedContent)
	normalizedContent := normalizeToLF(strippedContent)

	normalizedEdits := make([]editReplacement, len(edits))
	for i, edit := range edits {
		if edit.OldString == "" {
			return "", 0, fmt.Errorf("edit[%d].oldText must not be empty in %s", i, filePath)
		}
		normalizedEdits[i] = editReplacement{
			OldString:  normalizeToLF(edit.OldString),
			NewString:  normalizeToLF(edit.NewString),
			ReplaceAll: edit.ReplaceAll,
			Expected:   edit.Expected,
		}
	}

	anyFuzzy := false
	for _, edit := range normalizedEdits {
		match := fuzzyFindText(normalizedContent, edit.OldString)
		if match.usedFuzzyMatch {
			anyFuzzy = true
			break
		}
	}

	var baseContent string
	if anyFuzzy {
		baseContent = normalizeForFuzzyMatch(normalizedContent)
	} else {
		baseContent = normalizedContent
	}

	matches := make([]matchedReplacement, 0, len(normalizedEdits))
	for i, edit := range normalizedEdits {
		matchResult := fuzzyFindText(baseContent, edit.OldString)
		if !matchResult.found {
			return "", 0, fmt.Errorf(
				"could not find edits[%d].oldText in %s. The old text must match exactly including all whitespace and newlines",
				i,
				filePath,
			)
		}

		occurrences := countOccurrences(baseContent, edit.OldString)
		if occurrences > 1 && !edit.ReplaceAll {
			return "", 0, fmt.Errorf(
				"found %d occurrences of edits[%d].oldText in %s. Each oldText must be unique. Please provide more context to make it unique%s",
				occurrences,
				i,
				filePath,
				occurrenceLineSummary(baseContent, edit.OldString),
			)
		}

		if edit.Expected > 0 && occurrences != edit.Expected {
			return "", 0, fmt.Errorf(
				"oldText expected %d replacement(s) in %s, found %d%s",
				edit.Expected,
				filePath,
				occurrences,
				occurrenceLineSummary(baseContent, edit.OldString),
			)
		}

		var indices []int
		if edit.ReplaceAll {
			indices = replacementIndexes(baseContent, edit.OldString, true)
		} else {
			indices = []int{matchResult.index}
		}

		for _, start := range indices {
			matches = append(matches, matchedReplacement{
				editIndex: i,
				start:     start,
				end:       start + matchResult.matchLength,
				newString: edit.NewString,
			})
		}
	}

	if len(matches) == 0 {
		return "", 0, fmt.Errorf("edit produced no replacements in %s", filePath)
	}

	slices.SortFunc(matches, func(a, b matchedReplacement) int {
		if a.start != b.start {
			return a.start - b.start
		}
		return a.end - b.end
	})

	for i := 1; i < len(matches); i++ {
		prev := matches[i-1]
		cur := matches[i]
		if prev.end > cur.start {
			return "", 0, fmt.Errorf(
				"edits[%d] and edits[%d] overlap in %s; merge them into one edit",
				prev.editIndex,
				cur.editIndex,
				filePath,
			)
		}
	}

	newContent := baseContent
	for i := len(matches) - 1; i >= 0; i-- {
		match := matches[i]
		newContent = newContent[:match.start] + match.newString + newContent[match.end:]
	}

	if newContent == baseContent {
		return "", 0, fmt.Errorf("edit produced no content changes in %s", filePath)
	}

	finalContent := bom + restoreLineEndings(newContent, originalEnding)
	return finalContent, len(matches), nil
}

func replacementIndexes(content, oldString string, replaceAll bool) []int {
	var indexes []int
	start := 0
	for {
		idx := strings.Index(content[start:], oldString)
		if idx < 0 {
			return indexes
		}
		absolute := start + idx
		indexes = append(indexes, absolute)
		if !replaceAll {
			return indexes
		}
		start = absolute + len(oldString)
	}
}

func randomHexSuffix() (string, error) {
	var buf [8]byte
	if _, err := io.ReadFull(rand.Reader, buf[:]); err != nil {
		return "", err
	}
	return hex.EncodeToString(buf[:]), nil
}

func detectLineEnding(content string) string {
	crlfIdx := strings.Index(content, "\r\n")
	lfIdx := strings.Index(content, "\n")
	if lfIdx == -1 {
		return "\n"
	}
	if crlfIdx == -1 {
		return "\n"
	}
	if crlfIdx < lfIdx {
		return "\r\n"
	}
	return "\n"
}

func normalizeToLF(text string) string {
	return strings.ReplaceAll(strings.ReplaceAll(text, "\r\n", "\n"), "\r", "\n")
}

func restoreLineEndings(text, ending string) string {
	if ending == "\r\n" {
		return strings.ReplaceAll(text, "\n", "\r\n")
	}
	return text
}

func stripBom(content string) (string, string) {
	if strings.HasPrefix(content, "\ufeff") {
		return "\ufeff", content[len("\ufeff"):]
	}
	return "", content
}

func normalizeForFuzzyMatch(text string) string {
	text = norm.NFKC.String(text)
	lines := strings.Split(text, "\n")
	for i, line := range lines {
		lines[i] = strings.TrimRight(line, " \t\r")
	}
	text = strings.Join(lines, "\n")
	text = replaceRunes(text, []rune{'\u2018', '\u2019', '\u201a', '\u201b'}, '\'')
	text = replaceRunes(text, []rune{'\u201c', '\u201d', '\u201e', '\u201f'}, '"')
	text = replaceRunes(text, []rune{'\u2010', '\u2011', '\u2012', '\u2013', '\u2014', '\u2015', '\u2212'}, '-')
	specialSpaces := []rune{'\u00a0', '\u2002', '\u2003', '\u2004', '\u2005', '\u2006', '\u2007', '\u2008', '\u2009', '\u200a', '\u202f', '\u205f', '\u3000'}
	text = replaceRunes(text, specialSpaces, ' ')
	return text
}

func replaceRunes(text string, targets []rune, replacement rune) string {
	targetMap := make(map[rune]bool, len(targets))
	for _, r := range targets {
		targetMap[r] = true
	}
	var b strings.Builder
	b.Grow(len(text))
	for _, r := range text {
		if targetMap[r] {
			b.WriteRune(replacement)
		} else {
			b.WriteRune(r)
		}
	}
	return b.String()
}

type fuzzyMatchResult struct {
	found                 bool
	index                 int
	matchLength           int
	usedFuzzyMatch        bool
	contentForReplacement string
}

func fuzzyFindText(content, oldText string) fuzzyMatchResult {
	if idx := strings.Index(content, oldText); idx != -1 {
		return fuzzyMatchResult{
			found:                 true,
			index:                 idx,
			matchLength:           len(oldText),
			usedFuzzyMatch:        false,
			contentForReplacement: content,
		}
	}
	fuzzyContent := normalizeForFuzzyMatch(content)
	fuzzyOldText := normalizeForFuzzyMatch(oldText)
	if idx := strings.Index(fuzzyContent, fuzzyOldText); idx != -1 {
		return fuzzyMatchResult{
			found:                 true,
			index:                 idx,
			matchLength:           len(fuzzyOldText),
			usedFuzzyMatch:        true,
			contentForReplacement: fuzzyContent,
		}
	}
	return fuzzyMatchResult{
		found:                 false,
		index:                 -1,
		matchLength:           0,
		usedFuzzyMatch:        false,
		contentForReplacement: content,
	}
}

func countOccurrences(content, oldText string) int {
	fuzzyContent := normalizeForFuzzyMatch(content)
	fuzzyOldText := normalizeForFuzzyMatch(oldText)
	if fuzzyOldText == "" {
		return 0
	}
	return strings.Count(fuzzyContent, fuzzyOldText)
}

func occurrenceLineSummary(content, needle string) string {
	fuzzyContent := normalizeForFuzzyMatch(content)
	fuzzyNeedle := normalizeForFuzzyMatch(needle)
	if fuzzyNeedle == "" {
		return ""
	}
	var lines []string
	start := 0
	for {
		idx := strings.Index(fuzzyContent[start:], fuzzyNeedle)
		if idx < 0 {
			break
		}
		absoluteOffset := start + idx
		lineNum := strings.Count(fuzzyContent[:absoluteOffset], "\n") + 1
		lines = append(lines, fmt.Sprintf("%d", lineNum))
		start = absoluteOffset + len(fuzzyNeedle)
	}
	if len(lines) == 0 {
		return ""
	}
	return " at line(s) " + strings.Join(lines, ", ")
}
