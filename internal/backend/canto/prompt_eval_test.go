package canto

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/pelletier/go-toml/v2"
)

type promptEvalCase struct {
	Name      string   `toml:"name"`
	Required  []string `toml:"required"`
	Forbidden []string `toml:"forbidden"`
}

type promptEvalSuite struct {
	Cases []promptEvalCase `toml:"case"`
}

func TestPromptQualityEvals(t *testing.T) {
	combined := buildInstructions(
		"/tmp/project",
		time.Date(2026, time.March, 27, 0, 0, 0, 0, time.UTC),
	)
	cases := loadPromptEvalCases(t)

	for _, tc := range cases {
		t.Run(tc.Name, func(t *testing.T) {
			for _, want := range tc.Required {
				if !strings.Contains(combined, want) {
					t.Fatalf("prompt missing %q\n%s", want, combined)
				}
			}
			for _, bad := range tc.Forbidden {
				if strings.Contains(combined, bad) {
					t.Fatalf("prompt unexpectedly contains %q\n%s", bad, combined)
				}
			}
		})
	}
}

func loadPromptEvalCases(t *testing.T) []promptEvalCase {
	t.Helper()
	path := filepath.Join("..", "..", "..", "evals", "golden", "prompt_quality.toml")
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read prompt eval suite: %v", err)
	}
	var suite promptEvalSuite
	if err := toml.Unmarshal(data, &suite); err != nil {
		t.Fatalf("parse prompt eval suite: %v", err)
	}
	if len(suite.Cases) == 0 {
		t.Fatal("prompt eval suite has no cases")
	}
	return suite.Cases
}
