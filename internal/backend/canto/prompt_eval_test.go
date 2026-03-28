package canto

import (
	"strings"
	"testing"
	"time"
)

type promptEvalCase struct {
	name      string
	prompt    string
	required  []string
	forbidden []string
}

func TestPromptQualityEvals(t *testing.T) {
	combined := buildInstructions("/tmp/project", time.Date(2026, time.March, 27, 0, 0, 0, 0, time.UTC))

	cases := []promptEvalCase{
		{
			name:   "concise non-marketing tone",
			prompt: combined,
			required: []string{
				"Keep responses concise, factual, and non-marketing.",
				"Do not use self-promotional language.",
			},
			forbidden: []string{
				"elite",
				"best coding agent",
				"built on the Canto framework",
				"How can I help you today?",
			},
		},
		{
			name:   "inspect before editing",
			prompt: combined,
			required: []string{
				"Understand the relevant code, configuration, and tests before making changes.",
				"1. Inspect the relevant context first.",
			},
		},
		{
			name:   "verify after editing",
			prompt: combined,
			required: []string{
				"After editing files, run relevant verification commands when feasible.",
				"4. Verify the result.",
			},
		},
		{
			name:   "avoid stale model and provider recommendations",
			prompt: combined,
			forbidden: []string{
				"Anthropic:",
				"OpenAI:",
				"OpenRouter:",
				"Gemini:",
				"Claude 3.7 Sonnet",
				"GPT-4.1",
				"provider API",
			},
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			for _, want := range tc.required {
				if !strings.Contains(tc.prompt, want) {
					t.Fatalf("prompt missing %q\n%s", want, tc.prompt)
				}
			}
			for _, bad := range tc.forbidden {
				if strings.Contains(tc.prompt, bad) {
					t.Fatalf("prompt unexpectedly contains %q\n%s", bad, tc.prompt)
				}
			}
		})
	}
}
