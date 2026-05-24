package tooldisplay

import (
	"path/filepath"
	"strings"
	"testing"

	"github.com/charmbracelet/x/ansi"
)

func TestTitleShortensWorkspaceAbsolutePath(t *testing.T) {
	workdir := filepath.Join(t.TempDir(), "repo")
	path := filepath.Join(workdir, "internal", "app", "model.go")

	got := Title("read", `{"file_path":`+quote(path)+`}`, Options{Workdir: workdir})
	if got != "Read(internal/app/model.go)" {
		t.Fatalf("title = %q, want workspace-relative path", got)
	}
}

func TestTitleCleansRelativePath(t *testing.T) {
	got := Title("write", `{"file_path":"./internal/../hello.md"}`, Options{})
	if got != "Write(hello.md)" {
		t.Fatalf("title = %q, want cleaned relative path", got)
	}
}

func TestTitleAcceptsPiPathAlias(t *testing.T) {
	tests := []struct {
		name string
		args string
		want string
	}{
		{name: "read", args: `{"path":"./internal/../hello.md"}`, want: "Read(hello.md)"},
		{name: "write", args: `{"path":"./internal/../hello.md"}`, want: "Write(hello.md)"},
		{name: "edit", args: `{"path":"./internal/../hello.md"}`, want: "Edit(hello.md)"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := Title(tt.name, tt.args, Options{}); got != tt.want {
				t.Fatalf("title = %q, want %q", got, tt.want)
			}
		})
	}
}

func TestTitleShortensHomePathOutsideWorkspace(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	path := filepath.Join(home, "notes", "todo.md")

	got := Title(
		"read",
		`{"file_path":`+quote(path)+`}`,
		Options{Workdir: filepath.Join(t.TempDir(), "repo")},
	)
	if got != "Read(~/notes/todo.md)" {
		t.Fatalf("title = %q, want home-shortened path", got)
	}
}

func TestTitlePreservesOutsideAbsolutePath(t *testing.T) {
	path := filepath.Join(t.TempDir(), "outside", "file.md")

	got := Title(
		"read",
		`{"file_path":`+quote(path)+`}`,
		Options{Workdir: filepath.Join(t.TempDir(), "repo")},
	)
	if !strings.HasPrefix(got, "Read(/") || !strings.HasSuffix(got, "/outside/file.md)") {
		t.Fatalf("title = %q, want absolute outside path", got)
	}
}

func TestTitleKeepsCommandAndQueryText(t *testing.T) {
	if got := Title("bash", `{"command":"go test ./..."}`, Options{}); got != "Bash(go test ./...)" {
		t.Fatalf("bash title = %q, want literal command", got)
	}
	if got := Title("grep", `{"pattern":"func Render","path":"."}`, Options{}); got != "Search(func Render)" {
		t.Fatalf("grep title = %q, want pattern when present", got)
	}
	if got := Title("grep", `{"pattern":"func Render"}`, Options{}); got != "Search(func Render)" {
		t.Fatalf("grep pattern title = %q, want pattern fallback", got)
	}
}

func TestTitleMiddleShortensLongPath(t *testing.T) {
	got := Title(
		"read",
		`{"file_path":"internal/app/components/transcript/very-long/model_test.go"}`,
		Options{Width: 24},
	)
	if got != "Read(…/model_test.go)" {
		t.Fatalf("title = %q, want middle-shortened file suffix", got)
	}
	if width := ansi.StringWidth(strings.TrimPrefix(strings.TrimSuffix(got, ")"), "Read(")); width > 24 {
		t.Fatalf("arg width = %d, want <= 24 in %q", width, got)
	}
}

func TestNormalizeTitleUsesSamePathRules(t *testing.T) {
	workdir := filepath.Join(t.TempDir(), "repo")
	path := filepath.Join(workdir, "AGENTS.md")

	got := NormalizeTitle("read "+path, Options{Workdir: workdir})
	if got != "Read(AGENTS.md)" {
		t.Fatalf("normalized title = %q, want workspace-relative path", got)
	}
}

func TestNormalizeTitleShortensDisplayCaseToolPath(t *testing.T) {
	got := NormalizeTitle("Read(internal/app/model_test.go)", Options{Width: 26})
	if got != "Read(…/app/model_test.go)" {
		t.Fatalf("normalized title = %q, want shortened path inside display-case title", got)
	}
}

func quote(value string) string {
	escaped := strings.ReplaceAll(value, `\`, `\\`)
	escaped = strings.ReplaceAll(escaped, `"`, `\"`)
	return `"` + escaped + `"`
}
