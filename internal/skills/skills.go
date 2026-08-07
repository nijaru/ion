package skills

import (
	"context"
	"fmt"
	"os"
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

type resolvedSkill struct {
	skill *agentskills.Skill
	path  string
}

func discoverSkills(ctx context.Context, paths ...string) ([]resolvedSkill, error) {
	if ctx == nil {
		ctx = context.Background()
	}
	byName := make(map[string]resolvedSkill)
	for _, root := range paths {
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		if _, err := os.Stat(root); err != nil {
			if os.IsNotExist(err) {
				continue
			}
			return nil, err
		}
		if err := filepath.Walk(root, func(path string, info os.FileInfo, err error) error {
			if ctxErr := ctx.Err(); ctxErr != nil {
				return ctxErr
			}
			if err != nil {
				return fmt.Errorf("walk skills at %q: %w", path, err)
			}
			if info == nil || info.IsDir() || info.Name() != "SKILL.md" {
				return nil
			}
			skill, err := agentskills.Load(path)
			if err != nil {
				return fmt.Errorf("load skill %q: %w", path, err)
			}
			if skill != nil {
				byName[skill.Name] = resolvedSkill{skill: skill, path: path}
			}
			return nil
		}); err != nil {
			return nil, err
		}
	}

	resolved := make([]resolvedSkill, 0, len(byName))
	for _, item := range byName {
		resolved = append(resolved, item)
	}
	slices.SortFunc(resolved, func(a, b resolvedSkill) int {
		return strings.Compare(a.skill.Name, b.skill.Name)
	})
	return resolved, nil
}

func summaryForSkill(skill *agentskills.Skill) Summary {
	return Summary{
		Name:         skill.Name,
		Description:  strings.TrimSpace(skill.Description),
		AllowedTools: append([]string(nil), []string(skill.AllowedTools)...),
	}
}

func List(paths ...string) ([]Summary, error) {
	return ListContext(context.Background(), paths...)
}

func ListContext(ctx context.Context, paths ...string) ([]Summary, error) {
	resolved, err := discoverSkills(ctx, paths...)
	if err != nil {
		return nil, err
	}
	out := make([]Summary, 0, len(resolved))
	for _, item := range resolved {
		out = append(out, summaryForSkill(item.skill))
	}
	return out, nil
}

func Read(paths []string, name string) (Detail, error) {
	return ReadContext(context.Background(), paths, name)
}

func ReadContext(ctx context.Context, paths []string, name string) (Detail, error) {
	name = strings.TrimSpace(name)
	if name == "" {
		return Detail{}, fmt.Errorf("skill name is required")
	}
	resolved, err := discoverSkills(ctx, paths...)
	if err != nil {
		return Detail{}, err
	}
	var selected *agentskills.Skill
	for _, item := range resolved {
		if item.skill != nil && item.skill.Name == name {
			selected = item.skill
			break
		}
	}
	if selected == nil {
		for _, item := range resolved {
			if item.skill != nil && strings.EqualFold(item.skill.Name, name) {
				selected = item.skill
				break
			}
		}
	}
	if selected == nil {
		return Detail{}, fmt.Errorf("skill %q not found", name)
	}
	return Detail{
		Summary:      summaryForSkill(selected),
		Instructions: strings.TrimSpace(selected.Instructions),
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
	return NoticeContext(context.Background(), paths, query)
}

func NoticeContext(ctx context.Context, paths []string, query string) (string, error) {
	items, err := ListContext(ctx, paths...)
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

func FormatSkillsForPrompt(paths ...string) (string, error) {
	return FormatSkillsForPromptContext(context.Background(), paths...)
}

func FormatSkillsForPromptContext(ctx context.Context, paths ...string) (string, error) {
	resolved, err := discoverSkills(ctx, paths...)
	if err != nil {
		return "", err
	}
	if len(resolved) == 0 {
		return "", nil
	}

	var b strings.Builder
	b.WriteString("\n\nThe following skills provide specialized instructions for specific tasks.\n")
	b.WriteString("Use the read tool to load a skill's file when the task matches its description.\n")
	b.WriteString(
		"When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.\n\n",
	)
	b.WriteString("<available_skills>\n")

	for _, item := range resolved {
		summary := summaryForSkill(item.skill)
		location, err := filepath.Abs(item.path)
		if err != nil {
			location = item.path
		}
		b.WriteString("  <skill>\n")
		b.WriteString(fmt.Sprintf("    <name>%s</name>\n", escapeXml(summary.Name)))
		b.WriteString(fmt.Sprintf("    <description>%s</description>\n", escapeXml(summary.Description)))
		b.WriteString(fmt.Sprintf("    <location>%s</location>\n", escapeXml(location)))
		b.WriteString("  </skill>\n")
	}

	b.WriteString("</available_skills>")
	return b.String(), nil
}

func escapeXml(str string) string {
	str = strings.ReplaceAll(str, "&", "&amp;")
	str = strings.ReplaceAll(str, "<", "&lt;")
	str = strings.ReplaceAll(str, ">", "&gt;")
	str = strings.ReplaceAll(str, "\"", "&quot;")
	str = strings.ReplaceAll(str, "'", "&apos;")
	return str
}
