package main

import (
	"bytes"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"github.com/nijaru/ion/internal/storage"
)

func TestMain(m *testing.M) {
	if os.Getenv("ION_TEST_MAIN") == "1" {
		main()
		return
	}
	os.Exit(m.Run())
}

func TestSessionBundleFileExportImport(t *testing.T) {
	ctx := t.Context()
	exportStore, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new export store: %v", err)
	}
	defer exportStore.Close()

	sess, err := exportStore.OpenSession(ctx, "/tmp/ion-session-bundle", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}
	if err := sess.Append(ctx, storage.System{Content: "portable note"}); err != nil {
		t.Fatalf("append system note: %v", err)
	}

	path := filepath.Join(t.TempDir(), "session-bundle.json")
	exported, err := exportSessionBundleFile(ctx, exportStore, sess.ID(), path)
	if err != nil {
		t.Fatalf("export bundle file: %v", err)
	}
	if exported.Path != path {
		t.Fatalf("exported path = %q, want %q", exported.Path, path)
	}
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read bundle file: %v", err)
	}
	if !strings.Contains(string(raw), `"checksum"`) {
		t.Fatalf("bundle file missing checksum: %s", raw)
	}

	importStore, err := storage.NewCantoStore(t.TempDir())
	if err != nil {
		t.Fatalf("new import store: %v", err)
	}
	defer importStore.Close()

	imported, err := importSessionBundleFile(ctx, importStore, path)
	if err != nil {
		t.Fatalf("import bundle file: %v", err)
	}
	if len(imported) != 1 || imported[0].ID != sess.ID() {
		t.Fatalf("imported = %#v, want session %s", imported, sess.ID())
	}
	resumed, err := importStore.ResumeSession(ctx, sess.ID())
	if err != nil {
		t.Fatalf("resume imported session: %v", err)
	}
	entries, err := resumed.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 1 || entries[0].Content != "portable note" {
		t.Fatalf("entries = %#v, want portable note", entries)
	}
	if _, err := importSessionBundleFile(ctx, importStore, path); !errors.Is(
		err,
		storage.ErrSessionBundleConflict,
	) {
		t.Fatalf("second import error = %v, want conflict", err)
	}
}

func TestSessionBundleCLIImportExportSmoke(t *testing.T) {
	ctx := t.Context()
	exportHome := t.TempDir()
	exportDataDir := filepath.Join(exportHome, ".ion", "data")
	exportStore, err := storage.NewCantoStore(exportDataDir)
	if err != nil {
		t.Fatalf("new export store: %v", err)
	}
	sess, err := exportStore.OpenSession(ctx, "/tmp/ion-session-bundle", "model-a", "main")
	if err != nil {
		t.Fatalf("open session: %v", err)
	}
	if err := sess.Append(ctx, storage.System{Content: "cli portable note"}); err != nil {
		t.Fatalf("append system note: %v", err)
	}
	if err := exportStore.Close(); err != nil {
		t.Fatalf("close export store: %v", err)
	}

	bundlePath := filepath.Join(t.TempDir(), "cli-session-bundle.json")
	exe, err := os.Executable()
	if err != nil {
		t.Fatalf("test executable: %v", err)
	}
	exportCmd := exec.Command(exe, "--resume", sess.ID(), "--export-session", bundlePath)
	exportCmd.Env = append(os.Environ(), "ION_TEST_MAIN=1", "HOME="+exportHome)
	if out, err := exportCmd.CombinedOutput(); err != nil {
		t.Fatalf("export command failed: %v\n%s", err, out)
	} else if !strings.Contains(string(out), "Exported session "+sess.ID()) {
		t.Fatalf("export output = %q, want session id", out)
	}

	importHome := t.TempDir()
	importCmd := exec.Command(exe, "--import-session", bundlePath)
	importCmd.Env = append(os.Environ(), "ION_TEST_MAIN=1", "HOME="+importHome)
	if out, err := importCmd.CombinedOutput(); err != nil {
		t.Fatalf("import command failed: %v\n%s", err, out)
	} else if !strings.Contains(string(out), "Imported session "+sess.ID()) {
		t.Fatalf("import output = %q, want session id", out)
	}

	importStore, err := storage.NewCantoStore(filepath.Join(importHome, ".ion", "data"))
	if err != nil {
		t.Fatalf("new import store: %v", err)
	}
	defer importStore.Close()
	resumed, err := importStore.ResumeSession(ctx, sess.ID())
	if err != nil {
		t.Fatalf("resume imported session: %v", err)
	}
	entries, err := resumed.Entries(ctx)
	if err != nil {
		t.Fatalf("entries: %v", err)
	}
	if len(entries) != 1 || entries[0].Content != "cli portable note" {
		t.Fatalf("entries = %#v, want cli portable note", entries)
	}
}

func TestSessionBundleFlagValidationRejectsBothDirections(t *testing.T) {
	err := validateSessionBundleSelection("out.json", "in.json")
	if err == nil || !strings.Contains(err.Error(), "cannot be used together") {
		t.Fatalf("validate error = %v, want mutual exclusion", err)
	}
	if err := validateSessionBundleSelection("out.json", ""); err != nil {
		t.Fatalf("export-only validation: %v", err)
	}
	if err := validateSessionBundleSelection("", "in.json"); err != nil {
		t.Fatalf("import-only validation: %v", err)
	}
}

func TestPrintSessionBundleMessages(t *testing.T) {
	var out bytes.Buffer
	printSessionBundleImport(&out, []storage.SessionInfo{{ID: "one"}, {ID: "two"}})
	if got := out.String(); !strings.Contains(got, "Imported 2 sessions") ||
		!strings.Contains(got, "- one") ||
		!strings.Contains(got, "- two") {
		t.Fatalf("import output = %q", got)
	}
}
