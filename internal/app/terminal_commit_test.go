package app

import (
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/session"
)

func TestTerminalCommitOwnsBubbleTeaPrintBoundary(t *testing.T) {
	_, file, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("runtime caller unavailable")
	}
	dir := filepath.Dir(file)
	if !filepath.IsAbs(dir) {
		dir = "."
	}
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatalf("read app dir: %v", err)
	}
	for _, entry := range entries {
		name := entry.Name()
		if entry.IsDir() ||
			!strings.HasSuffix(name, ".go") ||
			strings.HasSuffix(name, "_test.go") ||
			name == "terminal_commit.go" {
			continue
		}
		path := filepath.Join(dir, name)
		data, err := os.ReadFile(path)
		if err != nil {
			t.Fatalf("read %s: %v", name, err)
		}
		source := string(data)
		if strings.Contains(source, "tea.Printf") ||
			strings.Contains(source, "tea.Println") {
			t.Fatalf("%s bypasses terminal_commit.go print boundary", name)
		}
	}
}

func TestTerminalCommitDefersEveryScrollbackCommit(t *testing.T) {
	model := readyModel(t)
	commits := []struct {
		name string
		cmd  tea.Cmd
	}{
		{
			name: "entries",
			cmd: model.terminalCommit().Entries(
				session.Entry{Role: session.System, Content: "notice"},
			),
		},
		{name: "help", cmd: model.terminalCommit().Help("help text")},
		{name: "lines", cmd: model.terminalCommit().Lines("line text")},
		{name: "deferred lines", cmd: model.terminalCommit().DeferredLines("line text")},
	}

	for _, commit := range commits {
		t.Run(commit.name, func(t *testing.T) {
			if commit.cmd == nil {
				t.Fatal("commit command is nil")
			}
			msg := commit.cmd()
			if _, ok := msg.(terminalCommitLinesMsg); !ok {
				t.Fatalf("commit command returned %T, want terminalCommitLinesMsg", msg)
			}
		})
	}
}
