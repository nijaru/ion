package canto

import (
	"strings"
	"testing"
)

func TestBaseInstructionsFocusOnOperatingPolicy(t *testing.T) {
	prompt := baseInstructions()

	required := []string{
		"You are ion, a terminal coding agent.",
		"Treat project instruction files as authoritative within their scope.",
		"After editing files, run relevant verification commands when feasible.",
		"Do not revert user changes, commit, or perform destructive operations unless the user explicitly asks.",
		"Workflow:",
	}
	for _, want := range required {
		if !strings.Contains(prompt, want) {
			t.Fatalf("prompt missing %q\n%s", want, prompt)
		}
	}
}

func TestBaseInstructionsAvoidStaleBrandingAndModelSpecifics(t *testing.T) {
	prompt := baseInstructions()

	forbidden := []string{
		"elite",
		"Canto framework",
		"Always prefer modern Go",
		"go test ./...",
		"high-fidelity verification loop",
	}
	for _, bad := range forbidden {
		if strings.Contains(prompt, bad) {
			t.Fatalf("prompt unexpectedly contains %q\n%s", bad, prompt)
		}
	}
}
