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
	"github.com/nijaru/canto/llm"
)

const (
	defaultGrepLimit = 100
	defaultGlobLimit = 1000
)

type SearchTool struct {
	cwd string
}

func NewSearchTool(cwd string) *SearchTool {
	return &SearchTool{cwd: cwd}
}

func (t *SearchTool) searchArg(target string) (string, error) {
	target = strings.TrimSpace(target)
	if target == "" || target == "." {
		target = "."
	}
	absCwd, err := filepath.Abs(t.cwd)
	if err != nil {
		return "", err
	}
	target, err = expandHomePath(target)
	if err != nil {
		return "", err
	}
	absPath := target
	if !filepath.IsAbs(absPath) {
		absPath = filepath.Join(absCwd, absPath)
	}
	absPath = filepath.Clean(absPath)
	if absPath == absCwd {
		return "", nil
	}
	relPath, err := filepath.Rel(absCwd, absPath)
	if err == nil && filepath.IsLocal(relPath) {
		return relPath, nil
	}
	return absPath, nil
}

func (t *SearchTool) globPatternArg(pattern string) (string, error) {
	pattern = strings.TrimSpace(pattern)
	if pattern == "" {
		return "", fmt.Errorf("pattern is required")
	}
	pattern, err := expandHomePath(pattern)
	if err != nil {
		return "", err
	}
	absCwd, err := filepath.Abs(t.cwd)
	if err != nil {
		return "", err
	}
	if filepath.IsAbs(pattern) {
		relPath, err := filepath.Rel(absCwd, filepath.Clean(pattern))
		if err == nil && (relPath == "." || filepath.IsLocal(relPath)) {
			pattern = relPath
		} else {
			return "", fmt.Errorf("pattern escapes workspace: %s", pattern)
		}
	}
	pattern = filepath.ToSlash(pattern)
	if err := validateGlobPattern(pattern); err != nil {
		return "", err
	}
	return pattern, nil
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
		Parameters:  grepParameters(),
	}
}

func (g *Grep) Execute(ctx context.Context, args string) (string, error) {
	input, err := decodeToolArgs[grepInput]("grep", args)
	if err != nil {
		return "", err
	}
	if strings.TrimSpace(input.Pattern) == "" {
		return "", fmt.Errorf("pattern is required")
	}
	if input.Context < 0 {
		return "", fmt.Errorf("context must be non-negative")
	}
	limit := input.Limit
	if limit <= 0 {
		limit = defaultGrepLimit
	}

	searchArg, err := g.searchArg(input.Path)
	if err != nil {
		return "", err
	}

	cmdArgs := []string{
		"--line-number",
		"--color", "never",
		"--hidden",
		"--no-require-git",
		"--glob", "!.git/**",
	}
	if input.IgnoreCase {
		cmdArgs = append(cmdArgs, "--ignore-case")
	}
	if input.Literal {
		cmdArgs = append(cmdArgs, "--fixed-strings")
	}
	if input.Context > 0 {
		cmdArgs = append(cmdArgs, "--context", fmt.Sprint(input.Context))
	}
	if strings.TrimSpace(input.Glob) != "" {
		cmdArgs = append(cmdArgs, "--glob", input.Glob)
	}
	cmdArgs = append(cmdArgs, "--", input.Pattern)
	if searchArg != "" {
		cmdArgs = append(cmdArgs, searchArg)
	}
	cmd := exec.CommandContext(ctx, "rg", cmdArgs...)
	cmd.Dir = g.cwd
	output, err := cmd.CombinedOutput()
	if err == nil {
		return limitToolOutput(limitSearchLines(string(output), limit, "matches")), nil
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
		Parameters:  globParameters(),
	}
}

func (g *Glob) Execute(ctx context.Context, args string) (string, error) {
	input, err := decodeToolArgs[globInput]("glob", args)
	if err != nil {
		return "", err
	}
	pattern, err := g.globPatternArg(input.Pattern)
	if err != nil {
		return "", err
	}
	limit := input.Limit
	if limit <= 0 {
		limit = defaultGlobLimit
	}
	searchArg, err := g.searchArg(input.Path)
	if err != nil {
		return "", err
	}

	cmdArgs := []string{
		"--files",
		"--hidden",
		"--no-require-git",
		"--glob", "!.git/**",
	}
	if searchArg != "" {
		cmdArgs = append(cmdArgs, searchArg)
	}
	cmd := exec.CommandContext(ctx, "rg", cmdArgs...)
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

	matches, err := globMatches(pattern, string(output), searchArg)
	if err != nil {
		return "", fmt.Errorf("glob failed: %w", err)
	}

	if len(matches) == 0 {
		return "No matches found.", nil
	}

	slices.Sort(matches)
	return limitToolOutput(joinLimitedMatches(matches, limit)), nil
}

func globMatches(pattern, output, searchArg string) ([]string, error) {
	matchPattern := effectiveGlobPattern(pattern)
	var matches []string
	scanner := bufio.NewScanner(strings.NewReader(output))
	for scanner.Scan() {
		path := filepath.ToSlash(strings.TrimSpace(scanner.Text()))
		if path == "" {
			continue
		}
		path = strings.TrimPrefix(path, "./")
		path = searchRelativePath(path, searchArg)
		matched, err := doublestar.Match(matchPattern, path)
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

func effectiveGlobPattern(pattern string) string {
	if strings.Contains(pattern, "/") &&
		!strings.HasPrefix(pattern, "/") &&
		!strings.HasPrefix(pattern, "**/") &&
		pattern != "**" {
		return "**/" + pattern
	}
	return pattern
}

func searchRelativePath(path, searchArg string) string {
	searchArg = strings.TrimPrefix(filepath.ToSlash(strings.TrimSpace(searchArg)), "./")
	if searchArg == "" || searchArg == "." {
		return path
	}
	if filepath.IsAbs(searchArg) {
		rel, err := filepath.Rel(filepath.FromSlash(searchArg), filepath.FromSlash(path))
		if err == nil && filepath.IsLocal(rel) {
			return filepath.ToSlash(rel)
		}
	}
	if path == searchArg {
		return filepath.Base(path)
	}
	prefix := strings.TrimSuffix(searchArg, "/") + "/"
	return strings.TrimPrefix(path, prefix)
}

func joinLimitedMatches(matches []string, limit int) string {
	if limit <= 0 || len(matches) <= limit {
		return strings.Join(matches, "\n")
	}
	output := strings.Join(matches[:limit], "\n")
	return fmt.Sprintf(
		"%s\n\n[%d results limit reached. Use limit=%d for more, or refine pattern.]",
		output,
		limit,
		limit*2,
	)
}

func limitSearchLines(output string, limit int, label string) string {
	if limit <= 0 {
		return output
	}
	output = strings.TrimRight(output, "\n")
	if output == "" {
		return output
	}
	lines := strings.Split(output, "\n")
	if len(lines) <= limit {
		return output
	}
	return fmt.Sprintf(
		"%s\n\n[%d %s limit reached. Use limit=%d for more, or refine pattern.]",
		strings.Join(lines[:limit], "\n"),
		limit,
		label,
		limit*2,
	)
}
