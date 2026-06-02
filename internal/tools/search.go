package tools

import (
	"bufio"
	"context"
	stdjson "encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"unicode/utf8"

	"github.com/bmatcuk/doublestar/v4"
	"github.com/nijaru/ion/llm"
)

const (
	defaultGrepLimit = 100
	defaultFindLimit = 1000
	grepMaxLineChars = 500
)

type SearchTool struct {
	cwd string
}

func NewSearchTool(cwd string) *SearchTool {
	return &SearchTool{cwd: cwd}
}

func (t *SearchTool) searchArg(target string) (string, error) {
	target, err := normalizeToolPathInput(target)
	if err != nil {
		return "", err
	}
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

func (t *SearchTool) commandSearchPath(searchArg string) (string, error) {
	absCwd, err := filepath.Abs(t.cwd)
	if err != nil {
		return "", err
	}
	if strings.TrimSpace(searchArg) == "" {
		return absCwd, nil
	}
	if filepath.IsAbs(searchArg) {
		return filepath.Clean(searchArg), nil
	}
	return filepath.Clean(filepath.Join(absCwd, filepath.FromSlash(searchArg))), nil
}

func (t *SearchTool) globPatternArg(pattern string) (string, error) {
	pattern, err := normalizeToolPathInput(pattern)
	if err != nil {
		return "", err
	}
	if pattern == "" {
		return "", fmt.Errorf("pattern is required")
	}
	pattern, err = expandHomePath(pattern)
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

type rgJSONEvent struct {
	Type string `json:"type"`
	Data struct {
		Path *struct {
			Text string `json:"text"`
		} `json:"path"`
		LineNumber int `json:"line_number"`
		Lines      *struct {
			Text string `json:"text"`
		} `json:"lines"`
	} `json:"data"`
}

type grepMatch struct {
	filePath   string
	lineNumber int
	lineText   string
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
	searchPath, err := g.commandSearchPath(searchArg)
	if err != nil {
		return "", err
	}
	info, err := os.Stat(searchPath)
	if err != nil {
		if os.IsNotExist(err) {
			return "", fmt.Errorf("path not found: %s", searchPath)
		}
		return "", err
	}
	isDirectory := info.IsDir()

	cmdArgs := []string{
		"--json",
		"--line-number",
		"--color=never",
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
	if strings.TrimSpace(input.Glob) != "" {
		cmdArgs = append(cmdArgs, "--glob", input.Glob)
	}
	cmdArgs = append(cmdArgs, "--", input.Pattern)
	if searchArg != "" {
		cmdArgs = append(cmdArgs, searchArg)
	}
	cmd := exec.CommandContext(ctx, "rg", cmdArgs...)
	cmd.Dir = g.cwd
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return "", fmt.Errorf("rg stdout: %w", err)
	}
	stderrPipe, err := cmd.StderrPipe()
	if err != nil {
		return "", fmt.Errorf("rg stderr: %w", err)
	}
	if err := cmd.Start(); err != nil {
		return "", fmt.Errorf("rg search failed: %w", err)
	}

	var stderr strings.Builder
	var stderrWG sync.WaitGroup
	stderrWG.Add(1)
	go func() {
		defer stderrWG.Done()
		_, _ = io.Copy(&stderr, stderrPipe)
	}()

	reader := bufio.NewReader(stdout)
	matches := make([]grepMatch, 0, min(limit, defaultGrepLimit))
	matchLimitReached := false
	killedDueToLimit := false
	for {
		line, readErr := reader.ReadString('\n')
		if strings.TrimSpace(line) != "" && len(matches) < limit {
			var event rgJSONEvent
			if err := stdjson.Unmarshal([]byte(line), &event); err == nil && event.Type == "match" &&
				event.Data.Path != nil &&
				event.Data.LineNumber > 0 {
				match := grepMatch{
					filePath:   event.Data.Path.Text,
					lineNumber: event.Data.LineNumber,
				}
				if event.Data.Lines != nil {
					match.lineText = event.Data.Lines.Text
				}
				matches = append(matches, match)
				if len(matches) >= limit {
					matchLimitReached = true
					killedDueToLimit = true
					if cmd.Process != nil {
						_ = cmd.Process.Kill()
					}
				}
			}
		}
		if readErr != nil {
			if !errors.Is(readErr, io.EOF) {
				_ = cmd.Wait()
				stderrWG.Wait()
				return "", fmt.Errorf("read rg output: %w", readErr)
			}
			break
		}
	}
	waitErr := cmd.Wait()
	stderrWG.Wait()
	if err := ctx.Err(); err != nil {
		return "", toolContextErr("search", err)
	}
	if waitErr != nil && !killedDueToLimit {
		if exitErr, ok := waitErr.(*exec.ExitError); ok && exitErr.ExitCode() == 1 {
			return "No matches found", nil
		}
		errorMsg := strings.TrimSpace(stderr.String())
		if errorMsg == "" {
			errorMsg = waitErr.Error()
		}
		return "", fmt.Errorf("rg search failed: %s", errorMsg)
	}
	if len(matches) == 0 {
		return "No matches found", nil
	}

	output, linesTruncated := g.formatGrepMatches(matches, searchPath, isDirectory, input.Context)
	output, byteTruncated := truncateToolOutputHead(output, maxToolOutputSize)
	var notices []string
	if matchLimitReached {
		notices = append(
			notices,
			fmt.Sprintf(
				"%d matches limit reached. Use limit=%d for more, or refine pattern",
				limit,
				limit*2,
			),
		)
	}
	if byteTruncated {
		notices = append(
			notices,
			fmt.Sprintf("%s limit reached", toolOutputLimitLabel(maxToolOutputSize)),
		)
	}
	if linesTruncated {
		notices = append(
			notices,
			fmt.Sprintf(
				"Some lines truncated to %d chars. Use read tool to see full lines",
				grepMaxLineChars,
			),
		)
	}
	if len(notices) > 0 {
		output += "\n\n[" + strings.Join(notices, ". ") + "]"
	}
	return output, nil
}

func (g *Grep) formatGrepMatches(
	matches []grepMatch,
	searchPath string,
	isDirectory bool,
	contextLines int,
) (string, bool) {
	fileCache := map[string][]string{}
	var output []string
	linesTruncated := false
	for _, match := range matches {
		path := g.grepDisplayPath(match.filePath, searchPath, isDirectory)
		if contextLines <= 0 && match.lineText != "" {
			lineText := strings.TrimSuffix(strings.ReplaceAll(match.lineText, "\r", ""), "\n")
			truncated, ok := truncateGrepLine(lineText)
			linesTruncated = linesTruncated || ok
			output = append(output, fmt.Sprintf("%s:%d: %s", path, match.lineNumber, truncated))
			continue
		}
		filePath := g.grepAbsolutePath(match.filePath)
		lines, ok := fileCache[filePath]
		if !ok {
			content, err := os.ReadFile(filePath)
			if err != nil {
				output = append(
					output,
					fmt.Sprintf("%s:%d: (unable to read file)", path, match.lineNumber),
				)
				continue
			}
			lines = strings.Split(
				strings.ReplaceAll(strings.ReplaceAll(string(content), "\r\n", "\n"), "\r", "\n"),
				"\n",
			)
			fileCache[filePath] = lines
		}
		start := match.lineNumber
		end := match.lineNumber
		if contextLines > 0 {
			start = max(1, match.lineNumber-contextLines)
			end = min(len(lines), match.lineNumber+contextLines)
		}
		for lineNumber := start; lineNumber <= end; lineNumber++ {
			lineText := ""
			if lineNumber-1 >= 0 && lineNumber-1 < len(lines) {
				lineText = lines[lineNumber-1]
			}
			truncated, ok := truncateGrepLine(lineText)
			linesTruncated = linesTruncated || ok
			if lineNumber == match.lineNumber {
				output = append(output, fmt.Sprintf("%s:%d: %s", path, lineNumber, truncated))
			} else {
				output = append(output, fmt.Sprintf("%s-%d- %s", path, lineNumber, truncated))
			}
		}
	}
	return strings.Join(output, "\n"), linesTruncated
}

func (g *Grep) grepDisplayPath(filePath, searchPath string, isDirectory bool) string {
	absPath := g.grepAbsolutePath(filePath)
	if isDirectory {
		if rel, err := filepath.Rel(searchPath, absPath); err == nil && filepath.IsLocal(rel) {
			return filepath.ToSlash(rel)
		}
	}
	return filepath.Base(absPath)
}

func (g *Grep) grepAbsolutePath(filePath string) string {
	if filepath.IsAbs(filePath) {
		return filepath.Clean(filePath)
	}
	return filepath.Clean(filepath.Join(g.cwd, filepath.FromSlash(filePath)))
}

func truncateGrepLine(line string) (string, bool) {
	if utf8.RuneCountInString(line) <= grepMaxLineChars {
		return line, false
	}
	runes := []rune(line)
	return string(runes[:grepMaxLineChars]) + "... [truncated]", true
}

// Find tool
type Find struct {
	SearchTool
}

func (f *Find) Spec() llm.Spec {
	return llm.Spec{
		Name:        "find",
		Description: "Find files matching a glob pattern using ripgrep's ignored-file list. Respects ignore files, includes hidden files, excludes .git internals, and supports ** for recursive search.",
		Parameters:  findParameters(),
	}
}

func (f *Find) Execute(ctx context.Context, args string) (string, error) {
	input, err := decodeToolArgs[findInput]("find", args)
	if err != nil {
		return "", err
	}
	pattern, err := f.globPatternArg(input.Pattern)
	if err != nil {
		return "", err
	}
	limit := input.Limit
	if limit <= 0 {
		limit = defaultFindLimit
	}
	searchArg, err := f.searchArg(input.Path)
	if err != nil {
		return "", err
	}
	searchPath, err := f.commandSearchPath(searchArg)
	if err != nil {
		return "", err
	}
	info, err := os.Stat(searchPath)
	if err != nil {
		if os.IsNotExist(err) {
			return "", fmt.Errorf("path not found: %s", searchPath)
		}
		return "", err
	}
	if !info.IsDir() {
		return "", fmt.Errorf("not a directory: %s", searchPath)
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
	cmd.Dir = f.cwd
	output, err := cmd.Output()
	if err != nil {
		if exitErr, ok := err.(*exec.ExitError); ok && exitErr.ExitCode() == 1 {
			return "No files found matching pattern", nil
		}
		if strings.TrimSpace(string(output)) == "" {
			return "", fmt.Errorf("rg file listing failed: %w", err)
		}
	}

	matches, err := globMatches(pattern, string(output), searchArg)
	if err != nil {
		return "", fmt.Errorf("find failed: %w", err)
	}

	if len(matches) == 0 {
		return "No files found matching pattern", nil
	}

	slices.Sort(matches)
	return formatLimitedFindMatches(matches, limit), nil
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
		displayPath := searchRelativePath(path, searchArg)
		var matched bool
		var err error
		if !strings.Contains(pattern, "/") {
			matched, err = doublestar.Match(pattern, filepath.Base(displayPath))
		} else {
			matched, err = doublestar.Match(matchPattern, displayPath)
			if err != nil {
				return nil, err
			}
			if !matched {
				matched, err = doublestar.Match(matchPattern, path)
			}
		}
		if err != nil {
			return nil, err
		}
		if matched {
			matches = append(matches, displayPath)
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

func formatLimitedFindMatches(matches []string, limit int) string {
	limited := matches
	resultLimitReached := false
	if limit > 0 && len(matches) > limit {
		limited = matches[:limit]
		resultLimitReached = true
	}
	output := strings.Join(limited, "\n")
	output, byteTruncated := truncateToolOutputHead(output, maxToolOutputSize)
	var notices []string
	if resultLimitReached {
		notices = append(
			notices,
			fmt.Sprintf(
				"%d results limit reached. Use limit=%d for more, or refine pattern",
				limit,
				limit*2,
			),
		)
	}
	if byteTruncated {
		notices = append(
			notices,
			fmt.Sprintf("%s limit reached", toolOutputLimitLabel(maxToolOutputSize)),
		)
	}
	if len(notices) > 0 {
		output += "\n\n[" + strings.Join(notices, ". ") + "]"
	}
	return output
}
