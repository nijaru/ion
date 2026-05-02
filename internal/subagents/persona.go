package subagents

import (
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"

	"gopkg.in/yaml.v3"
)

type ModelSlot string

const (
	ModelSlotPrimary ModelSlot = "primary"
	ModelSlotFast    ModelSlot = "fast"
)

type Persona struct {
	Name        string
	Description string
	ModelSlot   ModelSlot
	Tools       []string
	Prompt      string
}

type frontmatter struct {
	Name        string   `yaml:"name"`
	Description string   `yaml:"description"`
	Model       string   `yaml:"model"`
	Tools       []string `yaml:"tools"`
}

func Builtins() []Persona {
	return []Persona{
		{
			Name:        "explorer",
			Description: "Read-only codebase exploration and focused context gathering.",
			ModelSlot:   ModelSlotFast,
			Tools:       []string{"read", "grep", "glob", "list", "recall_memory"},
			Prompt:      "Explore the codebase for the requested question. Return a concise summary with file paths and concrete findings. Do not make changes.",
		},
		{
			Name:        "reviewer",
			Description: "Focused correctness, regression, and risk review.",
			ModelSlot:   ModelSlotPrimary,
			Tools:       []string{"read", "grep", "glob", "list", "bash", "recall_memory"},
			Prompt:      "Review the requested area for correctness risks, missing tests, and behavior regressions. Return findings first, with file paths and evidence.",
		},
		{
			Name:        "worker",
			Description: "Scoped implementation work with normal edit and verification tools.",
			ModelSlot:   ModelSlotPrimary,
			Tools: []string{
				"read",
				"grep",
				"glob",
				"list",
				"write",
				"edit",
				"multi_edit",
				"bash",
				"recall_memory",
				"remember_memory",
			},
			Prompt: "Implement only the assigned task. Keep changes scoped, verify them, and summarize changed files and test results.",
		},
	}
}

func LoadDir(path string) ([]Persona, error) {
	if strings.TrimSpace(path) == "" {
		return nil, nil
	}
	entries, err := os.ReadDir(path)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, err
	}

	var personas []Persona
	for _, entry := range entries {
		if entry.IsDir() || filepath.Ext(entry.Name()) != ".md" {
			continue
		}
		persona, err := LoadFile(filepath.Join(path, entry.Name()))
		if err != nil {
			return nil, err
		}
		personas = append(personas, persona)
	}
	slices.SortFunc(personas, func(a, b Persona) int {
		return strings.Compare(a.Name, b.Name)
	})
	return personas, nil
}

func LoadFile(path string) (Persona, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return Persona{}, err
	}
	persona, err := ParseMarkdown(string(data))
	if err != nil {
		return Persona{}, fmt.Errorf("%s: %w", path, err)
	}
	return persona, nil
}

func ParseMarkdown(input string) (Persona, error) {
	header, body, ok := strings.Cut(strings.TrimPrefix(input, "\ufeff"), "---\n")
	if header != "" || !ok {
		return Persona{}, fmt.Errorf("missing YAML frontmatter")
	}
	yml, prompt, ok := strings.Cut(body, "\n---")
	if !ok {
		return Persona{}, fmt.Errorf("unterminated YAML frontmatter")
	}

	var meta frontmatter
	if err := yaml.Unmarshal([]byte(yml), &meta); err != nil {
		return Persona{}, fmt.Errorf("parse frontmatter: %w", err)
	}

	persona := Persona{
		Name:        strings.TrimSpace(meta.Name),
		Description: strings.TrimSpace(meta.Description),
		ModelSlot:   normalizeModelSlot(meta.Model),
		Tools:       normalizeTools(meta.Tools),
		Prompt:      strings.TrimSpace(prompt),
	}
	if err := persona.Validate(); err != nil {
		return Persona{}, err
	}
	return persona, nil
}

func Merge(base, custom []Persona) []Persona {
	byName := make(map[string]Persona, len(base)+len(custom))
	for _, persona := range base {
		byName[persona.Name] = persona
	}
	for _, persona := range custom {
		byName[persona.Name] = persona
	}

	names := make([]string, 0, len(byName))
	for name := range byName {
		names = append(names, name)
	}
	slices.Sort(names)

	out := make([]Persona, 0, len(names))
	for _, name := range names {
		out = append(out, byName[name])
	}
	return out
}

func Find(personas []Persona, name string) (Persona, bool) {
	name = strings.TrimSpace(name)
	for _, persona := range personas {
		if persona.Name == name {
			return persona, true
		}
	}
	return Persona{}, false
}

func (p Persona) Validate() error {
	if p.Name == "" {
		return fmt.Errorf("name is required")
	}
	if strings.ContainsAny(p.Name, " \t\r\n") {
		return fmt.Errorf("name %q must not contain whitespace", p.Name)
	}
	if p.Description == "" {
		return fmt.Errorf("description is required")
	}
	if p.Prompt == "" {
		return fmt.Errorf("prompt body is required")
	}
	if p.ModelSlot == "" {
		return fmt.Errorf("model is required")
	}
	if p.ModelSlot != ModelSlotPrimary && p.ModelSlot != ModelSlotFast {
		return fmt.Errorf("invalid model %q", p.ModelSlot)
	}
	if len(p.Tools) == 0 {
		return fmt.Errorf("at least one tool is required")
	}
	return nil
}

func normalizeModelSlot(value string) ModelSlot {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "", string(ModelSlotPrimary):
		return ModelSlotPrimary
	case string(ModelSlotFast):
		return ModelSlotFast
	default:
		return ModelSlot(strings.TrimSpace(value))
	}
}

func normalizeTools(values []string) []string {
	seen := make(map[string]struct{}, len(values))
	var tools []string
	for _, value := range values {
		tool := strings.TrimSpace(value)
		if tool == "" {
			continue
		}
		if _, ok := seen[tool]; ok {
			continue
		}
		seen[tool] = struct{}{}
		tools = append(tools, tool)
	}
	slices.Sort(tools)
	return tools
}
