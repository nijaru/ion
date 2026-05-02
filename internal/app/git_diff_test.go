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

	updated, _ := model.Update(gitDiffStatsMsg{workdir: "/repo/old", stats: "+2/-1"})
	model = updated.(Model)

	if model.App.GitDiff != "+1" {
		t.Fatalf("git diff stats = %q, want unchanged", model.App.GitDiff)
	}
}
