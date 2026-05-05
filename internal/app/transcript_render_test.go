package app

import (
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/charmbracelet/x/ansi"

	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
)

func TestRenderPendingToolEntryHonorsVerbosity(t *testing.T) {
	model := readyModel(t)
	entry := session.Entry{
		Role:    session.Tool,
		Title:   "custom_tool",
		Content: "line 1\nline 2\n",
	}

	model.Model.Config = &config.Config{ToolVerbosity: "hidden"}
	if got := ansi.Strip(model.renderPendingEntry(entry)); strings.Contains(got, "line 1") {
		t.Fatalf("hidden pending tool output rendered content: %q", got)
	}

	model.Model.Config = &config.Config{ToolVerbosity: "collapsed"}
	if got := ansi.Strip(model.renderPendingEntry(entry)); !strings.Contains(got, "...") ||
		strings.Contains(got, "line 1") {
		t.Fatalf("collapsed pending tool output = %q, want ellipsis without content", got)
	}

	model.Model.Config = &config.Config{ToolVerbosity: "full"}
	if got := ansi.Strip(model.renderPendingEntry(entry)); !strings.Contains(got, "line 1") ||
		!strings.Contains(got, "line 2") {
		t.Fatalf("full pending tool output missing content: %q", got)
	}
}

func TestRenderBashToolHidesOutputByDefault(t *testing.T) {
	model := readyModel(t)
	entry := session.Entry{
		Role:    session.Tool,
		Title:   "bash go test ./...",
		Content: "ok github.com/nijaru/ion/internal/app\n",
	}

	got := ansi.Strip(model.renderEntry(entry))
	if got != "• Bash(go test ./...)" {
		t.Fatalf("default bash render = %q, want command only", got)
	}
}

func TestRenderEntryDoesNotDisplayTimestamp(t *testing.T) {
	model := readyModel(t)
	entry := session.Entry{
		Role:      session.User,
		Timestamp: time.Date(2026, 5, 2, 14, 30, 0, 0, time.UTC),
		Content:   "hello",
	}

	got := ansi.Strip(model.renderEntry(entry))
	if got != "› hello" {
		t.Fatalf("rendered entry = %q, want no timestamp", got)
	}
}

func TestRenderBashToolCanShowSummarizedOutput(t *testing.T) {
	model := readyModel(t)
	model.Model.Config = &config.Config{BashOutput: "summary"}
	entry := session.Entry{
		Role:    session.Tool,
		Title:   "bash go test ./...",
		Content: "ok github.com/nijaru/ion/internal/app\nok github.com/nijaru/ion/internal/config\n",
	}

	got := ansi.Strip(model.renderEntry(entry))
	if got != "• Bash(go test ./...) · 2 lines" {
		t.Fatalf("summary bash render = %q, want line count", got)
	}
}

func TestRenderBashToolCanShowFullOutput(t *testing.T) {
	model := readyModel(t)
	model.Model.Config = &config.Config{BashOutput: "full"}
	entry := session.Entry{
		Role:    session.Tool,
		Title:   "bash go test ./...",
		Content: "ok github.com/nijaru/ion/internal/app\n",
	}

	got := ansi.Strip(model.renderEntry(entry))
	if !strings.Contains(got, "Bash(go test ./...)\n") ||
		!strings.Contains(got, "ok github.com/nijaru/ion/internal/app") {
		t.Fatalf("full bash render = %q, want output", got)
	}
}

func TestRenderRoutineToolEntryCompactsByDefault(t *testing.T) {
	model := readyModel(t)
	entry := session.Entry{
		Role:    session.Tool,
		Title:   "read AGENTS.md",
		Content: "line 1\nline 2\nline 3\n",
	}

	got := ansi.Strip(model.renderEntry(entry))
	if got != "• Read(AGENTS.md) · 3 lines" {
		t.Fatalf("routine tool render = %q, want compact summary", got)
	}
}

func TestRenderPendingRoutineToolEntryCompactsByDefault(t *testing.T) {
	model := readyModel(t)
	entry := session.Entry{
		Role:    session.Tool,
		Title:   "read AGENTS.md",
		Content: "line 1\nline 2\nline 3\n",
	}

	got := ansi.Strip(model.renderPendingEntry(entry))
	if got != "• Read(AGENTS.md) · 3 lines" {
		t.Fatalf("pending routine tool render = %q, want compact summary", got)
	}
}

func TestRenderRoutineToolUsesSemanticSummaryMetrics(t *testing.T) {
	model := readyModel(t)
	tests := []struct {
		name  string
		entry session.Entry
		want  string
	}{
		{
			name: "list entries",
			entry: session.Entry{
				Role:    session.Tool,
				Title:   "list internal/app",
				Content: "model.go\nviewport.go\n",
			},
			want: "• List(internal/app) · 2 entries",
		},
		{
			name: "glob entries",
			entry: session.Entry{
				Role:    session.Tool,
				Title:   "glob **/*.go",
				Content: "main.go\ninternal/app/model.go\n",
			},
			want: "• Find(**/*.go) · 2 entries",
		},
		{
			name: "grep matches",
			entry: session.Entry{
				Role:    session.Tool,
				Title:   "grep TODO",
				Content: "file.go\n12:TODO\n",
			},
			want: "• Search(TODO) · 2 matches",
		},
		{
			name: "grep no matches",
			entry: session.Entry{
				Role:    session.Tool,
				Title:   "grep missing",
				Content: "No matches found.",
			},
			want: "• Search(missing) · 0 matches",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := ansi.Strip(model.renderEntry(tt.entry)); got != tt.want {
				t.Fatalf("render = %q, want %q", got, tt.want)
			}
		})
	}
}

func TestRenderRoutineToolEntryCanShowFullOutput(t *testing.T) {
	model := readyModel(t)
	model.Model.Config = &config.Config{ReadOutput: "full"}
	entry := session.Entry{
		Role:    session.Tool,
		Title:   "read",
		Content: "line 1\nline 2\n",
	}

	got := ansi.Strip(model.renderEntry(entry))
	if !strings.Contains(got, "line 1") || !strings.Contains(got, "line 2") {
		t.Fatalf("full routine tool render = %q, want original content", got)
	}
	if strings.Contains(got, "\n    line") {
		t.Fatalf("full routine tool render = %q, want two-space output indentation", got)
	}
}

func TestRenderRoutineToolEntryCanHideReadOutput(t *testing.T) {
	model := readyModel(t)
	model.Model.Config = &config.Config{ReadOutput: "hidden"}
	entry := session.Entry{
		Role:    session.Tool,
		Title:   "read AGENTS.md",
		Content: "line 1\nline 2\n",
	}

	got := ansi.Strip(model.renderEntry(entry))
	if got != "• Read(AGENTS.md)" {
		t.Fatalf("hidden read render = %q, want call only", got)
	}
}

func TestRenderEntriesCanExpandReplayedRoutineToolOutput(t *testing.T) {
	model := readyModel(t)
	model.Model.Config = &config.Config{ReadOutput: "full"}
	entries := []session.Entry{
		{Role: session.User, Content: "read file"},
		{Role: session.Tool, Title: "read", Content: "line 1\nline 2\nline 3"},
		{Role: session.Agent, Content: "done"},
	}

	got := ansi.Strip(strings.Join(model.RenderEntries(entries...), "\n"))
	if !strings.Contains(got, "line 1\n  line 2\n  line 3") {
		t.Fatalf("replayed routine tool render = %q, want full content", got)
	}
	if strings.Contains(got, " · 3 lines") {
		t.Fatalf("replayed routine tool render = %q, want no compact summary in full mode", got)
	}
	if !strings.Contains(got, "› read file\n\n• Read") ||
		!strings.Contains(got, "line 3\n\n• done") {
		t.Fatalf("replayed routine tool render = %q, want one blank row between entries", got)
	}
}

func TestRenderWriteToolSummarizesByDefault(t *testing.T) {
	model := readyModel(t)
	entry := session.Entry{
		Role:    session.Tool,
		Title:   "write hello.md",
		Content: "Successfully wrote 5 bytes to hello.md\nCheckpoint: 1777656162183207000-c25e58778d2941d5",
	}

	got := ansi.Strip(model.renderEntry(entry))
	if got != "• Write(hello.md)" {
		t.Fatalf("summary write render = %q, want call only", got)
	}
}

func TestRenderWriteToolCanShowDiff(t *testing.T) {
	model := readyModel(t)
	model.Model.Config = &config.Config{WriteOutput: "diff"}
	entry := session.Entry{
		Role:    session.Tool,
		Title:   "write AGENTS.md",
		Content: "--- AGENTS.md\n+++ AGENTS.md\n@@\n+line\n",
	}

	got := ansi.Strip(model.renderEntry(entry))
	if !strings.Contains(got, "Write(AGENTS.md)\n") || !strings.Contains(got, "+line") {
		t.Fatalf("diff write render = %q, want expanded content", got)
	}
}

func TestRenderRoutineToolEntryPreservesErrors(t *testing.T) {
	model := readyModel(t)
	entry := session.Entry{
		Role:    session.Tool,
		Title:   "grep",
		Content: "grep failed\npattern missing\n",
		IsError: true,
	}

	got := ansi.Strip(model.renderEntry(entry))
	if !strings.Contains(got, "grep failed") || strings.Contains(got, "... (2 lines)") {
		t.Fatalf("error routine tool render = %q, want full error content", got)
	}
}

func TestRenderThinkingEntryHidesReasoningByDefault(t *testing.T) {
	model := readyModel(t)
	entry := session.Entry{
		Role:      session.Agent,
		Reasoning: "private chain of thought",
		Content:   "answer",
	}

	got := ansi.Strip(model.renderEntry(entry))
	if strings.Contains(got, "Thinking") || strings.Contains(got, "...") {
		t.Fatalf("thinking render = %q, want answer without thinking marker", got)
	}
	if strings.Contains(got, "private chain of thought") {
		t.Fatalf("thinking render leaked reasoning: %q", got)
	}
}

func TestRenderThinkingEntryCanCollapseReasoning(t *testing.T) {
	model := readyModel(t)
	model.Model.Config = &config.Config{ThinkingVerbosity: "collapsed"}
	entry := session.Entry{
		Role:      session.Agent,
		Reasoning: "private chain of thought",
		Content:   "answer",
	}

	got := ansi.Strip(model.renderEntry(entry))
	if !strings.Contains(got, "• Thinking...") {
		t.Fatalf("collapsed thinking render = %q, want one-line thinking marker", got)
	}
	if strings.Contains(got, "\n    ...") {
		t.Fatalf("collapsed thinking render = %q, want no separate ellipsis row", got)
	}
	if strings.Contains(got, "private chain of thought") {
		t.Fatalf("collapsed thinking render leaked reasoning: %q", got)
	}
}

func TestRenderReasoningOnlyEntryShowsMarkerWhenThinkingHidden(t *testing.T) {
	model := readyModel(t)
	entry := session.Entry{
		Role:      session.Agent,
		Reasoning: "private chain of thought",
	}

	got := ansi.Strip(model.renderEntry(entry))
	if got != "• Thinking" {
		t.Fatalf("reasoning-only render = %q, want marker", got)
	}
	if strings.Contains(got, "private chain of thought") {
		t.Fatalf("reasoning-only render leaked reasoning: %q", got)
	}
}

func TestRenderThinkingEntryCanShowFullReasoning(t *testing.T) {
	model := readyModel(t)
	model.Model.Config = &config.Config{ThinkingVerbosity: "full"}
	entry := session.Entry{
		Role:      session.Agent,
		Reasoning: "visible reasoning",
		Content:   "answer",
	}

	got := ansi.Strip(model.renderEntry(entry))
	if !strings.Contains(got, "visible reasoning") {
		t.Fatalf("full thinking render = %q, want reasoning text", got)
	}
}

func TestRenderPlaneBThinkingHidesReasoningByDefault(t *testing.T) {
	model := readyModel(t)
	model.InFlight.ReasonBuf = "private chain of thought"

	got := ansi.Strip(model.renderPlaneB())
	if !strings.Contains(got, "Thinking...") {
		t.Fatalf("plane b thinking = %q, want thinking marker", got)
	}
	if strings.Contains(got, "\n    ...") {
		t.Fatalf("plane b thinking = %q, want no default ellipsis row", got)
	}
	if strings.Contains(got, "private chain of thought") {
		t.Fatalf("plane b thinking leaked reasoning: %q", got)
	}
}

func TestRenderAgentMarkdownDoesNotIndentContinuationLines(t *testing.T) {
	model := readyModel(t)
	entry := session.Entry{
		Role:    session.Agent,
		Content: "Read.\n\n- first\n- second",
	}

	got := ansi.Strip(model.renderEntry(entry))
	if strings.Contains(got, "\n  - first") || strings.Contains(got, "\n  - second") {
		t.Fatalf("agent render = %q, want markdown continuation lines unindented", got)
	}
	if !strings.Contains(got, "\n- first") || !strings.Contains(got, "\n- second") {
		t.Fatalf("agent render = %q, want bullet lines preserved", got)
	}
}

func TestRenderAgentMarkdownPreservesGFMInlineNodes(t *testing.T) {
	model := readyModel(t)
	entry := session.Entry{
		Role: session.Agent,
		Content: strings.Join([]string{
			"- [x] keep task markers",
			"- [ ] keep unchecked markers",
			"",
			"See <https://example.com> and ~~old wording~~.",
		}, "\n"),
	}

	got := ansi.Strip(model.renderEntry(entry))
	for _, want := range []string{
		"- [x] keep task markers",
		"- [ ] keep unchecked markers",
		"https://example.com",
		"old wording",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("agent render = %q, want %q", got, want)
		}
	}
}

func TestRenderAgentMarkdownPreservesTableInlineNodes(t *testing.T) {
	model := readyModel(t)
	model.App.Width = 120
	entry := session.Entry{
		Role: session.Agent,
		Content: strings.Join([]string{
			"| Command | Link | Done |",
			"| --- | --- | --- |",
			"| `go test ./...` | <https://example.com> | [x] |",
		}, "\n"),
	}

	got := ansi.Strip(model.renderEntry(entry))
	for _, want := range []string{
		"go test ./...",
		"https://example.com",
		"[x]",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("agent render = %q, want table cell %q", got, want)
		}
	}
}

func TestFormatToolTitleUsesReadableLabels(t *testing.T) {
	if got := FormatToolTitle("read", `{"file_path":"AGENTS.md"}`); got != "Read(AGENTS.md)" {
		t.Fatalf("read title = %q, want readable title", got)
	}
	if got := FormatToolTitle("bash", `{"command":"go test ./..."}`); got != "Bash(go test ./...)" {
		t.Fatalf("bash title = %q, want readable title", got)
	}
	if got := FormatToolTitle("unknown", `{"nested":{"x":1}}`); got != "unknown" {
		t.Fatalf("fallback title = %q, want tool name only", got)
	}
}

func TestToolCallStartedShortensWorkspacePath(t *testing.T) {
	workdir := "/Users/nick/github/nijaru/ion"
	model := readyModel(t)
	model.App.Workdir = workdir

	updated, _ := model.Update(session.ToolCallStarted{
		ToolUseID: "tool-read",
		ToolName:  "read",
		Args:      `{"file_path":` + strconv.Quote(filepath.Join(workdir, "AGENTS.md")) + `}`,
	})
	model = updated.(Model)

	got := model.InFlight.PendingTools["tool-read"].Title
	if got != "Read(AGENTS.md)" {
		t.Fatalf("tool title = %q, want workspace-relative path", got)
	}
}

func TestRenderToolLabelColorsOnlyStatusMarker(t *testing.T) {
	model := readyModel(t)

	rendered := model.renderEntry(session.Entry{
		Role:  session.Tool,
		Title: "bash(sleep 5; echo ion-queued)",
	})
	stripped := ansi.Strip(rendered)
	if stripped != "• Bash(sleep 5; echo ion-queued)" {
		t.Fatalf("tool label = %q, want call-style label", stripped)
	}
	if strings.Contains(rendered, "Bash(sleep 5; echo ion-queued)\x1b") {
		t.Fatalf("tool label appears styled through the full call text: %q", rendered)
	}
}

func TestRenderToolLabelShortensLongWorkspacePath(t *testing.T) {
	workdir := filepath.Join(t.TempDir(), "repo")
	model := readyModel(t)
	model.App.Workdir = workdir
	model.App.Width = 28

	rendered := model.renderEntry(session.Entry{
		Role:  session.Tool,
		Title: "read " + filepath.Join(workdir, "internal", "app", "model_test.go"),
	})
	stripped := ansi.Strip(rendered)
	if ansi.StringWidth(stripped) > model.App.Width {
		t.Fatalf(
			"tool label width = %d, want <= %d: %q",
			ansi.StringWidth(stripped),
			model.App.Width,
			stripped,
		)
	}
	if !strings.Contains(stripped, "…/app/model_test.go") {
		t.Fatalf("tool label = %q, want shortened file suffix", stripped)
	}
}
