package export

import (
	"context"
	"encoding/json"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/session"
)

func TestSessionBundleForkIsIndependentAndReopenable(t *testing.T) {
	ctx := context.Background()
	dir := t.TempDir()
	store, err := session.NewSQLiteStore(dir, "canto")
	if err != nil {
		t.Fatal(err)
	}

	userID, err := session.NewSession(store, 16).AppendMessage(ctx, session.NewUserText("source prompt", time.Now()))
	if err != nil {
		t.Fatal(err)
	}
	assistantID, err := session.NewSession(store, 16).AppendMessage(ctx, &session.AssistantMessage{
		Content:    []session.Content{session.TextContent{Text: "source answer"}},
		StopReason: session.StopReasonEndTurn,
		Timestamp:  time.Now(),
	})
	if err != nil {
		t.Fatal(err)
	}
	if userID == assistantID {
		t.Fatal("source entries unexpectedly share an ID")
	}
	if err := store.UpdateSession(ctx, session.SessionInfoEntry{
		EntryBase:   session.EntryBase{ID: assistantID},
		Workdir:     "/repo",
		Model:       "openrouter/test-model",
		Name:        "source session",
		LastPreview: "source prompt",
		UpdatedAt:   time.Now(),
	}); err != nil {
		t.Fatal(err)
	}

	bundle, err := ExportSessionBundle(ctx, store, assistantID)
	if err != nil {
		t.Fatalf("export: %v", err)
	}
	encoded, err := json.Marshal(bundle)
	if err != nil {
		t.Fatal(err)
	}
	decoded, err := DecodeSessionBundle(encoded)
	if err != nil {
		t.Fatalf("decode bundle: %v", err)
	}
	decoded.RootSessionID = ""

	forkID, err := ForkSession(ctx, store, assistantID)
	if err != nil {
		t.Fatalf("fork: %v", err)
	}
	if forkID == assistantID || forkID == userID {
		t.Fatalf("fork leaf %q reused source ID", forkID)
	}
	forkBranch, err := store.Branch(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(forkBranch) != 2 || session.EntryText(forkBranch[0]) != "source prompt" || session.EntryText(forkBranch[1]) != "source answer" {
		t.Fatalf("fork branch = %#v, want copied source conversation", forkBranch)
	}
	if forkBranch[0].ID() == userID || forkBranch[1].ID() == assistantID {
		t.Fatal("fork branch retained source entry IDs")
	}
	info, err := store.GetSessionInfo(ctx, forkID)
	if err != nil {
		t.Fatalf("fork catalog info: %v", err)
	}
	if info.Model != "openrouter/test-model" || info.Name != "source session" {
		t.Fatalf("fork catalog info = %+v, want copied metadata", info)
	}

	importedID, err := ImportSessionBundle(ctx, store, decoded)
	if err != nil {
		t.Fatalf("import: %v", err)
	}
	if importedID == assistantID || importedID == forkID {
		t.Fatalf("import leaf %q reused an existing session", importedID)
	}
	importedBranch, err := store.Branch(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(importedBranch) != 2 || importedBranch[0].ID() == userID || importedBranch[1].ID() == assistantID {
		t.Fatalf("import branch = %#v, want fresh IDs", importedBranch)
	}

	if err := store.Close(); err != nil {
		t.Fatal(err)
	}
	reopened, err := session.NewSQLiteStore(dir, "unused")
	if err != nil {
		t.Fatal(err)
	}
	defer reopened.Close()
	if err := reopened.ResumeSession(ctx, importedID); err != nil {
		t.Fatalf("resume imported session after reopen: %v", err)
	}
	reopenedBranch, err := reopened.Branch(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(reopenedBranch) != 2 || session.EntryText(reopenedBranch[1]) != "source answer" {
		t.Fatalf("reopened imported branch = %#v", reopenedBranch)
	}
}

func TestSessionBundleJSONLImportAndDuplicateRejection(t *testing.T) {
	ctx := context.Background()
	store, err := session.NewSQLiteStore(":memory:", "canto")
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close()

	jsonl := strings.Join([]string{
		`{"type":"session","version":1,"id":"pi-session","timestamp":"2026-07-15T12:00:00Z","cwd":"/repo"}`,
		`{"type":"model_change","id":"model-1","parentId":null,"timestamp":"2026-07-15T12:00:01Z","provider":"openrouter","modelId":"test-model"}`,
		`{"type":"message","id":"message-1","parentId":"model-1","timestamp":"2026-07-15T12:00:02Z","message":{"role":"user","content":[{"type":"text","text":"import me"}],"timestamp":1773144002000}}`,
		`{"type":"thinking_level_change","id":"thinking-1","parentId":"message-1","timestamp":"2026-07-15T12:00:03Z","thinkingLevel":"high"}`,
		`{"type":"active_tools_change","id":"tools-1","parentId":"thinking-1","timestamp":"2026-07-15T12:00:04Z","activeToolNames":["read","bash"]}`,
		`{"type":"compaction","id":"compact-1","parentId":"tools-1","timestamp":"2026-07-15T12:00:05Z","summary":"older context","firstKeptEntryId":"message-1","tokensBefore":42,"details":{"source":"pi"}}`,
	}, "\n")
	bundle, err := DecodeSessionBundle([]byte(jsonl))
	if err != nil {
		t.Fatalf("decode JSONL: %v", err)
	}
	if bundle.RootSessionID != "" || bundle.Sessions[0].Info.Model != "openrouter/test-model" {
		t.Fatalf("JSONL bundle = %+v, want fresh import and model metadata", bundle)
	}
	importedID, err := ImportSessionBundle(ctx, store, bundle)
	if err != nil {
		t.Fatalf("import JSONL: %v", err)
	}
	branch, err := store.Branch(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(branch) != 5 || session.EntryText(branch[1]) != "import me" {
		t.Fatalf("imported JSONL branch = %#v", branch)
	}
	if branch[0].ID() == "model-1" || branch[1].ID() == "message-1" {
		t.Fatal("JSONL import retained source IDs")
	}
	if _, ok := branch[2].(*session.ThinkingChangeEntry); !ok {
		t.Fatalf("JSONL thinking entry = %T, want ThinkingChangeEntry", branch[2])
	}
	tools, ok := branch[3].(*session.ToolsChangeEntry)
	if !ok || len(tools.ActiveTools) != 2 || tools.ActiveTools[0] != "read" {
		t.Fatalf("JSONL tools entry = %#v, want active tools", branch[3])
	}
	compaction, ok := branch[4].(*session.CompactionEntry)
	if !ok || string(compaction.Details) != `{"source":"pi"}` {
		t.Fatalf("JSONL compaction = %#v, want decoded JSON details", branch[4])
	}
	if _, err := store.GetSessionInfo(ctx, importedID); err != nil {
		t.Fatalf("imported JSONL catalog row: %v", err)
	}

	duplicate, err := ExportSessionBundle(ctx, store, importedID)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := ImportSessionBundle(ctx, store, duplicate); err == nil || !strings.Contains(err.Error(), "already exists") {
		t.Fatalf("duplicate import error = %v, want existing-ID rejection", err)
	}
}
