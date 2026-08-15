package app

import (
	"strings"
	"testing"

	"github.com/charmbracelet/x/ansi"
)

func TestParseGitDiffShortstat(t *testing.T) {
	tests := map[string]string{
		"":                                   "",
		" 1 file changed, 2 insertions(+)\n": "+2",
		" 1 file changed, 1 deletion(-)\n":   "-1",
		" 3 files changed, 42 insertions(+), 11 deletions(-)": "+42/-11",
	}
	for input, want := range tests {
		if got := parseGitDiffShortstat(input); got != want {
			t.Fatalf("parseGitDiffShortstat(%q) = %q, want %q", input, got, want)
		}
	}
}

func TestStatusLineIncludesGitDiffStats(t *testing.T) {
	model := readyModel(t)
	model.App.GitDiff = "+42/-11"

	line := ansi.Strip(model.statusLine())
	if !strings.Contains(line, "+42/-11") {
		t.Fatalf("status line = %q, want git diff stats", line)
	}
}

func TestGitDiffStatsMessageIgnoresStaleWorkspace(t *testing.T) {
	model := readyModel(t)
	model.App.Workdir = "/repo/current"
	model.App.GitDiff = "+1"

	updated, _ := model.Update(gitDiffStatsMsg{
		generation: model.Model.EventGeneration,
		workdir:    "/repo/old",
		stats:      "+2/-1",
	})
	model = testModel(t, updated)

	if model.App.GitDiff != "+1" {
		t.Fatalf("git diff stats = %q, want unchanged", model.App.GitDiff)
	}
}

func TestGitDiffStatsMessageIgnoresStaleRuntime(t *testing.T) {
	model := readyModel(t)
	model.App.Workdir = "/repo/current"
	model.App.GitDiff = "+1"
	model.Model.EventGeneration = 2

	updated, _ := model.Update(gitDiffStatsMsg{
		generation: 1,
		workdir:    model.App.Workdir,
		stats:      "+2/-1",
	})
	model = testModel(t, updated)

	if model.App.GitDiff != "+1" {
		t.Fatalf("stale git diff stats = %q, want unchanged", model.App.GitDiff)
	}
}

func TestRenderDiffIntraLineHighlight(t *testing.T) {
	model := readyModel(t)
	diffInput := "--- a/file.go\n+++ b/file.go\n@@ -1,2 +1,2 @@\n-func hello(name string) error\n+func hello(name, title string) error"

	rendered := model.renderDiff(diffInput)
	stripped := ansi.Strip(rendered)

	if !strings.Contains(stripped, "-func hello(name string) error") {
		t.Fatalf("stripped diff missing removed line: %q", stripped)
	}
	if !strings.Contains(stripped, "+func hello(name, title string) error") {
		t.Fatalf("stripped diff missing added line: %q", stripped)
	}
	// Verify raw rendered output contains ANSI formatting
	if rendered == stripped {
		t.Fatal("expected rendered diff to contain ANSI escape sequences")
	}
}
