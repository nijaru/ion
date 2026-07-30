package app

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
)

type stubMemoryController struct {
	records        []MemoryRecord
	queries        []string
	includeDeleted []bool
	deleted        []string
	restored       []string
	err            error
	searchStarted  chan struct{}
}

func (s *stubMemoryController) Search(
	ctx context.Context,
	query string,
	includeDeleted bool,
	_ int,
) ([]MemoryRecord, error) {
	if s.searchStarted != nil {
		close(s.searchStarted)
		<-ctx.Done()
		return nil, ctx.Err()
	}
	s.queries = append(s.queries, query)
	s.includeDeleted = append(s.includeDeleted, includeDeleted)
	return append([]MemoryRecord(nil), s.records...), s.err
}

func (s *stubMemoryController) Audit(context.Context, int) ([]MemoryAuditRecord, error) {
	return nil, s.err
}

func (s *stubMemoryController) Delete(_ context.Context, id string) error {
	s.deleted = append(s.deleted, id)
	return s.err
}

func (s *stubMemoryController) Restore(_ context.Context, id string) error {
	s.restored = append(s.restored, id)
	return s.err
}

func TestMemoryCommandUsesExplicitOperations(t *testing.T) {
	controller := &stubMemoryController{
		records: []MemoryRecord{{
			ID:        "mem_1",
			Content:   "keep this note",
			Tags:      "design",
			CreatedAt: time.Date(2026, 7, 16, 1, 2, 3, 0, time.UTC),
		}},
	}
	model := readyModel(t).WithMemory(controller)

	model, cmd := model.handleCommand("/memory search session tree")
	if cmd == nil {
		t.Fatal("search command returned no command")
	}
	if model.Model.MemoryRequest != 1 {
		t.Fatalf("memory request = %d, want 1", model.Model.MemoryRequest)
	}
	searchResult := cmd()
	if _, ok := searchResult.(memorySearchMsg); !ok {
		t.Fatalf("search result = %T, want memorySearchMsg", searchResult)
	}
	if len(controller.queries) != 1 || controller.queries[0] != "session tree" {
		t.Fatalf("queries %#v", controller.queries)
	}
	if controller.includeDeleted[0] {
		t.Fatal("search unexpectedly included deleted memory")
	}

	model, cmd = model.handleCommand("/memory forget mem_1")
	if cmd == nil {
		t.Fatal("forget command returned no command")
	}
	if result, ok := cmd().(memoryActionMsg); !ok || result.action != "forgot" || result.id != "mem_1" {
		t.Fatalf("forget result = %#v", result)
	}
	if len(controller.deleted) != 1 || controller.deleted[0] != "mem_1" {
		t.Fatalf("deleted %#v", controller.deleted)
	}
	model, cmd = model.handleCommand("/memory restore mem_1")
	if cmd == nil {
		t.Fatal("restore command returned no command")
	}
	if result, ok := cmd().(memoryActionMsg); !ok || result.action != "restored" || result.id != "mem_1" {
		t.Fatalf("restore result = %#v", result)
	}
	if len(controller.restored) != 1 || controller.restored[0] != "mem_1" {
		t.Fatalf("restored %#v", controller.restored)
	}
	model, cmd = model.handleCommand("/memory all")
	if cmd == nil {
		t.Fatal("all command returned no command")
	}
	if result, ok := cmd().(memorySearchMsg); !ok || !result.includeDeleted {
		t.Fatalf("all result = %#v", result)
	}
	if len(controller.queries) != 2 || !controller.includeDeleted[1] {
		t.Fatalf("queries %#v, includeDeleted %#v", controller.queries, controller.includeDeleted)
	}
	model, cmd = model.handleCommand("/memory audit")
	if cmd == nil {
		t.Fatal("audit command returned no command")
	}
	if result, ok := cmd().(memoryAuditMsg); !ok {
		t.Fatalf("audit result = %T, want memoryAuditMsg", result)
	}
}

func TestStaleMemoryResultCannotReportAfterRuntimeReplacement(t *testing.T) {
	controller := &stubMemoryController{records: []MemoryRecord{{ID: "mem_1"}}}
	model := readyModel(t).WithMemory(controller)
	_, cmd := model.handleCommand("/memory search note")
	if cmd == nil {
		t.Fatal("memory search returned no command")
	}
	result := cmd()
	msg, ok := result.(memorySearchMsg)
	if !ok {
		t.Fatalf("memory result = %T, want memorySearchMsg", result)
	}
	model.Model.EventGeneration++

	updated, resultCmd := model.update(msg)
	if resultCmd != nil {
		t.Fatal("stale memory result returned a terminal command")
	}
	if updated.Progress.LastError != "" {
		t.Fatalf("stale memory result reported error: %q", updated.Progress.LastError)
	}
}

func TestMemoryCommandUsesRuntimeContext(t *testing.T) {
	controller := &stubMemoryController{searchStarted: make(chan struct{})}
	model := readyModel(t).WithMemory(controller)
	_, cmd := model.handleCommand("/memory search note")
	if cmd == nil {
		t.Fatal("memory search returned no command")
	}

	resultCh := make(chan tea.Msg, 1)
	go func() { resultCh <- cmd() }()
	select {
	case <-controller.searchStarted:
	case <-time.After(time.Second):
		t.Fatal("memory search did not start")
	}
	model.Close()

	select {
	case result := <-resultCh:
		msg, ok := result.(memorySearchMsg)
		if !ok || !errors.Is(msg.err, context.Canceled) {
			t.Fatalf("canceled memory search = %#v, want context canceled", result)
		}
	case <-time.After(time.Second):
		t.Fatal("memory search did not observe runtime cancellation")
	}
}

func TestMemoryCommandReportsUnavailableAndUsageErrors(t *testing.T) {
	model := readyModel(t)
	_, cmd := model.handleCommand("/memory")
	if cmd == nil {
		t.Fatal("unconfigured memory command returned no error")
	}
	if err := localErrorFromMsg(t, cmd()); err == nil || err.Error() != "workspace memory is unavailable" {
		t.Fatalf("unconfigured memory error = %v", err)
	}

	controller := &stubMemoryController{}
	model = model.WithMemory(controller)
	_, cmd = model.handleCommand("/memory forget")
	if cmd == nil {
		t.Fatal("invalid memory usage returned no error")
	}
	if err := localErrorFromMsg(
		t,
		cmd(),
	); err == nil ||
		err.Error() != "usage: /memory [search <query>|audit|forget <id>|restore <id>|all]" {
		t.Fatalf("usage error = %v", err)
	}

	controller.err = errors.New("memory unavailable")
	model, cmd = model.handleCommand("/memory search note")
	if cmd == nil {
		t.Fatal("memory backend error returned no command")
	}
	updated, resultCmd := model.update(cmd())
	_ = updated
	if err := localErrorFromMsg(t, resultCmd()); err == nil || err.Error() != "memory unavailable" {
		t.Fatalf("backend error = %v", err)
	}
}

func TestMemoryOutputEscapesTerminalControls(t *testing.T) {
	got := formatMemoryRecords([]MemoryRecord{{
		ID:      "mem_1",
		Tags:    "tag\x1b]52;c;secret\a",
		Content: "safe\x1b[31m red\x1b[0m\nnext",
	}}, false)
	if strings.ContainsAny(got, "\x1b\a") {
		t.Fatalf("memory output contains terminal controls: %q", got)
	}
	if !strings.Contains(got, `\u001b`) || !strings.Contains(got, `\u0007`) {
		t.Fatalf("memory output did not visibly escape controls: %q", got)
	}
}

func TestFormatMemoryRecords(t *testing.T) {
	got := formatMemoryRecords([]MemoryRecord{{
		ID:        "mem_1",
		Content:   "first line\nsecond line",
		Tags:      "tag",
		CreatedAt: time.Date(2026, 7, 16, 1, 2, 3, 0, time.UTC),
		Deleted:   true,
	}}, true)
	want := "mem_1  deleted  [tag]  2026-07-16 01:02:03Z\n  first line\n  second line"
	if got != want {
		t.Fatalf("formatted memory = %q, want %q", got, want)
	}
}

func TestFormatMemoryRecordsIsBounded(t *testing.T) {
	got := formatMemoryRecords(
		[]MemoryRecord{{ID: "mem_1", Content: strings.Repeat("x", maxMemoryDisplayBytes)}},
		false,
	)
	if len(got) > maxMemoryDisplayBytes+100 {
		t.Fatalf("formatted memory length = %d, want bounded output", len(got))
	}
	if !strings.Contains(got, "memory output truncated") {
		t.Fatalf("bounded memory output has no truncation notice: %q", got[len(got)-100:])
	}
}
