package skills

import (
	"fmt"
	"path/filepath"
	"slices"
	"strings"

	agentskills "github.com/nijaru/agentskills"
)

type Summary struct {
	Name         string
	Description  string
	AllowedTools []string
}

type Detail struct {
	Summary
	Instructions string
}

func List(paths ...string) ([]Summary, error) {
	reg := agentskills.NewRegistry(paths...)
	if err := reg.Discover(); err != nil {
		return nil, err
	}
	loaded := reg.List()
	out := make([]Summary, 0, len(loaded))
	for _, skill := range loaded {
		if skill == nil {
			continue
		}
		out = append(out, Summary{
			Name:         skill.Name,
			Description:  strings.TrimSpace(skill.Description),
			AllowedTools: append([]string(nil), []string(skill.AllowedTools)...),
		})
	}
	slices.SortFunc(out, func(a, b Summary) int {
		return strings.Compare(a.Name, b.Name)
	})
	return out, nil
}

func Read(paths []string, name string) (Detail, error) {
	name = strings.TrimSpace(name)
	if name == "" {
		return Detail{}, fmt.Errorf("skill name is required")
	}
	reg := agentskills.NewRegistry(paths...)
	if err := reg.Discover(); err != nil {
		return Detail{}, err
	}
	skill, ok := reg.Get(name)
	if !ok {
		for _, candidate := range reg.List() {
			if candidate != nil && strings.EqualFold(candidate.Name, name) {
				skill = candidate
				ok = true
				break
			}
		}
	}
	if !ok || skill == nil {
		return Detail{}, fmt.Errorf("skill %q not found", name)
	}
	return Detail{
		Summary: Summary{
			Name:         skill.Name,
			Description:  strings.TrimSpace(skill.Description),
			AllowedTools: append([]string(nil), []string(skill.AllowedTools)...),
		},
		Instructions: strings.TrimSpace(skill.Instructions),
	}, nil
}

func FormatDetail(detail Detail) string {
	var b strings.Builder
	b.WriteString("# ")
	b.WriteString(detail.Name)
	if detail.Description != "" {
		b.WriteString("\n\n")
		b.WriteString(detail.Description)
	}
	if len(detail.AllowedTools) > 0 {
		b.WriteString("\n\nAllowed tools: ")
		b.WriteString(strings.Join(detail.AllowedTools, ", "))
	}
	if detail.Instructions != "" {
		b.WriteString("\n\n")
		b.WriteString(detail.Instructions)
	}
	return strings.TrimRight(b.String(), "\n")
}

func Search(items []Summary, query string) []Summary {
	query = strings.ToLower(strings.TrimSpace(query))
	if query == "" {
		return append([]Summary(nil), items...)
	}
	matches := make([]Summary, 0, len(items))
	for _, item := range items {
		haystack := strings.ToLower(item.Name + " " + item.Description + " " +
			strings.Join(item.AllowedTools, " "))
		if strings.Contains(haystack, query) {
			matches = append(matches, item)
		}
	}
	return matches
}

func Notice(paths []string, query string) (string, error) {
	items, err := List(paths...)
	if err != nil {
		return "", err
	}
	matches := Search(items, query)
	return FormatNotice(paths, query, matches), nil
}

func FormatNotice(paths []string, query string, items []Summary) string {
	var b strings.Builder
	b.WriteString("skills\n")
	if len(paths) > 0 {
		b.WriteString("\npaths:\n")
		for _, path := range paths {
			b.WriteString("- ")
			b.WriteString(filepath.Clean(path))
			b.WriteByte('\n')
		}
	}
	if trimmed := strings.TrimSpace(query); trimmed != "" {
		b.WriteString("\nquery: ")
		b.WriteString(trimmed)
		b.WriteByte('\n')
	}
	if len(items) == 0 {
		b.WriteString("\nNo installed skills found.")
		return b.String()
	}
	b.WriteString("\ninstalled:\n")
	for _, item := range items {
		b.WriteString("- ")
		b.WriteString(item.Name)
		if item.Description != "" {
			b.WriteString(": ")
			b.WriteString(item.Description)
		}
		if len(item.AllowedTools) > 0 {
			b.WriteString(" (tools: ")
			b.WriteString(strings.Join(item.AllowedTools, ", "))
			b.WriteByte(')')
		}
		b.WriteByte('\n')
	}
	return strings.TrimRight(b.String(), "\n")
}
