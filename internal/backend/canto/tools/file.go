package tools

import (
	"context"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"

	"github.com/aymanbagabas/go-udiff"
	"github.com/go-json-experiment/json"
	"github.com/nijaru/canto/llm"
	ionworkspace "github.com/nijaru/ion/internal/workspace"
)

type FileTool struct {
	cwd        string
	checkpoint *ionworkspace.CheckpointStore
}

func NewFileTool(cwd string) *FileTool {
	path, err := ionworkspace.DefaultCheckpointPath()
	if err != nil {
		return &FileTool{cwd: cwd}
	}
	return &FileTool{cwd: cwd, checkpoint: ionworkspace.NewCheckpointStore(path)}
}

func (t *FileTool) openRoot() (*os.Root, error) {
	absCwd, err := filepath.Abs(t.cwd)
	if err != nil {
		return nil, err
	}
	return os.OpenRoot(absCwd)
}

func (t *FileTool) relativePath(target string) (string, error) {
	target = strings.TrimSpace(target)
	if target == "" {
		return "", fmt.Errorf("path is required")
	}
	absCwd, err := filepath.Abs(t.cwd)
	if err != nil {
		return "", err
	}

	if filepath.IsAbs(target) {
		absPath, err := filepath.Abs(target)
		if err != nil {
			return "", err
		}
		target, err = filepath.Rel(absCwd, absPath)
		if err != nil {
			return "", err
		}
	}

	target = filepath.Clean(target)
	if !filepath.IsLocal(target) {
		return "", fmt.Errorf("path escapes workspace: %s", target)
	}
	return target, nil
}

// resolvePath returns the lexical absolute path for display/diff metadata only.
// File operations use os.Root methods so symlinks cannot escape the workspace.
func (t *FileTool) resolvePath(target string) (string, error) {
	relPath, err := t.relativePath(target)
	if err != nil {
		return "", err
	}
	absCwd, err := filepath.Abs(t.cwd)
	if err != nil {
		return "", err
	}
	return filepath.Join(absCwd, relPath), nil
}

func (t *FileTool) checkpointPaths(ctx context.Context, paths ...string) (string, error) {
	if t.checkpoint == nil {
		return "", nil
	}
	relPaths := make([]string, 0, len(paths))
	for _, path := range paths {
		relPath, err := t.relativePath(path)
		if err != nil {
			return "", err
		}
		relPaths = append(relPaths, relPath)
	}
	cp, err := t.checkpoint.Create(ctx, t.cwd, relPaths)
	if err != nil {
		return "", err
	}
	return cp.ID, nil
}

// Read tool (formerly read_file)
type Read struct {
	FileTool
}

func (r *Read) Spec() llm.Spec {
	return llm.Spec{
		Name:        "read",
		Description: "Read file contents. Returns the full file or a specific line range (use offset/limit for large files).",
		Parameters: map[string]any{
			"type": "object",
			"properties": map[string]any{
				"file_path": map[string]any{
					"type":        "string",
					"description": "Relative path to the file from the current directory.",
				},
				"offset": map[string]any{
					"type":        "integer",
					"description": "Line number to start reading from (0-indexed).",
				},
				"limit": map[string]any{
					"type":        "integer",
					"description": "Maximum number of lines to read.",
				},
			},
			"required": []string{"file_path"},
		},
	}
}

func (r *Read) Execute(ctx context.Context, args string) (string, error) {
	var input struct {
		FilePath string `json:"file_path"`
		Offset   int    `json:"offset"`
		Limit    int    `json:"limit"`
	}
	if err := json.Unmarshal([]byte(args), &input); err != nil {
		return "", err
	}
	if input.Offset < 0 {
		return "", fmt.Errorf("offset must be non-negative")
	}
	if input.Limit < 0 {
		return "", fmt.Errorf("limit must be non-negative")
	}

	relPath, err := r.relativePath(input.FilePath)
	if err != nil {
		return "", err
	}
	root, err := r.openRoot()
	if err != nil {
		return "", err
	}
	defer root.Close()

	content, err := root.ReadFile(relPath)
	if err != nil {
		return "", err
	}

	lines := strings.Split(string(content), "\n")
	if input.Limit > 0 {
		end := input.Offset + input.Limit
		if end > len(lines) {
			end = len(lines)
		}
		if input.Offset < len(lines) {
			return strings.Join(lines[input.Offset:end], "\n"), nil
		}
		return "", nil
	}

	return string(content), nil
}

// Write tool (formerly write_file)
type Write struct {
	FileTool
}

func (w *Write) Spec() llm.Spec {
	return llm.Spec{
		Name:        "write",
		Description: "Create or overwrite a file with new content. Use for new files or complete rewrites.",
		Parameters: map[string]any{
			"type": "object",
			"properties": map[string]any{
				"file_path": map[string]any{
					"type":        "string",
					"description": "Relative path to the file.",
				},
				"content": map[string]any{
					"type":        "string",
					"description": "The full content to write to the file.",
				},
			},
			"required": []string{"file_path", "content"},
		},
	}
}

func (w *Write) Execute(ctx context.Context, args string) (string, error) {
	var input struct {
		FilePath string `json:"file_path"`
		Content  string `json:"content"`
	}
	if err := json.Unmarshal([]byte(args), &input); err != nil {
		return "", err
	}

	relPath, err := w.relativePath(input.FilePath)
	if err != nil {
		return "", err
	}
	root, err := w.openRoot()
	if err != nil {
		return "", err
	}
	defer root.Close()

	checkpointID, err := w.checkpointPaths(ctx, input.FilePath)
	if err != nil {
		return "", err
	}

	if err := root.MkdirAll(filepath.Dir(relPath), 0755); err != nil {
		return "", err
	}

	if err := root.WriteFile(relPath, []byte(input.Content), 0644); err != nil {
		return "", err
	}
	return appendCheckpointID(fmt.Sprintf("Successfully wrote %d bytes to %s", len(input.Content), input.FilePath), checkpointID), nil
}

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
	count := strings.Count(strContent, input.OldString)
	if count == 0 {
		return "", fmt.Errorf("old_string not found in file")
	}

	if !input.ReplaceAll && count > 1 {
		return "", fmt.Errorf("old_string is not unique in file, found %d occurrences. Use replace_all: true to replace all.", count)
	}

	var newContent string
	if input.ReplaceAll {
		newContent = strings.Replace(strContent, input.OldString, input.NewString, -1)
	} else {
		newContent = strings.Replace(strContent, input.OldString, input.NewString, 1)
	}

	checkpointID, err := e.checkpointPaths(ctx, input.FilePath)
	if err != nil {
		return "", err
	}

	if err := root.WriteFile(relPath, []byte(newContent), 0644); err != nil {
		return "", err
	}

	diff := udiff.Unified("a/"+input.FilePath, "b/"+input.FilePath, strContent, newContent)
	return appendCheckpointID(fmt.Sprintf("Successfully replaced %d occurrence(s) in %s\n\n%s", count, input.FilePath, diff), checkpointID), nil
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

	// First pass: validate all edits and track original content
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

		// Load file if not already loaded
		if _, ok := contents[relPath]; !ok {
			content, err := root.ReadFile(relPath)
			if err != nil {
				return "", fmt.Errorf("failed to read %s: %w", edit.FilePath, err)
			}
			contents[relPath] = string(content)
			originals[relPath] = string(content)
		}

		strContent := contents[relPath]
		count := strings.Count(strContent, edit.OldString)
		if count == 0 {
			return "", fmt.Errorf("old_string not found in %s", edit.FilePath)
		}

		if !edit.ReplaceAll && count > 1 {
			return "", fmt.Errorf("old_string is not unique in %s, found %d occurrences. Provide more context.", edit.FilePath, count)
		}

		// Apply edit to our in-memory copy
		if edit.ReplaceAll {
			contents[relPath] = strings.Replace(strContent, edit.OldString, edit.NewString, -1)
		} else {
			contents[relPath] = strings.Replace(strContent, edit.OldString, edit.NewString, 1)
		}
	}

	// Second pass: write all modified files to temp files and generate aggregate diff
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
		if err := root.WriteFile(tmpPath, []byte(content), 0644); err != nil {
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

	// Clean up temp files if any write failed
	if len(writeErrs) > 0 {
		for _, op := range renames {
			_ = root.Remove(op.from)
		}
		return "", fmt.Errorf("multi_edit aborted: %v", writeErrs)
	}

	// Final pass: atomic renames
	for _, op := range renames {
		if err := root.Rename(op.from, op.to); err != nil {
			return "", fmt.Errorf("failed to finalize %s: %w", op.to, err)
		}
	}

	return appendCheckpointID(fmt.Sprintf("Successfully applied %d edit(s) across %d file(s)\n\n%s", len(input.Edits), len(contents), diffs.String()), checkpointID), nil
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

func appendCheckpointID(message, id string) string {
	if id == "" {
		return message
	}
	return message + "\nCheckpoint: " + id
}

// List tool (formerly list_directory)
type List struct {
	FileTool
}

func (l *List) Spec() llm.Spec {
	return llm.Spec{
		Name:        "list",
		Description: "List contents of a specific directory.",
		Parameters: map[string]any{
			"type": "object",
			"properties": map[string]any{
				"path": map[string]any{
					"type":        "string",
					"description": "Directory to list (default: current directory).",
				},
			},
		},
	}
}

func (l *List) Execute(ctx context.Context, args string) (string, error) {
	var input struct {
		Path string `json:"path"`
	}
	if err := json.Unmarshal([]byte(args), &input); err != nil {
		return "", err
	}
	if input.Path == "" {
		input.Path = "."
	}

	relPath, err := l.relativePath(input.Path)
	if err != nil {
		return "", err
	}
	root, err := l.openRoot()
	if err != nil {
		return "", err
	}
	defer root.Close()

	entries, err := fs.ReadDir(root.FS(), relPath)
	if err != nil {
		return "", err
	}

	var res strings.Builder
	for _, e := range entries {
		suffix := ""
		if e.IsDir() {
			suffix = "/"
		}
		res.WriteString(fmt.Sprintf("%s%s\n", e.Name(), suffix))
	}
	return res.String(), nil
}
