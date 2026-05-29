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
	"github.com/nijaru/ion/internal/llm"
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
	matches := make([]matchedReplacement, 0, len(edits))
	for i, edit := range edits {
		if err := validateEditStrings(edit.OldString, edit.NewString); err != nil {
			return "", 0, fmt.Errorf("edit[%d] in %s: %w", i, filePath, err)
		}
		oldString, newString := matchEditStrings(content, edit.OldString, edit.NewString)
		count, err := replacementCount(
			filePath,
			content,
			oldString,
			edit.ReplaceAll,
			edit.Expected,
		)
		if err != nil {
			return "", 0, fmt.Errorf("edit[%d]: %w", i, err)
		}
		for _, start := range replacementIndexes(content, oldString, edit.ReplaceAll) {
			matches = append(matches, matchedReplacement{
				editIndex: i,
				start:     start,
				end:       start + len(oldString),
				newString: newString,
			})
		}
		if !edit.ReplaceAll && count != 1 {
			return "", 0, fmt.Errorf(
				"edit[%d]: expected one replacement in %s, found %d",
				i,
				filePath,
				count,
			)
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
				"edit[%d] and edit[%d] overlap in %s; merge them into one edit",
				prev.editIndex,
				cur.editIndex,
				filePath,
			)
		}
	}

	newContent := content
	for i := len(matches) - 1; i >= 0; i-- {
		match := matches[i]
		newContent = newContent[:match.start] + match.newString + newContent[match.end:]
	}
	if newContent == content {
		return "", 0, fmt.Errorf("edit produced no content changes in %s", filePath)
	}
	return newContent, len(matches), nil
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

func validateEditStrings(oldString, newString string) error {
	if oldString == "" {
		return fmt.Errorf("oldText must not be empty")
	}
	if oldString == newString {
		return fmt.Errorf("newText must differ from oldText")
	}
	return nil
}

func matchEditStrings(content, oldString, newString string) (string, string) {
	if strings.Contains(content, "\r\n") && !strings.Contains(oldString, "\r\n") {
		oldString = strings.ReplaceAll(oldString, "\n", "\r\n")
		newString = strings.ReplaceAll(newString, "\n", "\r\n")
	}
	if strings.HasPrefix(content, "\ufeff"+oldString) && !strings.HasPrefix(oldString, "\ufeff") {
		oldString = "\ufeff" + oldString
		newString = "\ufeff" + newString
	}
	return oldString, newString
}

func replacementCount(
	filePath, content, oldString string,
	replaceAll bool,
	expected int,
) (int, error) {
	if expected < 0 {
		return 0, fmt.Errorf("%s: expected_replacements must be non-negative", filePath)
	}
	count := strings.Count(content, oldString)
	lines := occurrenceLineSummary(content, oldString)
	if count == 0 {
		return 0, fmt.Errorf("oldText not found in %s", filePath)
	}
	if expected > 0 && count != expected {
		return 0, fmt.Errorf(
			"oldText expected %d replacement(s) in %s, found %d%s",
			expected,
			filePath,
			count,
			lines,
		)
	}
	if !replaceAll && count > 1 {
		return 0, fmt.Errorf(
			"oldText is not unique in %s, found %d occurrences%s. Provide more context or use a legacy replace_all call with expected_replacements.",
			filePath,
			count,
			lines,
		)
	}
	return count, nil
}

func occurrenceLineSummary(content, needle string) string {
	if needle == "" {
		return ""
	}
	var lines []string
	lineNumber := 1
	for _, line := range strings.Split(content, "\n") {
		remaining := line
		for {
			idx := strings.Index(remaining, needle)
			if idx < 0 {
				break
			}
			lines = append(lines, fmt.Sprintf("%d", lineNumber))
			remaining = remaining[idx+len(needle):]
		}
		lineNumber++
	}
	if len(lines) == 0 {
		return ""
	}
	return " at line(s) " + strings.Join(lines, ", ")
}
