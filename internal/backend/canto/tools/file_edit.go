package tools

import (
	"context"
	"fmt"
	"strings"

	"github.com/aymanbagabas/go-udiff"
	"github.com/go-json-experiment/json"
	"github.com/nijaru/canto/llm"
)

// Edit tool
type Edit struct {
	FileTool
}

func (e *Edit) Spec() llm.Spec {
	return llm.Spec{
		Name:        "edit",
		Description: "Modify a file by replacing exact text with new text. Provide the exact string to find and its replacement. Use this for targeted changes.",
		Parameters: map[string]any{
			"type": "object",
			"properties": map[string]any{
				"file_path": map[string]any{
					"type":        "string",
					"description": "Relative path to the file to modify.",
				},
				"old_string": map[string]any{
					"type":        "string",
					"description": "The exact text to replace (must exist in file)",
				},
				"new_string": map[string]any{
					"type":        "string",
					"description": "The replacement text (must differ from old_string)",
				},
				"replace_all": map[string]any{
					"type":        "boolean",
					"description": "Replace all occurrences (default: false, requires unique match)",
				},
				"expected_replacements": map[string]any{
					"type":        "integer",
					"description": "Optional exact number of occurrences expected. Use with replace_all for broad replacements.",
				},
			},
			"required": []string{"file_path", "old_string", "new_string"},
		},
	}
}

func (e *Edit) Execute(ctx context.Context, args string) (string, error) {
	var input struct {
		FilePath   string `json:"file_path"`
		OldString  string `json:"old_string"`
		NewString  string `json:"new_string"`
		ReplaceAll bool   `json:"replace_all"`
		Expected   int    `json:"expected_replacements"`
	}
	if err := json.Unmarshal([]byte(args), &input); err != nil {
		return "", err
	}
	if err := validateEditStrings(input.OldString, input.NewString); err != nil {
		return "", err
	}

	relPath, err := e.relativePath(input.FilePath)
	if err != nil {
		return "", err
	}
	root, err := e.openRoot()
	if err != nil {
		return "", err
	}
	defer root.Close()

	content, err := root.ReadFile(relPath)
	if err != nil {
		return "", err
	}

	strContent := string(content)
	oldString, newString := matchEditStrings(strContent, input.OldString, input.NewString)
	count, err := replacementCount(
		input.FilePath,
		strContent,
		oldString,
		input.ReplaceAll,
		input.Expected,
	)
	if err != nil {
		return "", err
	}

	var newContent string
	if input.ReplaceAll {
		newContent = strings.Replace(strContent, oldString, newString, -1)
	} else {
		newContent = strings.Replace(strContent, oldString, newString, 1)
	}

	checkpointID, err := e.checkpointPaths(ctx, input.FilePath)
	if err != nil {
		return "", err
	}

	if err := root.WriteFile(relPath, []byte(newContent), 0o644); err != nil {
		return "", err
	}

	diff := udiff.Unified("a/"+input.FilePath, "b/"+input.FilePath, strContent, newContent)
	return limitToolOutput(
		appendCheckpointID(
			fmt.Sprintf(
				"Successfully replaced %d occurrence(s) in %s\n\n%s",
				count,
				input.FilePath,
				diff,
			),
			checkpointID,
		),
	), nil
}

// MultiEdit tool
type MultiEdit struct {
	FileTool
}

type EditOperation struct {
	FilePath   string `json:"file_path"`
	OldString  string `json:"old_string"`
	NewString  string `json:"new_string"`
	ReplaceAll bool   `json:"replace_all"`
	Expected   int    `json:"expected_replacements"`
}

func (m *MultiEdit) Spec() llm.Spec {
	return llm.Spec{
		Name:        "multi_edit",
		Description: "Apply multiple targeted text replacements across one or more files in a single atomic operation.",
		Parameters: map[string]any{
			"type": "object",
			"properties": map[string]any{
				"edits": map[string]any{
					"type": "array",
					"items": map[string]any{
						"type": "object",
						"properties": map[string]any{
							"file_path": map[string]any{
								"type":        "string",
								"description": "Path to the file to modify.",
							},
							"old_string": map[string]any{
								"type":        "string",
								"description": "The exact text to replace. Include context lines to ensure uniqueness.",
							},
							"new_string": map[string]any{
								"type":        "string",
								"description": "The replacement text.",
							},
							"replace_all": map[string]any{
								"type":        "boolean",
								"description": "Replace all occurrences (default: false, requires unique match).",
							},
							"expected_replacements": map[string]any{
								"type":        "integer",
								"description": "Optional exact number of occurrences expected. Use with replace_all for broad replacements.",
							},
						},
						"required": []string{"file_path", "old_string", "new_string"},
					},
				},
			},
			"required": []string{"edits"},
		},
	}
}

func (m *MultiEdit) Execute(ctx context.Context, args string) (string, error) {
	var input struct {
		Edits []EditOperation `json:"edits"`
	}
	if err := json.Unmarshal([]byte(args), &input); err != nil {
		return "", err
	}
	if len(input.Edits) == 0 {
		return "", fmt.Errorf("edits must contain at least one operation")
	}

	root, err := m.openRoot()
	if err != nil {
		return "", err
	}
	defer root.Close()

	contents := make(map[string]string)
	originals := make(map[string]string)
	for _, edit := range input.Edits {
		if err := validateEditStrings(edit.OldString, edit.NewString); err != nil {
			return "", fmt.Errorf("%s: %w", edit.FilePath, err)
		}
		relPath, err := m.relativePath(edit.FilePath)
		if err != nil {
			return "", err
		}

		if _, ok := contents[relPath]; !ok {
			content, err := root.ReadFile(relPath)
			if err != nil {
				return "", fmt.Errorf("failed to read %s: %w", edit.FilePath, err)
			}
			contents[relPath] = string(content)
			originals[relPath] = string(content)
		}

		strContent := contents[relPath]
		oldString, newString := matchEditStrings(strContent, edit.OldString, edit.NewString)
		_, err = replacementCount(
			edit.FilePath,
			strContent,
			oldString,
			edit.ReplaceAll,
			edit.Expected,
		)
		if err != nil {
			return "", err
		}

		if edit.ReplaceAll {
			contents[relPath] = strings.Replace(strContent, oldString, newString, -1)
		} else {
			contents[relPath] = strings.Replace(strContent, oldString, newString, 1)
		}
	}

	var diffs strings.Builder
	checkpointPaths := make([]string, 0, len(contents))
	for relPath := range contents {
		checkpointPaths = append(checkpointPaths, relPath)
	}
	checkpointID, err := m.checkpointPaths(ctx, checkpointPaths...)
	if err != nil {
		return "", err
	}

	type renameOp struct {
		from, to string
	}
	var renames []renameOp
	var writeErrs []error

	for relPath, content := range contents {
		tmpPath := relPath + ".tmp"
		if err := root.WriteFile(tmpPath, []byte(content), 0o644); err != nil {
			writeErrs = append(writeErrs, fmt.Errorf("failed to write temp %s: %w", tmpPath, err))
			break
		}
		renames = append(renames, renameOp{from: tmpPath, to: relPath})

		diff := udiff.Unified("a/"+relPath, "b/"+relPath, originals[relPath], content)
		if diff != "" {
			diffs.WriteString(diff)
			diffs.WriteString("\n")
		}
	}

	if len(writeErrs) > 0 {
		for _, op := range renames {
			_ = root.Remove(op.from)
		}
		return "", fmt.Errorf("multi_edit aborted: %v", writeErrs)
	}

	for _, op := range renames {
		if err := root.Rename(op.from, op.to); err != nil {
			return "", fmt.Errorf("failed to finalize %s: %w", op.to, err)
		}
	}

	return limitToolOutput(
		appendCheckpointID(
			fmt.Sprintf(
				"Successfully applied %d edit(s) across %d file(s)\n\n%s",
				len(input.Edits),
				len(contents),
				diffs.String(),
			),
			checkpointID,
		),
	), nil
}

func validateEditStrings(oldString, newString string) error {
	if oldString == "" {
		return fmt.Errorf("old_string must not be empty")
	}
	if oldString == newString {
		return fmt.Errorf("new_string must differ from old_string")
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
		return 0, fmt.Errorf("old_string not found in %s", filePath)
	}
	if expected > 0 && count != expected {
		return 0, fmt.Errorf(
			"old_string expected %d replacement(s) in %s, found %d%s",
			expected,
			filePath,
			count,
			lines,
		)
	}
	if !replaceAll && count > 1 {
		return 0, fmt.Errorf(
			"old_string is not unique in %s, found %d occurrences%s. Provide more context or set replace_all with expected_replacements.",
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
