package app

import (
	"fmt"
	"os"
	"os/exec"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/session"
)

type externalEditorFinishedMsg struct {
	content string
	err     error
}

func (m Model) openExternalEditor() (Model, tea.Cmd) {
	if m.InFlight.Thinking || m.Progress.Compacting || m.Approval.Pending != nil {
		return m, m.printEntries(session.Entry{
			Role:    session.System,
			Content: "External editor is unavailable while a turn is active",
		})
	}
	path, err := writeExternalEditorBuffer(m.expandMarkers(m.Input.Composer.Value()))
	if err != nil {
		return m, m.printEntries(session.Entry{
			Role:    session.System,
			Content: "Editor failed: " + err.Error(),
		})
	}
	editor := externalEditor()
	cmd := externalEditorCommand(editor, path)
	return m, tea.ExecProcess(cmd, func(runErr error) tea.Msg {
		defer os.Remove(path)
		if runErr != nil {
			return externalEditorFinishedMsg{
				err: fmt.Errorf("%s failed: %w", editor, runErr),
			}
		}
		data, err := os.ReadFile(path)
		if err != nil {
			return externalEditorFinishedMsg{
				err: fmt.Errorf("read editor buffer: %w", err),
			}
		}
		return externalEditorFinishedMsg{content: string(data)}
	})
}

func (m Model) handleExternalEditorFinished(msg externalEditorFinishedMsg) (Model, tea.Cmd) {
	if msg.err != nil {
		return m, m.printEntries(session.Entry{
			Role:    session.System,
			Content: "Editor failed: " + msg.err.Error(),
		})
	}
	m.Input.Composer.SetValue(msg.content)
	m.Input.HistoryIdx = -1
	m.Input.HistoryDraft = ""
	m.PasteMarkers = make(map[string]pasteMarker)
	m.relayoutComposer()
	return m, nil
}

func externalEditor() string {
	if editor := strings.TrimSpace(os.Getenv("VISUAL")); editor != "" {
		return editor
	}
	if editor := strings.TrimSpace(os.Getenv("EDITOR")); editor != "" {
		return editor
	}
	return "vi"
}

func externalEditorCommand(editor, path string) *exec.Cmd {
	cmd := exec.Command("sh", "-c", `$ION_EDITOR "$ION_COMPOSER_FILE"`)
	cmd.Env = append(os.Environ(), "ION_EDITOR="+editor, "ION_COMPOSER_FILE="+path)
	return cmd
}

func writeExternalEditorBuffer(content string) (string, error) {
	file, err := os.CreateTemp("", "ion-composer-*.md")
	if err != nil {
		return "", err
	}
	path := file.Name()
	if _, err := file.WriteString(content); err != nil {
		_ = file.Close()
		_ = os.Remove(path)
		return "", err
	}
	if err := file.Close(); err != nil {
		_ = os.Remove(path)
		return "", err
	}
	return path, nil
}
