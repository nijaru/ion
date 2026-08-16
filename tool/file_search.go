package tool

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"unicode/utf8"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ripgo"
	findpkg "github.com/nijaru/ripgo/find"
	"github.com/nijaru/ripgo/search"
)

const (
	defaultGrepLimit = 100
	defaultFindLimit = 1000
	grepMaxLineChars = 500
)

type fileSearchBase struct {
	cwd string
}

func newFileSearchBase(cwd string) *fileSearchBase {
	return &fileSearchBase{cwd: cwd}
}

func (t *fileSearchBase) searchArg(target string) (string, error) {
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

func (t *fileSearchBase) commandSearchPath(searchArg string) (string, error) {
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

func (t *fileSearchBase) globPatternArg(pattern string) (string, error) {
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

// Grep tool searches file contents natively using ripgo.
type Grep struct {
	fileSearchBase
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

	opts := []ripgo.Option{
		ripgo.WithHidden(true),
		ripgo.WithGlobExcludes(".git", ".git/**", "**/.git", "**/.git/**"),
	}
	if input.IgnoreCase {
		opts = append(opts, ripgo.WithIgnoreCase(true))
	}
	if input.Literal {
		opts = append(opts, ripgo.WithFixedStrings(true))
	}
	if input.Context > 0 {
		opts = append(opts, ripgo.WithContext(input.Context, input.Context))
	}
	if strings.TrimSpace(input.Glob) != "" {
		opts = append(opts, ripgo.WithGlobIncludes(input.Glob))
	}

	type matchLine struct {
		filePath   string
		lineNumber int
		lineText   string
		isContext  bool
	}

	var matchLines []matchLine
	matchCount := 0
	matchLimitReached := false

	var fileResults []search.Result
	for res, searchErr := range ripgo.Search(ctx, input.Pattern, []string{searchPath}, opts...) {
		if ctx.Err() != nil {
			return "", toolContextErr("search", ctx.Err())
		}
		if searchErr != nil {
			continue
		}
		if len(res.Matches) > 0 || len(res.Entries) > 0 {
			fileResults = append(fileResults, res)
		}
	}

	slices.SortFunc(fileResults, func(a, b search.Result) int {
		return strings.Compare(a.Path, b.Path)
	})

	for _, res := range fileResults {
		if input.Context > 0 && len(res.Entries) > 0 {
			for _, entry := range res.Entries {
				isCtx := entry.Kind == search.EntryContext
				if !isCtx {
					if matchCount >= limit {
						matchLimitReached = true
						break
					}
					matchCount++
					if matchCount >= limit {
						matchLimitReached = true
					}
				}
				matchLines = append(matchLines, matchLine{
					filePath:   res.Path,
					lineNumber: entry.Line,
					lineText:   string(entry.LineBytes),
					isContext:  isCtx,
				})
			}
		} else {
			for _, m := range res.Matches {
				if matchCount >= limit {
					matchLimitReached = true
					break
				}
				matchCount++
				if matchCount >= limit {
					matchLimitReached = true
				}
				matchLines = append(matchLines, matchLine{
					filePath:   res.Path,
					lineNumber: m.Line,
					lineText:   string(m.LineBytes),
					isContext:  false,
				})
				if matchCount >= limit {
					break
				}
			}
		}
		if matchLimitReached && (input.Context == 0 || matchCount > limit) {
			break
		}
	}

	if len(matchLines) == 0 {
		return "No matches found", nil
	}

	var outputLines []string
	linesTruncated := false
	for _, ml := range matchLines {
		displayPath := g.grepDisplayPath(ml.filePath, searchPath, isDirectory)
		lineText := strings.TrimSuffix(strings.ReplaceAll(ml.lineText, "\r", ""), "\n")
		truncated, ok := truncateGrepLine(lineText)
		linesTruncated = linesTruncated || ok
		if ml.isContext {
			outputLines = append(outputLines, fmt.Sprintf("%s-%d- %s", displayPath, ml.lineNumber, truncated))
		} else {
			outputLines = append(outputLines, fmt.Sprintf("%s:%d: %s", displayPath, ml.lineNumber, truncated))
		}
	}

	output := strings.Join(outputLines, "\n")
	output, byteTruncated := truncateToolOutputHead(output, MaxToolOutputSize)
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
			fmt.Sprintf("%s limit reached", toolOutputLimitLabel(MaxToolOutputSize)),
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

// Find tool searches files by glob pattern using ripgo's in-process finder.
type Find struct {
	fileSearchBase
}

func (f *Find) Spec() llm.Spec {
	return llm.Spec{
		Name:        "find",
		Description: "Find files matching a glob pattern with ripgo. Respects ignore files, includes hidden files, excludes .git internals, and supports ** for recursive search.",
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

	findPattern := findPatternForRoot(pattern, searchArg)
	findOpts := []ripgo.FindOption{
		ripgo.WithFindGlob(true),
		ripgo.WithFindFullPath(strings.Contains(findPattern, "/")),
		ripgo.WithFindType(findpkg.TypeFile),
		ripgo.WithFindMetadata(false),
		ripgo.WithFindHidden(true),
		ripgo.WithFindGlobExcludes(".git", ".git/**", "**/.git", "**/.git/**"),
	}

	var files []string
	for result, findErr := range ripgo.Find(ctx, findPattern, []string{searchPath}, findOpts...) {
		if ctx.Err() != nil {
			return "", toolContextErr("find", ctx.Err())
		}
		if findErr != nil {
			return "", fmt.Errorf("find failed: %w", findErr)
		}
		rawPath := result.Path
		if rel, err := filepath.Rel(f.cwd, rawPath); err == nil && (rel == "." || filepath.IsLocal(rel)) {
			files = append(files, filepath.ToSlash(rel))
		} else {
			files = append(files, filepath.ToSlash(rawPath))
		}
	}

	var matches []string
	for _, file := range files {
		matches = append(matches, searchRelativePath(file, searchArg))
	}
	slices.Sort(matches)

	if len(matches) == 0 {
		return "No files found matching pattern", nil
	}
	return formatLimitedFindMatches(matches, limit), nil
}

func findPatternForRoot(pattern, searchArg string) string {
	pattern = filepath.ToSlash(strings.TrimPrefix(pattern, "./"))
	root := strings.TrimPrefix(filepath.ToSlash(strings.TrimSpace(searchArg)), "./")
	root = strings.TrimSuffix(root, "/")
	if root != "" && root != "." && strings.Contains(pattern, "/") {
		for _, prefix := range []string{root + "/", "**/" + root + "/"} {
			if strings.HasPrefix(pattern, prefix) {
				pattern = strings.TrimPrefix(pattern, prefix)
				break
			}
		}
	}
	if strings.Contains(pattern, "/") &&
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
	output, byteTruncated := truncateToolOutputHead(output, MaxToolOutputSize)
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
			fmt.Sprintf("%s limit reached", toolOutputLimitLabel(MaxToolOutputSize)),
		)
	}
	if len(notices) > 0 {
		output += "\n\n[" + strings.Join(notices, ". ") + "]"
	}
	return output
}
