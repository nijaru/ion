package canto

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/nijaru/canto/llm"
	"github.com/nijaru/canto/session"
)

// FileTagProcessor implements canto.context.RequestProcessor to resolve
// @file tags within user messages.
type FileTagProcessor struct {
	cwd string
}

func NewFileTagProcessor(cwd string) *FileTagProcessor {
	return &FileTagProcessor{cwd: cwd}
}

// ApplyRequest implements canto.context.RequestProcessor.
func (f *FileTagProcessor) ApplyRequest(
	ctx context.Context,
	p llm.Provider,
	model string,
	sess *session.Session,
	req *llm.Request,
) error {
	// Look at the last message if it's from the user
	if len(req.Messages) == 0 {
		return nil
	}
	last := &req.Messages[len(req.Messages)-1]
	if last.Role != llm.RoleUser {
		return nil
	}

	input := last.Content
	var resolved strings.Builder
	var filesAdded []string

	words := strings.Fields(input)
	for _, word := range words {
		if strings.HasPrefix(word, "@") && len(word) > 1 {
			filePath := word[1:]
			absPath, err := f.resolvePath(filePath)
			if err != nil {
				continue
			}

			content, err := os.ReadFile(absPath)
			if err == nil {
				resolved.WriteString(fmt.Sprintf("\n--- FILE: %s ---\n%s\n---\n", filePath, string(content)))
				filesAdded = append(filesAdded, filePath)
			}
		}
	}

	if len(filesAdded) > 0 {
		resolved.WriteString("\nUser Query: ")
		resolved.WriteString(input)
		last.Content = resolved.String()
	}

	return nil
}

func (f *FileTagProcessor) resolvePath(target string) (string, error) {
	absPath, err := filepath.Abs(target)
	if !filepath.IsAbs(target) {
		absPath, err = filepath.Abs(filepath.Join(f.cwd, target))
	}
	if err != nil {
		return "", err
	}

	absCwd, err := filepath.Abs(f.cwd)
	if err != nil {
		return "", err
	}

	if !strings.HasPrefix(absPath, absCwd+string(filepath.Separator)) && absPath != absCwd {
		return "", fmt.Errorf("path escapes workspace: %s", target)
	}
	return absPath, nil
}
