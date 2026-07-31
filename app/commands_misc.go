package app

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"strings"

	"github.com/nijaru/ion/config"

	tea "charm.land/bubbletea/v2"
	ionskills "github.com/nijaru/ion/internal/skills"
)

func (m Model) logoutProvider() (Model, tea.Cmd) {
	if m.Model.RuntimeSwitchRequest != 0 {
		return m, cmdError(m.localCommandBusyMessage("logging out"))
	}
	cfg, err := m.commandConfig()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	provider := cfg.Provider
	if provider == "" {
		return m, cmdError("no active provider")
	}
	requestID := m.runtimeRequest().begin("Logging out...")
	generation := m.Model.EventGeneration
	return m, func() tea.Msg {
		err := saveProviderKey(provider, "")
		return logoutProviderSavedMsg{
			generation: generation,
			requestID:  requestID,
			provider:   provider,
			err:        err,
		}
	}
}

func (m Model) handleLogoutProviderSaved(msg logoutProviderSavedMsg) (Model, tea.Cmd) {
	if msg.generation != m.Model.EventGeneration || !m.runtimeRequest().matches(msg.requestID) {
		return m, nil
	}
	if !m.runtimeRequest().finish(msg.requestID) {
		return m, nil
	}
	if msg.err != nil {
		return m.handleLocalError(fmt.Errorf("failed to clear API key: %w", msg.err))
	}
	return m, m.terminalCommit().Entries(systemEntry("Logged out from " + msg.provider))
}

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

// === Extracted command handlers ===

func (m Model) handleHelpCommand() (Model, tea.Cmd) {
	return m, m.terminalCommit().Help(helpText())
}

func (m Model) handleHotkeysCommand() (Model, tea.Cmd) {
	return m, m.terminalCommit().Help(hotkeysText())
}

func (m Model) handleExitCommand() (Model, tea.Cmd) {
	return m, tea.Quit
}

func (m Model) handleStatusCommand(fields []string) (Model, tea.Cmd) {
	if len(fields) != 1 {
		return m, cmdError("usage: /status")
	}
	return m, m.terminalCommit().Entries(systemEntry(runtimeStatusSummary(m)))
}

func (m Model) handleChangelogCommand(fields []string) (Model, tea.Cmd) {
	if len(fields) != 1 {
		return m, cmdError("usage: /changelog")
	}
	requestID := m.runtimeRequest().begin("Loading changelog...")
	generation := m.Model.EventGeneration
	ctx := m.Model.runtimeRequestContext
	return m, func() tea.Msg {
		if err := ctx.Err(); err != nil {
			return changelogLoadedMsg{generation: generation, requestID: requestID, err: err}
		}
		content, err := os.ReadFile("CHANGELOG.md")
		if err != nil {
			return changelogLoadedMsg{
				generation: generation,
				requestID:  requestID,
				err:        fmt.Errorf("failed to read CHANGELOG.md: %w", err),
			}
		}
		return changelogLoadedMsg{
			generation: generation,
			requestID:  requestID,
			content:    string(content),
		}
	}
}

func (m Model) handleChangelogLoaded(msg changelogLoadedMsg) (Model, tea.Cmd) {
	if msg.generation != m.Model.EventGeneration || !m.runtimeRequest().matches(msg.requestID) {
		return m, nil
	}
	if !m.runtimeRequest().finish(msg.requestID) {
		return m, nil
	}
	if errors.Is(msg.err, context.Canceled) {
		return m, nil
	}
	if msg.err != nil {
		return m.handleLocalError(msg.err)
	}
	return m, m.terminalCommit().Entries(systemEntry(msg.content))
}

func (m Model) handleSkillsCommand(input, command string) (Model, tea.Cmd) {
	query := strings.TrimSpace(strings.TrimPrefix(input, command))
	requestID := m.runtimeRequest().begin("Loading skills...")
	generation := m.Model.EventGeneration
	ctx := m.Model.runtimeRequestContext
	return m, func() tea.Msg {
		if err := ctx.Err(); err != nil {
			return skillsNoticeLoadedMsg{generation: generation, requestID: requestID, err: err}
		}
		dir, err := config.DefaultSkillsDir()
		if err == nil {
			var out string
			out, err = ionskills.NoticeContext(ctx, []string{dir}, query)
			if err == nil {
				return skillsNoticeLoadedMsg{
					generation: generation,
					requestID:  requestID,
					content:    out,
				}
			}
		}
		if err != nil {
			return skillsNoticeLoadedMsg{
				generation: generation,
				requestID:  requestID,
				err:        fmt.Errorf("failed to load skills: %w", err),
			}
		}
		return skillsNoticeLoadedMsg{generation: generation, requestID: requestID}
	}
}

func (m Model) handleSkillsNoticeLoaded(msg skillsNoticeLoadedMsg) (Model, tea.Cmd) {
	if msg.generation != m.Model.EventGeneration || !m.runtimeRequest().matches(msg.requestID) {
		return m, nil
	}
	if !m.runtimeRequest().finish(msg.requestID) {
		return m, nil
	}
	if errors.Is(msg.err, context.Canceled) {
		return m, nil
	}
	if msg.err != nil {
		return m.handleLocalError(msg.err)
	}
	return m, m.terminalCommit().Entries(systemEntry(msg.content))
}

func (m Model) handleSkillDetailCommand(name string) (Model, tea.Cmd) {
	requestID := m.runtimeRequest().begin("Loading skill...")
	generation := m.Model.EventGeneration
	ctx := m.Model.runtimeRequestContext
	return m, func() tea.Msg {
		if err := ctx.Err(); err != nil {
			return skillDetailLoadedMsg{generation: generation, requestID: requestID, err: err}
		}
		dir, err := config.DefaultSkillsDir()
		if err == nil {
			var detail ionskills.Detail
			detail, err = ionskills.ReadContext(ctx, []string{dir}, name)
			if err == nil {
				return skillDetailLoadedMsg{
					generation: generation,
					requestID:  requestID,
					content:    ionskills.FormatDetail(detail),
				}
			}
		}
		if err != nil {
			return skillDetailLoadedMsg{
				generation: generation,
				requestID:  requestID,
				err:        err,
			}
		}
		return skillDetailLoadedMsg{generation: generation, requestID: requestID}
	}
}

func (m Model) handleSkillDetailLoaded(msg skillDetailLoadedMsg) (Model, tea.Cmd) {
	if msg.generation != m.Model.EventGeneration || !m.runtimeRequest().matches(msg.requestID) {
		return m, nil
	}
	if !m.runtimeRequest().finish(msg.requestID) {
		return m, nil
	}
	if errors.Is(msg.err, context.Canceled) {
		return m, nil
	}
	if msg.err != nil {
		return m.handleLocalError(msg.err)
	}
	return m, m.terminalCommit().Help(msg.content)
}
