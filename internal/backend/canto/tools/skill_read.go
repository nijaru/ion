package tools

import (
	"context"
	"fmt"
	"strings"

	"github.com/go-json-experiment/json"
	"github.com/nijaru/canto/llm"
	ionskills "github.com/nijaru/ion/internal/skills"
)

type ReadSkill struct {
	paths []string
}

func NewReadSkill(paths []string) *ReadSkill {
	return &ReadSkill{paths: append([]string(nil), paths...)}
}

func (t *ReadSkill) Spec() llm.Spec {
	return llm.Spec{
		Name: "read_skill",
		Description: strings.Join([]string{
			"Read the body of an installed skill by name.",
			"Use this only when the user asks for a skill or a skill appears relevant from the local skills list.",
			"Skills are instructions and local resources, not executable packages.",
		}, " "),
		Parameters: map[string]any{
			"type": "object",
			"properties": map[string]any{
				"name": map[string]any{
					"type":        "string",
					"description": "Installed skill name, for example go-review.",
				},
			},
			"required": []string{"name"},
		},
	}
}

func (t *ReadSkill) Execute(_ context.Context, args string) (string, error) {
	var input struct {
		Name string `json:"name"`
	}
	if err := json.Unmarshal([]byte(args), &input); err != nil {
		return "", err
	}
	name := strings.TrimSpace(input.Name)
	if name == "" {
		return "", fmt.Errorf("name is required")
	}
	detail, err := ionskills.Read(t.paths, name)
	if err != nil {
		return "", err
	}
	return limitToolOutput(ionskills.FormatDetail(detail)), nil
}
