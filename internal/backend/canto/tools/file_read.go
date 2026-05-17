package tools

import (
	"context"
	"fmt"
	"strings"

	"github.com/go-json-experiment/json"
	"github.com/nijaru/canto/llm"
)

// Read tool (formerly read_file)
type Read struct {
	FileTool
}

func (r *Read) Spec() llm.Spec {
	return llm.Spec{
		Name:        "read",
		Description: "Read file contents with line numbers. Returns the full file or a specific line range (use offset/limit for large files).",
		Parameters: map[string]any{
			"type": "object",
			"properties": map[string]any{
				"file_path": map[string]any{
					"type":        "string",
					"description": "Relative path to the file from the current directory.",
				},
				"offset": map[string]any{
					"type":        "integer",
					"description": "Line number to start reading from (1-indexed).",
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

	output, err := numberedReadOutput(string(content), input.Offset, input.Limit)
	if err != nil {
		return "", err
	}
	return limitToolOutput(output), nil
}

func numberedReadOutput(content string, offset, limit int) (string, error) {
	if content == "" {
		return "", nil
	}

	lines := strings.Split(content, "\n")
	if strings.HasSuffix(content, "\n") && len(lines) > 0 {
		lines = lines[:len(lines)-1]
	}
	start := 0
	if offset > 0 {
		start = offset - 1
	}
	if start >= len(lines) {
		return "", fmt.Errorf("offset %d is beyond end of file (%d lines total)", offset, len(lines))
	}

	end := len(lines)
	if limit > 0 {
		end = min(start+limit, len(lines))
	}
	output := numberedLines(lines, start, end)
	if limit > 0 && end < len(lines) {
		remaining := len(lines) - end
		output += fmt.Sprintf(
			"\n\n[%d more line(s) in file. Use offset=%d to continue.]",
			remaining,
			end+1,
		)
	}
	return output, nil
}

func numberedLines(lines []string, start, end int) string {
	var b strings.Builder
	for i, line := range lines[start:end] {
		if i > 0 {
			b.WriteByte('\n')
		}
		if start+i == 0 {
			line = strings.TrimPrefix(line, "\ufeff")
		}
		line = strings.TrimSuffix(line, "\r")
		fmt.Fprintf(&b, "%6d\t%s", start+i+1, line)
	}
	return b.String()
}
