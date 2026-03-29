package backend

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

type InstructionLayer struct {
	Path    string
	Content string
}

func BuildInstructions(base, cwd string) (string, error) {
	base = strings.TrimSpace(base)
	if cwd == "" {
		return base, nil
	}

	layers, err := LoadInstructionLayers(cwd)
	if err != nil {
		return "", err
	}
	if len(layers) == 0 {
		return base, nil
	}

	var b strings.Builder
	b.WriteString(base)
	b.WriteString("\n\n## Project Instructions\n")
	for _, layer := range layers {
		b.WriteString("\n### ")
		b.WriteString(layer.Path)
		b.WriteString("\n")
		b.WriteString(layer.Content)
		if !strings.HasSuffix(layer.Content, "\n") {
			b.WriteByte('\n')
		}
	}
	return b.String(), nil
}

func LoadInstructionLayers(cwd string) ([]InstructionLayer, error) {
	abs, err := filepath.Abs(cwd)
	if err != nil {
		return nil, err
	}

	root, ok := findRepoRoot(abs)
	if !ok {
		return instructionLayerForDir(abs)
	}

	dirs := dirsFromRoot(root, abs)
	layers := make([]InstructionLayer, 0, len(dirs))
	for _, dir := range dirs {
		layer, err := instructionLayerForDir(dir)
		if err != nil {
			return nil, err
		}
		if len(layer) != 0 {
			layers = append(layers, layer...)
		}
	}
	return layers, nil
}

func findRepoRoot(cwd string) (string, bool) {
	dir := cwd
	for {
		gitPath := filepath.Join(dir, ".git")
		if _, err := os.Stat(gitPath); err == nil {
			return dir, true
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			return "", false
		}
		dir = parent
	}
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
	for _, name := range []string{"AGENTS.md", "GEMINI.md", "CLAUDE.md"} {
		path := filepath.Join(dir, name)
		data, err := os.ReadFile(path)
		if err != nil {
			if os.IsNotExist(err) {
				continue
			}
			return nil, fmt.Errorf("read instruction file %s: %w", path, err)
		}
		content := strings.TrimSpace(string(data))
		if content == "" {
			return nil, nil
		}
		return []InstructionLayer{{
			Path:    path,
			Content: content,
		}}, nil
	}
	return nil, nil
}
