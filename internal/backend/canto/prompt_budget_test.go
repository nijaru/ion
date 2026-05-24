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

const (
	maxCoreRuntimePromptChars = 2_500
	maxP1ToolSpecChars        = 5_000
	maxStaticPreludeChars     = 20_000
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

	if len(specs) != 7 {
		t.Fatalf("P1 tool spec count = %d, want 7", len(specs))
	}
	if len(coreRuntime) > maxCoreRuntimePromptChars {
		t.Fatalf(
			"core runtime prompt = %s, want <= %s",
			sizeWithEstimate(len(coreRuntime)),
			sizeWithEstimate(maxCoreRuntimePromptChars),
		)
	}
	if len(specsJSON) > maxP1ToolSpecChars {
		t.Fatalf(
			"P1 tool specs = %s, want <= %s",
			sizeWithEstimate(len(specsJSON)),
			sizeWithEstimate(maxP1ToolSpecChars),
		)
	}
	if len(withProject)+len(specsJSON) > maxStaticPreludeChars {
		t.Fatalf(
			"static prelude = %s, want <= %s",
			sizeWithEstimate(len(withProject)+len(specsJSON)),
			sizeWithEstimate(maxStaticPreludeChars),
		)
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
		&tools.List{FileTool: *fileTool},
		&tools.Grep{SearchTool: *searchTool},
		&tools.Find{SearchTool: *searchTool},
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
