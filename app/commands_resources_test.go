package app

import (
	"os"
	"path/filepath"
	"testing"
)

func TestResourceCommandsDeferFilesystemWork(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	skillPath := filepath.Join(home, ".ion", "skills", "review", "SKILL.md")
	if err := os.MkdirAll(filepath.Dir(skillPath), 0o755); err != nil {
		t.Fatalf("mkdir skill dir: %v", err)
	}
	if err := os.WriteFile(skillPath, []byte("---\nname: review\ndescription: Review code.\n---\nReview instructions."), 0o644); err != nil {
		t.Fatalf("write skill: %v", err)
	}

	model := readyModel(t)
	for _, command := range []string{"/changelog", "/skills", "//review"} {
		updated, cmd := model.handleCommand(command)
		if cmd == nil {
			t.Fatalf("%s returned no command", command)
		}
		if updated.Model.RuntimeSwitchRequest == 0 {
			t.Fatalf("%s did not enter a request before filesystem work", command)
		}
		msg := cmd()
		switch command {
		case "/changelog":
			if _, ok := msg.(changelogLoadedMsg); !ok {
				t.Fatalf("%s message = %T, want changelogLoadedMsg", command, msg)
			}
		case "/skills":
			if _, ok := msg.(skillsNoticeLoadedMsg); !ok {
				t.Fatalf("%s message = %T, want skillsNoticeLoadedMsg", command, msg)
			}
		case "//review":
			if _, ok := msg.(skillDetailLoadedMsg); !ok {
				t.Fatalf("%s message = %T, want skillDetailLoadedMsg", command, msg)
			}
		}
	}
}

func TestResourceCompletionCannotReportAfterRuntimeReplacement(t *testing.T) {
	model := readyModel(t)
	updated, cmd := model.handleCommand("/changelog")
	if cmd == nil {
		t.Fatal("changelog returned no command")
	}
	updated.rotateRuntimeContext()
	updated.runtimeRequest().clear()
	updated.Model.EventGeneration++

	msg := cmd()
	_, terminalCmd := updated.Update(msg)
	if terminalCmd != nil {
		t.Fatalf("stale changelog completion returned command %T", terminalCmd)
	}
}
