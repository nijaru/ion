package instructions

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"
)

const defaultBasePrompt = `You are ion, a terminal coding agent.

Core rules:
- Be concise, direct, and factual. Do not use self-promotional language.
- Treat project instruction files as authoritative within their scope.
- Understand the relevant code, configuration, and tests before making changes.
- Match existing project conventions, structure, dependencies, and style. Do not assume a library, framework, or command is in use without verifying it in the repo.
- Make small, targeted changes that fit the existing codebase.
- Do not revert user changes, commit, or perform destructive operations unless the user explicitly asks.

Workflow:
1. Inspect the relevant context first.
2. Plan the smallest correct change.
3. Apply the change.
4. Verify the result.
5. Report what changed and any remaining issues succinctly.

Tool policy:
- Use the available tools to inspect, search, edit, run commands, and verify work.
- Use shell commands when needed and interpret their output carefully.
- After editing files, run relevant verification commands when feasible. Prefer project-specific test, lint, build, or type-check commands you find in the repo over generic guesses.
- Some tools may require host approval. If approval is denied, do not repeat the same blocked action unchanged.

Response style:
- Communicate with the user in normal responses, not through code comments or command output.
- Keep responses concise, factual, and non-marketing.`

// BasePrompt returns Ion's built-in operating policy. Project instructions and
// runtime context are added by BuildSystemPrompt; callers should not duplicate
// this text in startup or harness code.
func BasePrompt() string { return defaultBasePrompt }

// BuildSystemPrompt assembles the complete prompt sent to the provider. A
// non-empty override replaces Ion's built-in policy, while an append and the
// resource sections are retained in both cases. Project instruction files are
// included only below the host-resolved trusted project root.
func BuildSystemPrompt(
	systemPromptOverride, appendSystemPrompt, skillsText, cwd, projectTrustRoot string,
) (string, error) {
	override, err := resolvePromptInput(systemPromptOverride)
	if err != nil {
		return "", err
	}
	appendPrompt, err := resolvePromptInput(appendSystemPrompt)
	if err != nil {
		return "", err
	}

	base := strings.TrimSpace(override)
	if base == "" {
		base = BasePrompt()
	}
	if appendPrompt = strings.TrimSpace(appendPrompt); appendPrompt != "" {
		base += "\n\n" + appendPrompt
	}

	resolvedCWD, err := resolveCWD(cwd)
	if err != nil {
		return "", err
	}
	prompt, err := BuildInstructions(base, resolvedCWD, projectTrustRoot)
	if err != nil {
		return "", err
	}
	if skills := strings.TrimSpace(skillsText); skills != "" {
		prompt += "\n\n" + skills
	}

	prompt = strings.TrimSpace(prompt)
	prompt += "\n\nCurrent date: " + time.Now().Format("2006-01-02")
	prompt += "\nCurrent working directory: " + filepath.ToSlash(resolvedCWD)
	return prompt, nil
}

func resolvePromptInput(input string) (string, error) {
	input = strings.TrimSpace(input)
	if input == "" {
		return "", nil
	}
	data, err := os.ReadFile(input)
	if err == nil {
		return string(data), nil
	}
	if os.IsNotExist(err) {
		return input, nil
	}
	return "", fmt.Errorf("read prompt file %q: %w", input, err)
}

func resolveCWD(cwd string) (string, error) {
	cwd = strings.TrimSpace(cwd)
	if cwd == "" {
		var err error
		cwd, err = os.Getwd()
		if err != nil {
			return "", err
		}
	}
	return filepath.Abs(cwd)
}

type InstructionLayer struct {
	Path    string
	Content string
}

var instructionFileNames = []string{
	"AGENTS.md",
	"AGENTS.MD",
	"CLAUDE.md",
	"CLAUDE.MD",
}

func BuildInstructions(base, cwd, projectTrustRoot string) (string, error) {
	base = strings.TrimSpace(base)
	if cwd == "" {
		return base, nil
	}

	layers, err := LoadInstructionLayers(cwd, projectTrustRoot)
	if err != nil {
		return "", err
	}
	if len(layers) == 0 {
		return base, nil
	}

	var b strings.Builder
	b.WriteString(base)
	b.WriteString("\n\n<project_context>\n\nProject-specific instructions and guidelines:\n\n")
	for _, layer := range layers {
		b.WriteString(`<project_instructions path="`)
		b.WriteString(layer.Path)
		b.WriteString(`">` + "\n")
		b.WriteString(layer.Content)
		if !strings.HasSuffix(layer.Content, "\n") {
			b.WriteByte('\n')
		}
		b.WriteString("</project_instructions>\n\n")
	}
	b.WriteString("</project_context>\n")
	return b.String(), nil
}

func LoadInstructionLayers(cwd, projectTrustRoot string) ([]InstructionLayer, error) {
	abs, err := filepath.Abs(cwd)
	if err != nil {
		return nil, err
	}
	if resolved, err := filepath.EvalSymlinks(abs); err == nil {
		abs = filepath.Clean(resolved)
	}

	layers := make([]InstructionLayer, 0, 2)
	seen := make(map[string]struct{}, 2)

	home, err := os.UserHomeDir()
	if err != nil {
		return nil, fmt.Errorf("resolve home directory: %w", err)
	}
	if home != "" {
		globalDir := filepath.Join(home, ".ion")
		globalLayers, err := instructionLayerForDir(globalDir)
		if err != nil {
			return nil, fmt.Errorf("load global instructions: %w", err)
		}
		appendInstructionLayers(&layers, seen, globalLayers)
	}

	projectRoot := strings.TrimSpace(projectTrustRoot)
	if projectRoot == "" {
		return layers, nil
	}
	projectRoot, err = filepath.Abs(projectRoot)
	if err != nil {
		return nil, err
	}
	projectRoot = filepath.Clean(projectRoot)
	if resolved, err := filepath.EvalSymlinks(projectRoot); err == nil {
		projectRoot = filepath.Clean(resolved)
	}
	if !pathContains(projectRoot, abs) {
		return layers, nil
	}
	for _, dir := range dirsFromRoot(projectRoot, abs) {
		projectLayers, err := instructionLayerForDir(dir)
		if err != nil {
			return nil, fmt.Errorf("load project instructions from %q: %w", dir, err)
		}
		appendInstructionLayers(&layers, seen, projectLayers)
	}
	return layers, nil
}

func pathContains(root, path string) bool {
	relative, err := filepath.Rel(root, path)
	if err != nil {
		return false
	}
	return relative == "." || (relative != ".." && !strings.HasPrefix(relative, ".."+string(filepath.Separator)))
}

func dirsFromRoot(root, cwd string) []string {
	if root == cwd {
		return []string{root}
	}

	var dirs []string
	for dir := cwd; ; dir = filepath.Dir(dir) {
		dirs = append(dirs, dir)
		if dir == root {
			break
		}
	}

	for i, j := 0, len(dirs)-1; i < j; i, j = i+1, j-1 {
		dirs[i], dirs[j] = dirs[j], dirs[i]
	}
	return dirs
}

func instructionLayerForDir(dir string) ([]InstructionLayer, error) {
	for _, name := range instructionFileNames {
		path := filepath.Join(dir, name)
		if _, err := os.Lstat(path); err != nil {
			if os.IsNotExist(err) {
				continue
			}
			return nil, fmt.Errorf("inspect %q: %w", path, err)
		}
		data, err := os.ReadFile(path)
		if err != nil {
			return nil, fmt.Errorf("read %q: %w", path, err)
		}
		content := string(data)
		if strings.TrimSpace(content) == "" {
			continue
		}
		return []InstructionLayer{{
			Path:    path,
			Content: content,
		}}, nil
	}
	return nil, nil
}

func appendInstructionLayers(
	layers *[]InstructionLayer,
	seen map[string]struct{},
	additional []InstructionLayer,
) {
	for _, layer := range additional {
		path := filepath.Clean(layer.Path)
		if _, ok := seen[path]; ok {
			continue
		}
		seen[path] = struct{}{}
		layer.Path = path
		*layers = append(*layers, layer)
	}
}
