package tools

import (
	"bufio"
	"context"
	"fmt"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"time"

	"github.com/bmatcuk/doublestar/v4"
	"github.com/go-json-experiment/json"
	"github.com/nijaru/canto/llm"
	"github.com/nijaru/ion/internal/storage"
	ignore "github.com/sabhiram/go-gitignore"
)

type SearchTool struct {
	cwd string
}

func NewSearchTool(cwd string) *SearchTool {
	return &SearchTool{cwd: cwd}
}

// Grep tool
type Grep struct {
	SearchTool
}

func (g *Grep) Spec() llm.Spec {
	return llm.Spec{
		Name:        "grep",
		Description: "Search for a regex pattern in files. Uses ripgrep if available, falling back to a Go-native search that respects .gitignore.",
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

	if input.Path == "" {
		input.Path = "."
	}

	// Try ripgrep first as it's the fastest
	cmd := exec.CommandContext(ctx, "rg", "--max-count", "100", "--heading", "--line-number", "--color", "never", input.Pattern, input.Path)
	cmd.Dir = g.cwd
	output, err := cmd.CombinedOutput()
	if err == nil {
		return string(output), nil
	}

	// Fallback to Go-native implementation that respects .gitignore
	return g.nativeGrep(input.Pattern, input.Path)
}

func (g *Grep) nativeGrep(pattern, root string) (string, error) {
	re, err := regexp.Compile(pattern)
	if err != nil {
		return "", fmt.Errorf("invalid regex: %w", err)
	}

	absRoot := filepath.Join(g.cwd, root)
	
	// Load .gitignore if present
	var ignorer *ignore.GitIgnore
	if gitignore, err := os.ReadFile(filepath.Join(g.cwd, ".gitignore")); err == nil {
		ignorer = ignore.CompileIgnoreLines(strings.Split(string(gitignore), "\n")...)
	}

	var res strings.Builder
	count := 0
	maxResults := 100

	err = filepath.WalkDir(absRoot, func(path string, d fs.DirEntry, err error) error {
		if err != nil || count >= maxResults {
			return err
		}

		rel, _ := filepath.Rel(g.cwd, path)
		if ignorer != nil && ignorer.MatchesPath(rel) {
			if d.IsDir() {
				return filepath.SkipDir
			}
			return nil
		}

		if d.IsDir() {
			if d.Name() == ".git" {
				return filepath.SkipDir
			}
			return nil
		}

		f, err := os.Open(path)
		if err != nil {
			return nil
		}
		defer f.Close()

		scanner := bufio.NewScanner(f)
		lineNum := 1
		fileHeaderPrinted := false

		for scanner.Scan() {
			line := scanner.Text()
			if re.MatchString(line) {
				if !fileHeaderPrinted {
					res.WriteString(fmt.Sprintf("\n%s\n", rel))
					fileHeaderPrinted = true
				}
				res.WriteString(fmt.Sprintf("%d:%s\n", lineNum, line))
				count++
				if count >= maxResults {
					break
				}
			}
			lineNum++
		}

		return nil
	})

	if err != nil && err != filepath.SkipDir {
		return "", err
	}

	if res.Len() == 0 {
		return "No matches found.", nil
	}

	return res.String(), nil
}

// Glob tool
type Glob struct {
	SearchTool
}

func (g *Glob) Spec() llm.Spec {
	return llm.Spec{
		Name:        "glob",
		Description: "Search for files matching a glob pattern (supports ** for recursive search).",
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

	// Use doublestar for robust, recursive globbing
	fsys := os.DirFS(g.cwd)
	matches, err := doublestar.Glob(fsys, input.Pattern)
	if err != nil {
		return "", fmt.Errorf("glob failed: %w", err)
	}

	if len(matches) == 0 {
		return "No matches found.", nil
	}

	return strings.Join(matches, "\n"), nil
}
