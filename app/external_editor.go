package app

import (
	"fmt"
	"os"
	"os/exec"
	"strings"

	tea "charm.land/bubbletea/v2"
)

type externalEditorFinishedMsg struct {
	content string
	err     error
}

var (
	externalEditorName            = externalEditor
	writeExternalEditorBufferFile = writeExternalEditorBuffer
)

func (m Model) openExternalEditor() (Model, tea.Cmd) {
	if m.localCommandBusy() {
		return m, m.terminalCommit().Entries(
			systemEntry(m.localCommandBusyMessage("opening the external editor")),
		)
	}
	return m, openExternalEditorCmd(m.expandMarkers(m.Input.Composer.Value()))
}

func openExternalEditorCmd(content string) tea.Cmd {
	return func() tea.Msg {
		path, err := writeExternalEditorBufferFile(content)
		if err != nil {
			return externalEditorFinishedMsg{err: err}
		}

		editor := externalEditorName()
		cmd := externalEditorCommand(editor, path)
		return tea.ExecProcess(cmd, func(runErr error) tea.Msg {
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
		})()
	}
}

func (m Model) handleExternalEditorFinished(msg externalEditorFinishedMsg) (Model, tea.Cmd) {
	if msg.err != nil {
		return m, m.terminalCommit().Entries(systemEntry("Editor failed: " + msg.err.Error()))
	}
	cmd := m.setComposerDraft(msg.content)
	m.resetHistoryCursor()
	m.clearPasteMarkers()
	return m, cmd
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
