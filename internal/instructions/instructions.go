package instructions

import (
	"os"
	"path/filepath"
	"strings"
)

type InstructionLayer struct {
	Path    string
	Content string
}

var instructionFileNames = []string{
	"AGENTS.md",
	"AGENTS.MD",
	"CLAUDE.md",
	"CLAUDE.MD",
	"GEMINI.md",
	"GEMINI.MD",
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

func LoadInstructionLayers(cwd string) ([]InstructionLayer, error) {
	abs, err := filepath.Abs(cwd)
	if err != nil {
		return nil, err
	}

	dirs := dirsFromRoot(filepath.VolumeName(abs)+string(filepath.Separator), abs)
	layers := make([]InstructionLayer, 0, len(dirs)+1)
	seen := make(map[string]struct{}, len(dirs)+1)

	if home, err := os.UserHomeDir(); err == nil && home != "" {
		globalDir := filepath.Join(home, ".ion")
		appendInstructionLayers(&layers, seen, instructionLayerForDir(globalDir))
	}

	for _, dir := range dirs {
		appendInstructionLayers(&layers, seen, instructionLayerForDir(dir))
	}
	return layers, nil
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

func instructionLayerForDir(dir string) []InstructionLayer {
	for _, name := range instructionFileNames {
		path := filepath.Join(dir, name)
		data, err := os.ReadFile(path)
		if err != nil {
			if os.IsNotExist(err) {
				continue
			}
			continue
		}
		content := strings.TrimSpace(string(data))
		if content == "" {
			continue
		}
		return []InstructionLayer{{
			Path:    path,
			Content: content,
		}}
	}
	return nil
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
