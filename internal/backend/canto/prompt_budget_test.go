package canto

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/nijaru/canto/llm"
	ionbackend "github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/backend/canto/tools"
)

func TestPromptPreludeBudgetReport(t *testing.T) {
	root := repoRoot(t)
	now := time.Date(2026, time.May, 2, 0, 0, 0, 0, time.UTC)

	core := baseInstructions()
	runtime := runtimeInstructions(root, now)
	coreRuntime := buildInstructions(root, now)
	withProject, err := ionbackend.BuildInstructions(coreRuntime, root)
	if err != nil {
		t.Fatalf("build instructions: %v", err)
	}

	specs := p1ToolSpecs(root)
	specsJSON, err := json.Marshal(specs)
	if err != nil {
		t.Fatalf("marshal tool specs: %v", err)
	}

	if len(specs) != 8 {
		t.Fatalf("P1 tool spec count = %d, want 8", len(specs))
	}

	t.Logf(
		"prompt budget: core=%s, runtime=%s, core+runtime=%s, project_layers=%s, p1_tool_specs=%s, total_current_static=%s",
		sizeWithEstimate(len(core)),
		sizeWithEstimate(len(runtime)),
		sizeWithEstimate(len(coreRuntime)),
		sizeWithEstimate(len(withProject)-len(coreRuntime)),
		sizeWithEstimate(len(specsJSON)),
		sizeWithEstimate(len(withProject)+len(specsJSON)),
	)
}

func p1ToolSpecs(cwd string) []llm.Spec {
	fileTool := tools.NewFileTool(cwd)
	searchTool := tools.NewSearchTool(cwd)
	registered := []interface{ Spec() llm.Spec }{
		tools.NewBash(cwd),
		&tools.Read{FileTool: *fileTool},
		&tools.Write{FileTool: *fileTool},
		&tools.Edit{FileTool: *fileTool},
		&tools.MultiEdit{FileTool: *fileTool},
		&tools.List{FileTool: *fileTool},
		&tools.Grep{SearchTool: *searchTool},
		&tools.Glob{SearchTool: *searchTool},
	}
	specs := make([]llm.Spec, 0, len(registered))
	for _, t := range registered {
		specs = append(specs, t.Spec())
	}
	return specs
}

func sizeWithEstimate(chars int) string {
	tokens := (chars + 3) / 4
	return fmt.Sprintf("%d chars/~%d tok", chars, tokens)
}

func repoRoot(t *testing.T) string {
	t.Helper()
	dir, err := os.Getwd()
	if err != nil {
		t.Fatalf("get cwd: %v", err)
	}
	for {
		if _, err := os.Stat(filepath.Join(dir, "go.mod")); err == nil {
			return dir
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			t.Fatal("go.mod not found")
		}
		dir = parent
	}
}
