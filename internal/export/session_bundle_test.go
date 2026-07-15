package export

import (
	"context"
	"encoding/json"
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
	var decoded SessionBundle
	if err := json.Unmarshal(encoded, &decoded); err != nil {
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
	if _, err := ImportSessionBundle(ctx, store, bundle); err == nil {
		t.Fatal("duplicate exact import succeeded")
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
