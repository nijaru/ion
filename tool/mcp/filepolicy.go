package mcp

import (
	"fmt"
	"slices"
	"strings"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/tool"
	"github.com/nijaru/ion/internal/workvfs"
)

// FilePolicy applies workspace validation and sensitive-path detection to MCP
// tools that look like file operations.
type FilePolicy struct {
	Validator      *workspace.Validator
	ProtectedPaths []string
}

// category is the type of file operation.
type category string

const (
	categoryRead    category = "read"
	categoryWrite   category = "write"
	categoryExecute category = "execute"
)

// Requirement describes an approval requirement for a tool call.
type Requirement = tool.Requirement

type fileIntent struct {
	category category
	paths    []string
}

func (p *FilePolicy) normalizeArguments(
	spec llm.Spec,
	arguments map[string]any,
) (map[string]any, *fileIntent, error) {
	intent := classifyFileIntent(spec, arguments)
	if intent == nil {
		return arguments, nil, nil
	}
	normalized := cloneArguments(arguments)
	seen := make([]string, 0, len(intent.paths))
	for _, key := range orderedPathKeys(arguments) {
		value, ok := arguments[key]
		if !ok {
			continue
		}
		text, ok := value.(string)
		if !ok || text == "" {
			continue
		}
		cleaned := text
		if p != nil && p.Validator != nil {
			var err error
			cleaned, err = p.Validator.Validate(text)
			if err != nil {
				return nil, nil, fmt.Errorf("mcp file policy %q: %w", key, err)
			}
		}
		normalized[key] = cleaned
		seen = append(seen, cleaned)
	}
	intent.paths = slices.Compact(seen)
	return normalized, intent, nil
}

func (p *FilePolicy) approvalRequirement(
	spec llm.Spec,
	arguments map[string]any,
) (Requirement, bool, error) {
	normalized, intent, err := p.normalizeArguments(spec, arguments)
	if err != nil {
		return Requirement{}, false, err
	}
	if intent == nil || len(intent.paths) == 0 {
		return Requirement{}, false, nil
	}
	intent.paths = extractPathLikeValues(normalized)

	resource := intent.paths[0]
	if p != nil && len(p.ProtectedPaths) > 0 {
		for _, path := range intent.paths {
			if isProtectedPath(path, p.ProtectedPaths) {
				resource = path
				break
			}
		}
	}
	return Requirement{
		Category:  string(intent.category),
		Operation: spec.Name,
		Resource:  resource,
	}, true, nil
}

func classifyFileIntent(spec llm.Spec, arguments map[string]any) *fileIntent {
	paths := extractPathLikeValues(arguments)
	if len(paths) == 0 {
		return nil
	}
	descriptor := strings.ToLower(spec.Name + " " + spec.Description)
	switch {
	case containsAny(descriptor, "write", "edit", "create", "update", "delete", "remove", "rename", "move", "mkdir"):
		return &fileIntent{category: categoryWrite, paths: paths}
	case containsAny(descriptor, "read", "list", "glob", "find", "search", "stat", "cat"):
		return &fileIntent{category: categoryRead, paths: paths}
	default:
		return nil
	}
}

func extractPathLikeValues(arguments map[string]any) []string {
	keys := orderedPathKeys(arguments)
	paths := make([]string, 0, len(keys))
	for _, key := range keys {
		if value, ok := arguments[key].(string); ok && value != "" {
			paths = append(paths, value)
		}
	}
	return paths
}

func orderedPathKeys(arguments map[string]any) []string {
	keys := make([]string, 0, len(arguments))
	for key := range arguments {
		if isPathKey(key) {
			keys = append(keys, key)
		}
	}
	slices.Sort(keys)
	return keys
}

func isPathKey(key string) bool {
	switch strings.ToLower(key) {
	case "path", "file", "file_path", "filepath", "filename", "dir", "directory", "pattern",
		"source", "source_path", "destination", "destination_path", "target", "target_path",
		"from", "to", "old_path", "new_path":
		return true
	default:
		return false
	}
}

func containsAny(text string, needles ...string) bool {
	for _, needle := range needles {
		if strings.Contains(text, needle) {
			return true
		}
	}
	return false
}

// isProtectedPath checks if a path matches any of the protected paths.
func isProtectedPath(path string, protectedPaths []string) bool {
	for _, protected := range protectedPaths {
		if strings.HasPrefix(path, protected) || path == protected {
			return true
		}
	}
	return false
}

func cloneArguments(arguments map[string]any) map[string]any {
	if arguments == nil {
		return nil
	}
	cloned := make(map[string]any, len(arguments))
	for key, value := range arguments {
		cloned[key] = value
	}
	return cloned
}
