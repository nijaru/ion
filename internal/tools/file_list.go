package tools

import (
	"context"
	"fmt"
	"os"
	"slices"
	"strings"

	"github.com/nijaru/ion/internal/llm"
)

const defaultLSLimit = 500

// List tool (Pi-style ls; formerly list_directory).
type List struct {
	FileTool
}

func (l *List) Spec() llm.Spec {
	return llm.Spec{
		Name:        "ls",
		Description: "List contents of a specific directory.",
		Parameters:  lsParameters(),
	}
}

func (l *List) Execute(ctx context.Context, args string) (string, error) {
	input, err := decodeToolArgs[lsInput]("ls", args)
	if err != nil {
		return "", err
	}
	if input.Path == "" {
		input.Path = "."
	}

	absPath, err := l.absolutePath(input.Path)
	if err != nil {
		return "", err
	}

	info, err := os.Stat(absPath)
	if err != nil {
		return "", err
	}
	if !info.IsDir() {
		return "", fmt.Errorf("not a directory: %s", input.Path)
	}

	entries, err := os.ReadDir(absPath)
	if err != nil {
		return "", err
	}
	slices.SortFunc(entries, func(a, b os.DirEntry) int {
		return strings.Compare(strings.ToLower(a.Name()), strings.ToLower(b.Name()))
	})

	limit := input.Limit
	if limit <= 0 {
		limit = defaultLSLimit
	}

	var res strings.Builder
	limitReached := false
	for i, e := range entries {
		if i >= limit {
			limitReached = true
			break
		}
		suffix := ""
		if e.IsDir() {
			suffix = "/"
		}
		res.WriteString(fmt.Sprintf("%s%s\n", e.Name(), suffix))
	}
	if res.Len() == 0 {
		return "(empty directory)", nil
	}
	output := strings.TrimRight(res.String(), "\n")
	output, byteTruncated := truncateToolOutputHead(output, maxToolOutputSize)
	var notices []string
	if limitReached {
		notices = append(
			notices,
			fmt.Sprintf("%d entries limit reached. Use limit=%d for more", limit, limit*2),
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
	return output, nil
}
