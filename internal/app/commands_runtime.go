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

func (m Model) resumeStoredSessionByID(sessionID string) (Model, tea.Cmd) {
	if m.Model.Store == nil {
		return m, cmdError("session store not available")
	}

	resumed, err := m.Model.Store.ResumeSession(context.Background(), sessionID)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to resume session %s: %v", sessionID, err))
	}
	defer func() {
		_ = resumed.Close()
	}()

	meta := resumed.Meta()
	provider, model := splitStoredSessionModel(meta.Model)
	if provider == "" || model == "" {
		return m, cmdError(fmt.Sprintf("session %s is missing provider/model metadata", sessionID))
	}

	cfg := &config.Config{Provider: provider, Model: model}
	notice := session.Entry{Role: session.System, Content: "Resumed session " + sessionID}
	return m.resumeRuntimeCommand(cfg, notice, sessionID)
}

func (m Model) switchPresetCommand(preset modelPreset) (Model, tea.Cmd) {
	if m.localCommandBusy() {
		return m, cmdError("Finish or cancel the current turn before changing presets.")
	}
	cfg, err := m.commandConfig()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	runtimeCfg, err := m.runtimeConfigForPreset(cfg, preset)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to resolve %s preset: %v", preset, err))
	}
	notice := session.Entry{Role: session.System, Content: "Switched to " + preset.String()}
	transition := newRuntimeTransition(cfg, runtimeCfg, preset, "")
	return m.switchRuntimeCommand(
		transition,
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

func (m Model) ResumeSessionID() string {
	return m.currentMaterializedSessionID()
}

func (m Model) switchRuntimeCommand(
	transition runtimeTransition,
	notice session.Entry,
	sessionID string,
	preserveSession bool,
) (Model, tea.Cmd) {
	transition = transition.withActivePresetPersistence()

	if m.Model.Switcher == nil {
		var err error
		m, err = m.commitRuntimeTransition(transition)
		if err != nil {
			return m, runtimeTransitionErrorCmd(err)
		}
		return m, m.printEntries(notice)
	}

	oldSession := m.Model.Session
	oldStorage := m.Model.Storage
	targetSessionID := sessionID
	if preserveSession && targetSessionID == "" && oldSession != nil {
		targetSessionID = oldSession.ID()
	}
	switcher := m.Model.Switcher
	cfgCopy := transition.snapshot.backendConfig
	appCfgCopy := transition.snapshot.appConfig
	preset := transition.snapshot.preset
	m.Model.RuntimeSwitchRequest++
	requestID := m.Model.RuntimeSwitchRequest
	m.Progress.Status = "Switching runtime..."

	return m, func() tea.Msg {
		if oldSession != nil {
			_ = oldSession.CancelTurn(context.Background())
		}
		backend, sess, storageSess, err := switcher(context.Background(), &cfgCopy, targetSessionID)
		if err != nil {
			return runtimeSwitchErrorMsg{switchID: requestID, err: err}
		}
		if err := transition.persist(); err != nil {
			closeSwitchedRuntime(sess, storageSess)
			return runtimeSwitchErrorMsg{
				switchID: requestID,
				err:      err,
			}
		}
		return runtimeSwitchedMsg{
			switchID:   requestID,
			cfg:        &appCfgCopy,
			runtimeCfg: &cfgCopy,
			preset:     preset,
			backend:    backend,
			session:    sess,
			storage:    storageSess,
			oldSession: oldSession,
			oldStorage: oldStorage,
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
) (Model, tea.Cmd) {
	transition := resumeSelectionTransition(cfg)

	if m.Model.Switcher == nil {
		var err error
		m, err = m.commitRuntimeTransition(transition)
		if err != nil {
			return m, runtimeTransitionErrorCmd(err)
		}
		return m, m.printEntries(notice)
	}
	switcher := m.Model.Switcher
	cfgCopy := transition.snapshot.backendConfig
	appCfgCopy := transition.snapshot.appConfig
	preset := transition.snapshot.preset
	m.Model.RuntimeSwitchRequest++
	switchID := m.Model.RuntimeSwitchRequest
	oldStorage := m.Model.Storage
	m.Progress.Status = "Switching runtime..."
	return m, func() tea.Msg {
		oldSession := m.Model.Session
		if oldSession != nil {
			_ = oldSession.CancelTurn(context.Background())
		}
		backend, sess, storageSess, err := switcher(context.Background(), &cfgCopy, sessionID)
		if err != nil {
			return runtimeSwitchErrorMsg{switchID: switchID, err: err}
		}
		var entries []session.Entry
		resumeBranch := currentBranchName(m.App.Branch, storageSess)
		if storageSess != nil {
			entries, err = storageSess.Entries(context.Background())
			if err != nil {
				closeSwitchedRuntime(sess, storageSess)
				return runtimeSwitchErrorMsg{
					switchID: switchID,
					err:      fmt.Errorf("load session transcript: %w", err),
				}
			}
		}
		printLines := []string{m.runtimeHeaderLine(backend)}
		if header := m.headerLineFor(resumeBranch); header != "" {
			printLines = append(printLines, header)
		}
		printLines = append(printLines, "", "--- resumed ---", "")
		if err := transition.persist(); err != nil {
			closeSwitchedRuntime(sess, storageSess)
			return runtimeSwitchErrorMsg{
				switchID: switchID,
				err:      err,
			}
		}
		return runtimeSwitchedMsg{
			switchID:      switchID,
			cfg:           &appCfgCopy,
			runtimeCfg:    &cfgCopy,
			preset:        preset,
			backend:       backend,
			session:       sess,
			storage:       storageSess,
			oldSession:    oldSession,
			oldStorage:    oldStorage,
			printLines:    printLines,
			replayEntries: entries,
			status:        backend.Bootstrap().Status,
			notice:        notice.Content,
			showStatus:    false,
		}
	}
}

func (m Model) handleRuntimeSwitched(msg runtimeSwitchedMsg) (Model, tea.Cmd) {
	if msg.switchID != 0 && msg.switchID != m.Model.RuntimeSwitchRequest {
		closeSwitchedRuntime(msg.session, msg.storage)
		return m, nil
	}

	m.applyRuntimeSwitched(msg)
	cmds := m.runtimeSwitchedCommands(msg)
	return m, tea.Sequence(cmds...)
}

func (m *Model) applyRuntimeSwitched(msg runtimeSwitchedMsg) {
	m.Model.RuntimeSwitchRequest = 0
	preset := msg.preset
	if preset == "" {
		preset = presetPrimary
	}
	m.Model.Backend = msg.backend
	m.Model.Session = msg.session
	m.Model.Storage = msg.storage
	if msg.cfg != nil || msg.runtimeCfg != nil {
		snapshot := newRuntimeSnapshot(msg.cfg, msg.runtimeCfg, preset, msg.status)
		m.applyRuntimeSnapshot(snapshot)
	} else {
		m.Model.Config = nil
		m.App.ActivePreset = preset
		m.Progress.Status = msg.status
	}
	if msg.oldSession != nil {
		_ = msg.oldSession.Close()
	}
	if msg.oldStorage != nil {
		_ = msg.oldStorage.Close()
	}
	m.Model.EventGeneration++
	m.App.Sandbox = backendSandboxSummary(msg.backend)
	m.Picker.Overlay = nil
	m.Picker.Session = nil
	m.clearProgressError()
	if msg.storage != nil {
		meta := msg.storage.Meta()
		m.App.Branch = meta.Branch
	}
	m.clearActiveTurnState(true)
	m.Progress.Mode = stateReady
	m.Progress.LastTurnSummary = turnSummary{}
	m.clearPendingAction()
	m.Progress.TokensSent = 0
	m.Progress.TokensReceived = 0
	m.Progress.TotalCost = 0
	if msg.storage != nil {
		if input, output, cost, err := msg.storage.Usage(context.Background()); err == nil {
			m.Progress.TokensSent = input
			m.Progress.TokensReceived = output
			m.Progress.TotalCost = cost
		}
	}
	m.resetHistoryCursor()
}

func (m *Model) runtimeSwitchedCommands(msg runtimeSwitchedMsg) []tea.Cmd {
	cmds := make([]tea.Cmd, 0, 5)
	if len(msg.printLines) > 0 {
		m.App.PrintedTranscript = true
		cmds = append(cmds, printLinesCmd(msg.printLines...))
	}
	if len(msg.replayEntries) > 0 {
		cmds = append(cmds, m.printEntries(msg.replayEntries...))
	}
	if strings.TrimSpace(msg.notice) != "" {
		cmds = append(
			cmds,
			m.printEntries(session.Entry{Role: session.System, Content: msg.notice}),
		)
	}
	if msg.showStatus && strings.TrimSpace(msg.status) != "" && !isConfigurationStatus(msg.status) {
		cmds = append(
			cmds,
			m.printEntries(session.Entry{Role: session.System, Content: msg.status}),
		)
	}
	return append(cmds, m.awaitSessionEvent())
}

func (m Model) handleRuntimeSwitchError(msg runtimeSwitchErrorMsg) (Model, tea.Cmd) {
	if msg.switchID != 0 && msg.switchID != m.Model.RuntimeSwitchRequest {
		return m, nil
	}
	m.Model.RuntimeSwitchRequest = 0
	return m.handleLocalError(msg.err)
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
