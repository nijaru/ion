package prompts

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"strconv"
	"strings"
	"unicode"

	"gopkg.in/yaml.v3"
)

// PromptTemplate represents a reusable prompt template loaded from a markdown file.
type PromptTemplate struct {
	Name         string `json:"name" yaml:"name"`
	Description  string `json:"description" yaml:"description"`
	ArgumentHint string `json:"argument_hint" yaml:"argument-hint"`
	Content      string `json:"content" yaml:"content"`
	Path         string `json:"path" yaml:"path"`
}

type frontmatter struct {
	Description  string `yaml:"description"`
	ArgumentHint string `yaml:"argument-hint"`
}

// DiscoverPrompts finds all .md files in the provided directories.
func DiscoverPrompts(ctx context.Context, dirs ...string) ([]PromptTemplate, error) {
	if ctx == nil {
		ctx = context.Background()
	}
	byName := make(map[string]PromptTemplate)
	for _, root := range dirs {
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		root = strings.TrimSpace(root)
		if root == "" {
			continue
		}
		info, err := os.Stat(root)
		if err != nil {
			if os.IsNotExist(err) {
				continue
			}
			return nil, err
		}
		if !info.IsDir() {
			if strings.HasSuffix(strings.ToLower(root), ".md") {
				tmpl, err := LoadTemplateFile(root)
				if err == nil {
					byName[tmpl.Name] = tmpl
				}
			}
			continue
		}

		entries, err := os.ReadDir(root)
		if err != nil {
			continue
		}
		for _, entry := range entries {
			if entry.IsDir() || !strings.HasSuffix(strings.ToLower(entry.Name()), ".md") {
				continue
			}
			filePath := filepath.Join(root, entry.Name())
			tmpl, err := LoadTemplateFile(filePath)
			if err == nil {
				byName[tmpl.Name] = tmpl
			}
		}
	}

	result := make([]PromptTemplate, 0, len(byName))
	for _, item := range byName {
		result = append(result, item)
	}
	slices.SortFunc(result, func(a, b PromptTemplate) int {
		return strings.Compare(a.Name, b.Name)
	})
	return result, nil
}

// LoadTemplateFile loads a single PromptTemplate from a markdown file path.
func LoadTemplateFile(filePath string) (PromptTemplate, error) {
	data, err := os.ReadFile(filePath)
	if err != nil {
		return PromptTemplate{}, fmt.Errorf("read template %q: %w", filePath, err)
	}

	name := strings.TrimSuffix(filepath.Base(filePath), filepath.Ext(filePath))
	fm, body := parseFrontmatter(string(data))

	desc := strings.TrimSpace(fm.Description)
	if desc == "" {
		for _, line := range strings.Split(body, "\n") {
			line = strings.TrimSpace(line)
			if line != "" {
				desc = line
				if len(desc) > 60 {
					desc = desc[:60] + "..."
				}
				break
			}
		}
	}

	return PromptTemplate{
		Name:         name,
		Description:  desc,
		ArgumentHint: strings.TrimSpace(fm.ArgumentHint),
		Content:      body,
		Path:         filePath,
	}, nil
}

type rawFrontmatter struct {
	Description  string      `yaml:"description"`
	ArgumentHint interface{} `yaml:"argument-hint"`
}

func parseFrontmatter(raw string) (frontmatter, string) {
	var fm frontmatter
	if !strings.HasPrefix(raw, "---\n") && !strings.HasPrefix(raw, "---\r\n") {
		return fm, strings.TrimSpace(raw)
	}

	lines := strings.Split(raw, "\n")
	endIdx := -1
	for i := 1; i < len(lines); i++ {
		if strings.TrimSpace(lines[i]) == "---" {
			endIdx = i
			break
		}
	}

	if endIdx == -1 {
		return fm, strings.TrimSpace(raw)
	}

	fmLines := strings.Join(lines[1:endIdx], "\n")
	var rawFm rawFrontmatter
	if err := yaml.Unmarshal([]byte(fmLines), &rawFm); err == nil {
		fm.Description = rawFm.Description
		switch v := rawFm.ArgumentHint.(type) {
		case string:
			fm.ArgumentHint = v
		case []interface{}:
			var parts []string
			for _, item := range v {
				parts = append(parts, fmt.Sprint(item))
			}
			fm.ArgumentHint = "[" + strings.Join(parts, ", ") + "]"
		}
	}
	body := strings.TrimSpace(strings.Join(lines[endIdx+1:], "\n"))
	return fm, body
}

// ParseCommandArgs splits an argument string bash-style, respecting single and double quotes.
func ParseCommandArgs(argsString string) []string {
	var args []string
	var current strings.Builder
	var inQuote rune

	for _, r := range argsString {
		if inQuote != 0 {
			if r == inQuote {
				inQuote = 0
			} else {
				current.WriteRune(r)
			}
		} else if r == '"' || r == '\'' {
			inQuote = r
		} else if unicode.IsSpace(r) {
			if current.Len() > 0 {
				args = append(args, current.String())
				current.Reset()
			}
		} else {
			current.WriteRune(r)
		}
	}

	if current.Len() > 0 {
		args = append(args, current.String())
	}
	return args
}

var argPlaceholderRegex = regexp.MustCompile(`\$\{(\d+|ARGUMENTS|@):-([^}]*)\}|\$\{@:(\d+)(?::(\d+))?\}|\$(ARGUMENTS|@|\d+)`)

// SubstituteArgs replaces positional and aggregate arguments in template content.
// Supports:
// - $1, $2, ... for positional arguments
// - $@ and $ARGUMENTS for all arguments
// - ${N:-default} for positional argument N with default
// - ${@:-default} and ${ARGUMENTS:-default} for all arguments with default
// - ${@:N} for args from Nth onwards (1-indexed)
// - ${@:N:L} for L args starting from Nth
func SubstituteArgs(content string, args []string) string {
	allArgs := strings.Join(args, " ")

	return argPlaceholderRegex.ReplaceAllStringFunc(content, func(match string) string {
		submatches := argPlaceholderRegex.FindStringSubmatch(match)
		if len(submatches) == 0 {
			return match
		}

		defaultTarget := submatches[1]
		defaultValue := submatches[2]
		sliceStart := submatches[3]
		sliceLength := submatches[4]
		simple := submatches[5]

		if defaultTarget != "" {
			if defaultTarget == "@" || defaultTarget == "ARGUMENTS" {
				if allArgs != "" {
					return allArgs
				}
				return defaultValue
			}
			idx, err := strconv.Atoi(defaultTarget)
			if err == nil && idx >= 1 && idx <= len(args) && args[idx-1] != "" {
				return args[idx-1]
			}
			return defaultValue
		}

		if sliceStart != "" {
			start, err := strconv.Atoi(sliceStart)
			if err != nil || start < 1 {
				start = 1
			}
			startIdx := start - 1
			if startIdx >= len(args) {
				return ""
			}
			if sliceLength != "" {
				length, err := strconv.Atoi(sliceLength)
				if err == nil && length >= 0 {
					endIdx := min(len(args), startIdx+length)
					return strings.Join(args[startIdx:endIdx], " ")
				}
			}
			return strings.Join(args[startIdx:], " ")
		}

		if simple == "@" || simple == "ARGUMENTS" {
			return allArgs
		}

		if simple != "" {
			idx, err := strconv.Atoi(simple)
			if err == nil && idx >= 1 && idx <= len(args) {
				return args[idx-1]
			}
			return ""
		}

		return match
	})
}

// ExpandPromptTemplate checks if text matches a prompt template (/template_name [args...])
// and returns the expanded string and true if expanded, or text and false.
func ExpandPromptTemplate(text string, templates []PromptTemplate) (string, bool) {
	trimmed := strings.TrimSpace(text)
	if !strings.HasPrefix(trimmed, "/") || strings.HasPrefix(trimmed, "//") {
		return text, false
	}

	commandAndArgs := strings.TrimPrefix(trimmed, "/")
	parts := strings.SplitN(commandAndArgs, " ", 2)
	templateName := parts[0]
	argsString := ""
	if len(parts) > 1 {
		argsString = strings.TrimSpace(parts[1])
	}

	for _, tmpl := range templates {
		if strings.EqualFold(tmpl.Name, templateName) {
			args := ParseCommandArgs(argsString)
			return SubstituteArgs(tmpl.Content, args), true
		}
	}

	return text, false
}
