package app

import (
	"context"
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

func (m Model) resumeStoredSessionByID(sessionID string) tea.Cmd {
	if m.Model.Store == nil {
		return cmdError("session store not available")
	}

	resumed, err := m.Model.Store.ResumeSession(context.Background(), sessionID)
	if err != nil {
		return cmdError(fmt.Sprintf("failed to resume session %s: %v", sessionID, err))
	}
	defer func() {
		_ = resumed.Close()
	}()

	meta := resumed.Meta()
	provider, model := splitStoredSessionModel(meta.Model)
	if provider == "" || model == "" {
		return cmdError(fmt.Sprintf("session %s is missing provider/model metadata", sessionID))
	}

	cfg := &config.Config{Provider: provider, Model: model}
	notice := session.Entry{Role: session.System, Content: "Resumed session " + sessionID}
	return m.resumeRuntimeCommand(cfg, notice, sessionID)
}

func (m Model) switchPresetCommand(preset modelPreset) (Model, tea.Cmd) {
	cfg, err := m.commandConfig()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	runtimeCfg, err := m.runtimeConfigForPreset(cfg, preset)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to resolve %s preset: %v", preset, err))
	}
	notice := session.Entry{Role: session.System, Content: "Switched to " + preset.String()}
	return m, m.switchRuntimeCommand(
		runtimeCfg,
		cfg,
		preset,
		notice,
		m.currentMaterializedSessionID(),
		false,
	)
}

func (m Model) currentMaterializedSessionID() string {
	if m.Model.Session == nil {
		return ""
	}
	if m.Model.Storage == nil {
		return m.Model.Session.ID()
	}
	if !storage.IsMaterialized(m.Model.Storage) {
		return ""
	}
	return strings.TrimSpace(m.Model.Storage.ID())
}

func (m Model) switchRuntimeCommand(
	cfg *config.Config,
	appCfg *config.Config,
	preset modelPreset,
	notice session.Entry,
	sessionID string,
	preserveSession bool,
) tea.Cmd {
	if m.Model.Switcher == nil {
		if err := config.SaveActivePreset(preset.String()); err != nil {
			return persistErrorCmd("save active preset", err)
		}
		m.Model.Backend.SetConfig(cfg)
		m.App.ActivePreset = preset
		m.Progress.ReasoningEffort = normalizeThinkingValue(cfg.ReasoningEffort)
		return m.printEntries(notice)
	}

	oldSession := m.Model.Session
	switchID := sessionID
	if preserveSession && switchID == "" && oldSession != nil {
		switchID = oldSession.ID()
	}
	switcher := m.Model.Switcher
	cfgCopy := *cfg
	appCfgCopy := cfgCopy
	if appCfg != nil {
		appCfgCopy = *appCfg
	}

	return func() tea.Msg {
		if oldSession != nil {
			_ = oldSession.CancelTurn(context.Background())
		}
		backend, sess, storageSess, err := switcher(context.Background(), &cfgCopy, switchID)
		if err != nil {
			return localErrorMsg{err: err}
		}
		if err := config.SaveActivePreset(preset.String()); err != nil {
			closeSwitchedRuntime(sess, storageSess)
			return localErrorMsg{err: fmt.Errorf("save active preset: %w", err)}
		}
		if oldSession != nil {
			_ = oldSession.Close()
		}
		return runtimeSwitchedMsg{
			cfg:        &appCfgCopy,
			reasoning:  cfgCopy.ReasoningEffort,
			preset:     preset,
			backend:    backend,
			session:    sess,
			storage:    storageSess,
			status:     backend.Bootstrap().Status,
			notice:     notice.Content,
			showStatus: preserveSession,
		}
	}
}

func (m Model) resumeRuntimeCommand(
	cfg *config.Config,
	notice session.Entry,
	sessionID string,
) tea.Cmd {
	if m.Model.Switcher == nil {
		m.Model.Backend.SetConfig(cfg)
		m.App.ActivePreset = presetPrimary
		m.Progress.ReasoningEffort = normalizeThinkingValue(cfg.ReasoningEffort)
		return m.printEntries(notice)
	}
	switcher := m.Model.Switcher
	cfgCopy := *cfg
	return func() tea.Msg {
		oldSession := m.Model.Session
		if oldSession != nil {
			_ = oldSession.CancelTurn(context.Background())
		}
		backend, sess, storageSess, err := switcher(context.Background(), &cfgCopy, sessionID)
		if err != nil {
			return localErrorMsg{err: err}
		}
		var entries []session.Entry
		resumeBranch := currentBranchName(m.App.Branch, storageSess)
		if storageSess != nil {
			entries, err = storageSess.Entries(context.Background())
			if err != nil {
				closeSwitchedRuntime(sess, storageSess)
				return localErrorMsg{err: fmt.Errorf("load session transcript: %w", err)}
			}
		}
		if oldSession != nil {
			_ = oldSession.Close()
		}
		printLines := []string{m.runtimeHeaderLine(backend)}
		if header := m.headerLineFor(resumeBranch); header != "" {
			printLines = append(printLines, header)
		}
		printLines = append(printLines, "", "--- resumed ---", "")
		return runtimeSwitchedMsg{
			cfg:           &cfgCopy,
			preset:        presetPrimary,
			backend:       backend,
			session:       sess,
			storage:       storageSess,
			printLines:    printLines,
			replayEntries: entries,
			status:        backend.Bootstrap().Status,
			notice:        notice.Content,
			showStatus:    false,
		}
	}
}

func closeSwitchedRuntime(sess session.AgentSession, storageSess storage.Session) {
	if sess != nil {
		_ = sess.Close()
	}
	if storageSess != nil {
		_ = storageSess.Close()
	}
}

func currentBranchName(defaultBranch string, sess storage.Session) string {
	if sess == nil {
		return defaultBranch
	}
	if branch := strings.TrimSpace(sess.Meta().Branch); branch != "" {
		return branch
	}
	return defaultBranch
}

func splitStoredSessionModel(value string) (string, string) {
	value = strings.TrimSpace(value)
	if value == "" {
		return "", ""
	}
	provider, model, ok := strings.Cut(value, "/")
	if !ok {
		return "", value
	}
	return strings.TrimSpace(provider), strings.TrimSpace(model)
}
