package app

import (
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/session"
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
			(name == "render.go" || name == "terminal_commit.go") {
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

func TestStaleTerminalCommitCannotFlushAfterRuntimeReplacement(t *testing.T) {
	model := readyModel(t)
	model.Model.EventGeneration++

	next, cmd, handled := model.dispatchAppControlMessage(terminalCommitLinesMsg{
		generation: model.Model.EventGeneration - 1,
		lines:      []string{"stale output"},
	})
	if !handled {
		t.Fatal("stale terminal commit was not handled")
	}
	if cmd != nil {
		t.Fatal("stale terminal commit returned a flush command")
	}
	if next.App.PrintedTranscript {
		t.Fatal("stale terminal commit changed transcript state")
	}
}

func TestTerminalCommitPromotesDurableEntryOnlyOnce(t *testing.T) {
	model := readyModel(t)
	entry := &session.MessageEntry{
		EntryBase: session.EntryBase{ID: "entry-1"},
		Message:   &session.UserMessage{Content: []session.Content{session.TextContent{Text: "hello"}}},
	}
	if cmd := model.terminalCommit().Entries(entry); cmd == nil {
		t.Fatal("first entry commit returned nil")
	}
	if cmd := model.terminalCommit().Entries(entry); cmd != nil {
		t.Fatal("duplicate entry commit returned a flush command")
	}
}

func TestTerminalCommitCorrelatesEphemeralAndDurableMessage(t *testing.T) {
	model := readyModel(t)
	ts := time.Unix(123, 0)
	live := &session.MessageEntry{
		EntryBase: session.EntryBase{Timestamp: ts},
		Message:   &session.UserMessage{Timestamp: ts, Content: []session.Content{session.TextContent{Text: "hello"}}},
	}
	durable := &session.MessageEntry{
		EntryBase: session.EntryBase{ID: "durable-1", Timestamp: ts},
		Message:   &session.UserMessage{Timestamp: ts, Content: []session.Content{session.TextContent{Text: "hello"}}},
	}
	msg, ok := model.terminalCommit().Entries(live)().(terminalCommitLinesMsg)
	if !ok {
		t.Fatal("live commit did not produce deferred message")
	}
	model, _, _ = model.dispatchAppControlMessage(msg)
	if cmd := model.terminalCommit().Entries(durable); cmd != nil {
		t.Fatal("durable replay of live message returned a flush command")
	}
}

func TestTerminalReplaySkipsPrintedDurableEntries(t *testing.T) {
	model := readyModel(t)
	entry := &session.MessageEntry{
		EntryBase: session.EntryBase{ID: "entry-1"},
		Message:   &session.UserMessage{Content: []session.Content{session.TextContent{Text: "hello"}}},
	}
	model = model.WithPrintedEntries([]session.Entry{entry})
	if cmd := model.terminalCommit().SwitchReplay(nil, []session.Entry{entry}, "", ""); cmd != nil {
		t.Fatal("replay of printed entry returned a flush command")
	}
}

func TestStaleTerminalCommitDoesNotCrossProjectionEpoch(t *testing.T) {
	model := readyModel(t)
	entry := &session.MessageEntry{
		EntryBase: session.EntryBase{ID: "entry-epoch"},
		Message:   &session.UserMessage{Content: []session.Content{session.TextContent{Text: "old"}}},
	}
	commit := model.terminalCommit().Entries(entry)
	stale, ok := commit().(terminalCommitLinesMsg)
	if !ok {
		t.Fatalf("deferred commit = %T, want terminalCommitLinesMsg", commit())
	}
	model.clearPrintedEntries()
	next, flush, handled := model.dispatchAppControlMessage(stale)
	if !handled || flush != nil {
		t.Fatalf("stale commit = handled=%t flush=%v, want handled without flush", handled, flush)
	}
	if next.Model.terminalCommitEpoch == stale.epoch {
		t.Fatal("projection epoch did not advance")
	}
	if cmd := next.terminalCommit().Entries(entry); cmd == nil {
		t.Fatal("stale commit consumed the entry reservation")
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
				sysEntry("notice"),
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

func requireTerminalCommitContains(t *testing.T, cmd tea.Cmd, want string) {
	t.Helper()
	if cmd == nil {
		t.Fatal("terminal commit command is nil")
	}
	msg := cmd()
	commit, ok := msg.(terminalCommitLinesMsg)
	if !ok {
		t.Fatalf("terminal commit returned %T, want terminalCommitLinesMsg", msg)
	}
	if output := strings.Join(commit.lines, "\n"); !strings.Contains(output, want) {
		t.Fatalf("terminal commit output = %q, want substring %q", output, want)
	}
}
