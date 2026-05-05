package tools

import (
	"bufio"
	"context"
	"fmt"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"

	"github.com/bmatcuk/doublestar/v4"
	"github.com/go-json-experiment/json"
	"github.com/nijaru/canto/llm"
)

type SearchTool struct {
	cwd string
}

func NewSearchTool(cwd string) *SearchTool {
	return &SearchTool{cwd: cwd}
}

func (t *SearchTool) searchArg(target string) (string, error) {
	if target == "" {
		target = "."
	}
	absCwd, err := filepath.Abs(t.cwd)
	if err != nil {
		return "", err
	}
	candidate := target
	if !filepath.IsAbs(candidate) {
		candidate = filepath.Join(absCwd, candidate)
	}
	absPath, err := filepath.Abs(candidate)
	if err != nil {
		return "", err
	}
	if !strings.HasPrefix(absPath, absCwd+string(filepath.Separator)) && absPath != absCwd {
		return "", fmt.Errorf("path escapes workspace: %s", target)
	}
	if err := t.rejectSymlinkEscape(absCwd, absPath, target); err != nil {
		return "", err
	}
	if absPath == absCwd {
		return "", nil
	}
	relPath, err := filepath.Rel(absCwd, absPath)
	if err != nil {
		return "", err
	}
	return relPath, nil
}

func (t *SearchTool) rejectSymlinkEscape(absCwd, absPath, target string) error {
	realCwd, err := filepath.EvalSymlinks(absCwd)
	if err != nil {
		realCwd = absCwd
	}
	realPath, err := filepath.EvalSymlinks(absPath)
	if err != nil {
		return fmt.Errorf("path not found: %s", target)
	}
	if realPath != realCwd &&
		!strings.HasPrefix(realPath, realCwd+string(filepath.Separator)) {
		return fmt.Errorf("path escapes workspace: %s", target)
	}
	return nil
}

func validateGlobPattern(pattern string) error {
	if strings.TrimSpace(pattern) == "" {
		return fmt.Errorf("pattern is required")
	}
	if filepath.IsAbs(pattern) {
		return fmt.Errorf("pattern escapes workspace: %s", pattern)
	}
	for _, part := range strings.FieldsFunc(pattern, func(r rune) bool {
		return r == '/' || r == '\\'
	}) {
		if part == ".." {
			return fmt.Errorf("pattern escapes workspace: %s", pattern)
		}
	}
	return nil
}

// Grep tool
type Grep struct {
	SearchTool
}

func (g *Grep) Spec() llm.Spec {
	return llm.Spec{
		Name:        "grep",
		Description: "Search file contents with ripgrep. Respects ignore files, includes hidden files, and excludes .git internals.",
		Parameters: map[string]any{
			"type": "object",
			"properties": map[string]any{
				"pattern": map[string]any{
					"type":        "string",
					"description": "The regex pattern to search for.",
				},
				"path": map[string]any{
					"type":        "string",
					"description": "The directory or file to search in (defaults to current working directory).",
				},
			},
			"required": []string{"pattern"},
		},
	}
}

func (g *Grep) Execute(ctx context.Context, args string) (string, error) {
	var input struct {
		Pattern string `json:"pattern"`
		Path    string `json:"path"`
	}
	if err := json.Unmarshal([]byte(args), &input); err != nil {
		return "", err
	}
	if strings.TrimSpace(input.Pattern) == "" {
		return "", fmt.Errorf("pattern is required")
	}

	searchArg, err := g.searchArg(input.Path)
	if err != nil {
		return "", err
	}

	cmdArgs := []string{
		"--max-count", "100",
		"--heading",
		"--line-number",
		"--color", "never",
		"--hidden",
		"--no-require-git",
		"--glob", "!.git/**",
		"--",
		input.Pattern,
	}
	if searchArg != "" {
		cmdArgs = append(cmdArgs, searchArg)
	}
	cmd := exec.CommandContext(ctx, "rg", cmdArgs...)
	cmd.Dir = g.cwd
	output, err := cmd.CombinedOutput()
	if err == nil {
		return limitToolOutput(string(output)), nil
	}
	if exitErr, ok := err.(*exec.ExitError); ok && exitErr.ExitCode() == 1 {
		return "No matches found.", nil
	}
	return "", fmt.Errorf("rg search failed: %w", err)
}

// Glob tool
type Glob struct {
	SearchTool
}

func (g *Glob) Spec() llm.Spec {
	return llm.Spec{
		Name:        "glob",
		Description: "Find files matching a glob pattern using ripgrep's ignored-file list. Respects ignore files, includes hidden files, excludes .git internals, and supports ** for recursive search.",
		Parameters: map[string]any{
			"type": "object",
			"properties": map[string]any{
				"pattern": map[string]any{
					"type":        "string",
					"description": "The glob pattern to search for (e.g. '**/*.go').",
				},
			},
			"required": []string{"pattern"},
		},
	}
}

func (g *Glob) Execute(ctx context.Context, args string) (string, error) {
	var input struct {
		Pattern string `json:"pattern"`
	}
	if err := json.Unmarshal([]byte(args), &input); err != nil {
		return "", err
	}
	if err := validateGlobPattern(input.Pattern); err != nil {
		return "", err
	}

	cmd := exec.CommandContext(
		ctx,
		"rg",
		"--files",
		"--hidden",
		"--no-require-git",
		"--glob", "!.git/**",
	)
	cmd.Dir = g.cwd
	output, err := cmd.Output()
	if err != nil {
		if exitErr, ok := err.(*exec.ExitError); ok && exitErr.ExitCode() == 1 {
			return "No matches found.", nil
		}
		if strings.TrimSpace(string(output)) == "" {
			return "", fmt.Errorf("rg file listing failed: %w", err)
		}
	}

	matches, err := globMatches(input.Pattern, string(output))
	if err != nil {
		return "", fmt.Errorf("glob failed: %w", err)
	}

	if len(matches) == 0 {
		return "No matches found.", nil
	}

	slices.Sort(matches)
	return limitToolOutput(strings.Join(matches, "\n")), nil
}

func globMatches(pattern, output string) ([]string, error) {
	var matches []string
	scanner := bufio.NewScanner(strings.NewReader(output))
	for scanner.Scan() {
		path := filepath.ToSlash(strings.TrimSpace(scanner.Text()))
		if path == "" {
			continue
		}
		path = strings.TrimPrefix(path, "./")
		matched, err := doublestar.Match(pattern, path)
		if err != nil {
			return nil, err
		}
		if matched {
			matches = append(matches, path)
		}
	}
	if err := scanner.Err(); err != nil {
		return nil, err
	}
	return matches, nil
}
