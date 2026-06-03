package app

import (
	"github.com/nijaru/ion/config"
	"context"
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/session"
	"github.com/nijaru/ion/internal/core"
)

func (m Model) resumeStoredSessionByID(sessionID string) (Model, tea.Cmd) {
	if m.Model.Store == nil {
		return m, cmdError("session store not available")
	}

	switchID := m.runtimeRequest().begin("Loading session...")
	store := m.Model.Store
	return m, func() tea.Msg {
		cfg, err := m.storedSessionConfig(context.Background(), store, sessionID)
		if err != nil {
			return runtimeSwitchErrorMsg{switchID: switchID, err: err}
		}
		return resumeSessionSelectedMsg{
			switchID:  switchID,
			sessionID: sessionID,
			cfg:       cfg,
		}
	}
}

func (m Model) storedSessionConfig(
	ctx context.Context,
	store session.SessionStore,
	sessionID string,
) (*config.Config, error) {
	resumed, err := store.ResumeSession(ctx, sessionID)
	if err != nil {
		return nil, fmt.Errorf("failed to resume session %s: %w", sessionID, err)
	}
	if resumed == nil {
		return nil, fmt.Errorf("failed to resume session %s: missing session", sessionID)
	}
	defer func() {
		_ = resumed.Close()
	}()

	meta := resumed.Meta()
	provider, modelName := splitStoredSessionModel(meta.Model)
	if provider == "" || modelName == "" {
		return nil, fmt.Errorf("session %s is missing provider/model metadata", sessionID)
	}
	cfg, err := m.configForStoredSession(provider, modelName)
	if err != nil {
		return nil, fmt.Errorf("failed to apply session metadata: %w", err)
	}
	return cfg, nil
}

func (m Model) handleResumeSessionSelected(msg resumeSessionSelectedMsg) (Model, tea.Cmd) {
	if !m.runtimeRequest().matches(msg.switchID) {
		return m, nil
	}
	notice := systemEntry("Resumed session " + msg.sessionID)
	return m.resumeRuntimeCommand(msg.cfg, notice, msg.sessionID)
}

func (m Model) configForStoredSession(provider, model string) (*config.Config, error) {
	cfg, err := m.commandConfig()
	if err != nil {
		return nil, fmt.Errorf("load config: %w", err)
	}
	cfg, err = updateProviderSelection(cfg, provider)
	if err != nil {
		return nil, err
	}
	return updateModelForPreset(cfg, model, presetPrimary), nil
}

func (m Model) switchPresetCommand(preset core.Preset) (Model, tea.Cmd) {
	if m.localCommandBusy() {
		return m, cmdError(m.localCommandBusyMessage("changing presets"))
	}
	cfg, err := m.commandConfig()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	runtimeCfg, err := m.runtimeConfigForPreset(cfg, preset)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to resolve %s preset: %v", preset, err))
	}
	notice := systemEntry("Switched to " + preset.String())
	transition := newRuntimeTransition(cfg, runtimeCfg, preset, "")
	return m.switchRuntimeCommand(
		transition,
		notice,
		m.currentMaterializedSessionID(),
		false,
	)
}

func (m Model) currentMaterializedSessionID() string {
	if id := m.Model.Runtime.MaterializedSessionID(); id != "" {
		return id
	}
	if m.Model.Session == nil {
		return ""
	}
	if m.Model.Storage == nil {
		return m.Model.Session.ID()
	}
	if !session.IsMaterialized(m.Model.Storage) {
		return ""
	}
	return strings.TrimSpace(m.Model.Storage.ID())
}

func (m Model) ResumeSessionID() string {
	return m.currentMaterializedSessionID()
}

func (m Model) switchRuntimeCommand(
	transition core.Transition,
	notice session.Entry,
	sessionID string,
	preserveSession bool,
) (Model, tea.Cmd) {
	transition = transition.WithActivePresetPersistence()

	if m.Model.Switcher == nil {
		return m.beginRuntimeTransitionCommit(transition, notice)
	}

	switcher := m.Model.Switcher
	current := m.Handles()
	requestID := m.runtimeRequest().begin("Switching runtime...")

	return m, func() tea.Msg {
		result, err := core.Switch(context.Background(), core.SwitchInput{
			Switcher:        switcher,
			Transition:      transition,
			Current:         current,
			TargetSessionID: sessionID,
			PreserveSession: preserveSession,
			SaveState:       saveRuntimeState,
		})
		if err != nil {
			return runtimeSwitchErrorMsg{switchID: requestID, err: err}
		}
		return runtimeSwitchedMsg{
			switchID:   requestID,
			runtime:    result.Runtime,
			previous:   result.Previous,
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
		return m.beginRuntimeTransitionCommit(transition, notice)
	}
	switcher := m.Model.Switcher
	current := m.Handles()
	switchID := m.runtimeRequest().begin("Switching runtime...")
	return m, func() tea.Msg {
		result, err := core.Resume(context.Background(), core.ResumeInput{
			Switcher:   switcher,
			Transition: transition,
			Current:    current,
			SessionID:  sessionID,
			SaveState:  saveRuntimeState,
		})
		if err != nil {
			return runtimeSwitchErrorMsg{switchID: switchID, err: err}
		}
		resumeBranch := currentBranchName(m.App.Branch, result.Runtime.Handles.Storage)
		printLines := []string{m.runtimeHeaderLine(result.Runtime.Handles.Backend)}
		if header := m.headerLineFor(resumeBranch); header != "" {
			printLines = append(printLines, header)
		}
		printLines = append(printLines, "", "--- resumed ---", "")
		return runtimeSwitchedMsg{
			switchID:      switchID,
			runtime:       result.Runtime,
			previous:      result.Previous,
			printLines:    printLines,
			replayEntries: result.Entries,
			notice:        notice.Content,
			showStatus:    false,
		}
	}
}

func (m Model) handleRuntimeSwitched(msg runtimeSwitchedMsg) (Model, tea.Cmd) {
	if !m.runtimeRequest().matches(msg.switchID) {
		closeRuntimeHandles(msg.runtime.Handles)
		return m, nil
	}

	m.applyRuntimeSwitched(msg)
	cmds := m.runtimeSwitchedCommands(msg)
	return m, tea.Sequence(cmds...)
}

func (m *Model) applyRuntimeSwitched(msg runtimeSwitchedMsg) {
	m.runtimeRequest().clear()
	m.Model.Backend = msg.runtime.Handles.Backend
	m.Model.Session = msg.runtime.Handles.Session
	m.Model.Storage = msg.runtime.Handles.Storage
	m.applyRuntimeSnapshot(msg.runtime.Transition.Snapshot)
	closeRuntimeHandles(msg.previous)
	m.Model.EventGeneration++
	m.pickerReducer().closeAll()
	m.clearProgressError()
	if msg.runtime.Handles.Storage != nil {
		meta := msg.runtime.Handles.Storage.Meta()
		m.App.Branch = meta.Branch
	}
	m.turnReducer().ClearActiveState(true)
	m.progressReducer().clearLocalBusyStatus()
	m.progressReducer().markRuntimeReady()
	m.turnReducer().ResetFinishedTurnSummary()
	m.clearPendingAction()
	m.progressReducer().resetSessionUsage()
	m.resetHistoryCursor()
}

func (m *Model) runtimeSwitchedCommands(msg runtimeSwitchedMsg) []tea.Cmd {
	cmds := make([]tea.Cmd, 0, 3)

	var status string
	s := msg.runtime.Transition.Snapshot.Status
	if msg.showStatus && strings.TrimSpace(s) != "" && !isConfigurationStatus(s) {
		status = s
	}

	if cmd := m.terminalCommit().SwitchReplay(msg.printLines, msg.replayEntries, msg.notice, status); cmd != nil {
		cmds = append(cmds, cmd)
	}

	if msg.runtime.Handles.Storage != nil {
		cmds = append(
			cmds,
			loadSessionUsageCmd(m.Model.EventGeneration, msg.runtime.Handles.Storage),
		)
	}
	return append(cmds, m.awaitSessionEvent())
}

func (m Model) handleRuntimeSwitchError(msg runtimeSwitchErrorMsg) (Model, tea.Cmd) {
	if !m.runtimeRequest().matches(msg.switchID) {
		return m, nil
	}
	m.runtimeRequest().clear()
	return m.handleLocalError(msg.err)
}

func closeRuntimeHandles(handles core.Handles) {
	core.CloseHandles(handles)
}

func currentBranchName(defaultBranch string, sess session.SessionHandle) string {
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
