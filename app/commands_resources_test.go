package app

import (
	"os"
	"path/filepath"
	"testing"

	tea "charm.land/bubbletea/v2"
	ionskills "github.com/nijaru/ion/internal/skills"
)

func TestResourceCommandsDeferFilesystemWork(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	skillPath := filepath.Join(home, ".ion", "skills", "review", "SKILL.md")
	if err := os.MkdirAll(filepath.Dir(skillPath), 0o755); err != nil {
		t.Fatalf("mkdir skill dir: %v", err)
	}
	if err := os.WriteFile(
		skillPath,
		[]byte("---\nname: review\ndescription: Review code.\n---\nReview instructions."),
		0o644,
	); err != nil {
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

func TestSkillCompletionDefersDiscoveryAndDropsReplacementResult(t *testing.T) {
	previous := loadSkillSummaries
	t.Cleanup(func() { loadSkillSummaries = previous })
	started := make(chan struct{})
	loadSkillSummaries = func(...string) ([]ionskills.Summary, error) {
		close(started)
		return []ionskills.Summary{{Name: "review", Description: "Review code."}}, nil
	}

	model := readyModel(t)
	model.Input.Composer.SetValue("//r")
	updatedValue, cmd := model.Update(tea.KeyPressMsg{Code: 'e', Text: "e"})
	if cmd == nil {
		t.Fatal("skill completion returned no command")
	}
	select {
	case <-started:
		t.Fatal("skill discovery started during Bubble Tea Update")
	default:
	}

	messages := runCommandTree(t, cmd)
	select {
	case <-started:
	default:
		t.Fatal("skill completion command did not discover skills")
	}
	var loaded skillCompletionMsg
	var ok bool
	for _, message := range messages {
		if loadedMessage, loadedOK := message.(skillCompletionMsg); loadedOK {
			loaded, ok = loadedMessage, true
			break
		}
	}
	if !ok {
		t.Fatalf("skill completion messages = %#v, want skillCompletionMsg", messages)
	}

	updated := testModel(t, updatedValue)
	updated.resetComposerDraft()
	next, completionCmd := updated.Update(loaded)
	if completionCmd != nil {
		t.Fatal("stale skill completion after reset returned a command")
	}
	if testModel(t, next).Input.Completion != nil {
		t.Fatal("stale skill completion after reset changed the composer")
	}

	updated = testModel(t, updatedValue)
	updated.rotateRuntimeContext()
	updated.runtimeRequest().clear()
	updated.Model.EventGeneration++
	next, completionCmd = updated.Update(loaded)
	if completionCmd != nil {
		t.Fatal("stale skill completion returned a command")
	}
	if testModel(t, next).Input.Completion != nil {
		t.Fatal("stale skill completion changed the replacement runtime")
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
