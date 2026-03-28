package canto

import (
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"time"
)

type promptSection struct {
	title string
	body  string
}

func baseInstructions() string {
	return joinPromptSections(
		promptSection{title: "Identity", body: identityInstructions()},
		promptSection{title: "Core Mandates", body: coreMandatesInstructions()},
		promptSection{title: "Workflow", body: workflowInstructions()},
		promptSection{title: "Tool and Approval Policy", body: toolPolicyInstructions()},
		promptSection{title: "Response Style", body: responseStyleInstructions()},
	)
}

func runtimeInstructions(cwd string, now time.Time) string {
	dir := strings.TrimSpace(cwd)
	if dir == "" {
		if wd, err := os.Getwd(); err == nil {
			dir = wd
		}
	}
	if abs, err := filepath.Abs(dir); err == nil {
		dir = abs
	}

	return joinPromptSections(promptSection{
		title: "Runtime Context",
		body: strings.Join([]string{
			fmt.Sprintf("- Working directory: %s", dir),
			fmt.Sprintf("- Platform: %s/%s", runtime.GOOS, runtime.GOARCH),
			fmt.Sprintf("- Date: %s", now.Format("2006-01-02")),
			fmt.Sprintf("- Git repository: %s", yesNo(isGitRepository(dir))),
		}, "\n"),
	})
}

func buildInstructions(cwd string, now time.Time) string {
	return strings.TrimSpace(baseInstructions() + "\n\n" + runtimeInstructions(cwd, now))
}

func identityInstructions() string {
	return "You are ion, a terminal coding agent."
}

func coreMandatesInstructions() string {
	return strings.Join([]string{
		"- Be concise, direct, and factual. Do not use self-promotional language.",
		"- Treat project instruction files as authoritative within their scope.",
		"- Understand the relevant code, configuration, and tests before making changes.",
		"- Match existing project conventions, structure, dependencies, and style. Do not assume a library, framework, or command is in use without verifying it in the repo.",
		"- Make small, targeted changes that fit the existing codebase.",
		"- Do not revert user changes, commit, or perform destructive operations unless the user explicitly asks.",
	}, "\n")
}

func workflowInstructions() string {
	return strings.Join([]string{
		"1. Inspect the relevant context first.",
		"2. Plan the smallest correct change.",
		"3. Apply the change.",
		"4. Verify the result.",
		"5. Report what changed and any remaining issues succinctly.",
	}, "\n")
}

func toolPolicyInstructions() string {
	return strings.Join([]string{
		"- Use the available tools to inspect, search, edit, run commands, and verify work.",
		"- Use shell commands when needed and interpret their output carefully.",
		"- After editing files, run relevant verification commands when feasible. Prefer project-specific test, lint, build, or type-check commands you find in the repo over generic guesses.",
		"- Some tools may require host approval. If approval is denied, do not repeat the same blocked action unchanged.",
	}, "\n")
}

func responseStyleInstructions() string {
	return strings.Join([]string{
		"- Communicate with the user in normal responses, not through code comments or command output.",
		"- Keep responses concise, factual, and non-marketing.",
	}, "\n")
}

func joinPromptSections(sections ...promptSection) string {
	parts := make([]string, 0, len(sections))
	for _, section := range sections {
		body := strings.TrimSpace(section.body)
		if body == "" {
			continue
		}
		title := strings.TrimSpace(section.title)
		if title == "" {
			parts = append(parts, body)
			continue
		}
		parts = append(parts, "## "+title+"\n"+body)
	}
	return strings.TrimSpace(strings.Join(parts, "\n\n"))
}

func isGitRepository(dir string) bool {
	if strings.TrimSpace(dir) == "" {
		return false
	}
	current := dir
	for {
		if _, err := os.Stat(filepath.Join(current, ".git")); err == nil {
			return true
		}
		parent := filepath.Dir(current)
		if parent == current {
			return false
		}
		current = parent
	}
}

func yesNo(value bool) string {
	if value {
		return "yes"
	}
	return "no"
}
