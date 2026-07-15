package app

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"strings"

	"github.com/nijaru/ion/config"

	tea "charm.land/bubbletea/v2"
	ionclipboard "github.com/nijaru/ion/internal/clipboard"
	ionexport "github.com/nijaru/ion/internal/export"
	"github.com/nijaru/ion/session"
)

func (m Model) exportSession() (Model, tea.Cmd) {
	if m.Model.Store == nil {
		return m, cmdError("no store available")
	}
	if m.activeSession() == nil {
		return m, cmdError("no active session")
	}
	sessionID := m.activeSession().ID()
	return m, func() tea.Msg {
		bundle, err := ionexport.ExportSessionBundle(context.Background(), m.Model.Store, sessionID)
		if err != nil {
			return localErrorMsg{err: err}
		}
		data, err := json.Marshal(bundle)
		if err != nil {
			return localErrorMsg{err: err}
		}
		filename := fmt.Sprintf("session-%s.json", sessionID)
		if err := os.WriteFile(filename, data, 0644); err != nil {
			return localErrorMsg{err: err}
		}
		return sessionExportedMsg{filename: filename}
	}
}

func (m Model) exportSessionHTML() (Model, tea.Cmd) {
	if m.Model.Store == nil {
		return m, cmdError("no store available")
	}
	if m.activeSession() == nil {
		return m, cmdError("no active session")
	}
	sessionID := m.activeSession().ID()
	return m, func() tea.Msg {
		bundle, err := ionexport.ExportSessionBundle(context.Background(), m.Model.Store, sessionID)
		if err != nil {
			return localErrorMsg{err: err}
		}
		htmlContent, err := ionexport.BundleToHTML(bundle)
		if err != nil {
			return localErrorMsg{err: err}
		}
		filename := fmt.Sprintf("session-%s.html", sessionID[:min(8, len(sessionID))])
		if err := os.WriteFile(filename, []byte(htmlContent), 0644); err != nil {
			return localErrorMsg{err: err}
		}
		return sessionExportedMsg{filename: filename}
	}
}

type sessionExportedMsg struct {
	filename string
}

func (m Model) handleSessionExported(msg sessionExportedMsg) (Model, tea.Cmd) {
	m.terminalCommit().Entries(systemEntry("Exported session to " + msg.filename))
	return m, nil
}

func (m Model) importSession(filename string) (Model, tea.Cmd) {
	if m.Model.Runner == nil {
		return m, cmdError("active runtime does not support import")
	}
	runner := m.Model.Runner
	return m, func() tea.Msg {
		data, err := os.ReadFile(filename)
		if err != nil {
			return localErrorMsg{err: err}
		}
		bundle, err := ionexport.DecodeSessionBundle(data)
		if err != nil {
			return localErrorMsg{err: err}
		}
		bundle.RootSessionID = ""
		imported, err := runner.ImportSessionBundle(context.Background(), bundle)
		if err != nil {
			return localErrorMsg{err: err}
		}
		return sessionImportedMsg{sessionID: imported, filename: filename}
	}
}

type sessionImportedMsg struct {
	sessionID string
	filename  string
}

func (m Model) handleSessionImported(msg sessionImportedMsg) (Model, tea.Cmd) {
	notice := fmt.Sprintf("Imported %d session(s) from %s", 1, msg.filename)
	commit := m.terminalCommit().Entries(systemEntry(notice))
	resumeModel, resumeCmd := m.resumeStoredSessionByID(msg.sessionID)
	return resumeModel, tea.Sequence(commit, resumeCmd)
}

func (m Model) nameSession(name string) (Model, tea.Cmd) {
	if m.Model.Runner == nil {
		return m, cmdError("no active session")
	}
	runner := m.Model.Runner
	return m, func() tea.Msg {
		_, err := runner.AppendSessionInfo(context.Background(), name)
		if err != nil {
			return localErrorMsg{err: err}
		}
		return sessionNamedMsg{name: name}
	}
}

type sessionNamedMsg struct {
	name string
}

func (m Model) handleSessionNamed(msg sessionNamedMsg) (Model, tea.Cmd) {
	m.terminalCommit().Entries(systemEntry("Session named: " + msg.name))
	return m, nil
}

func (m Model) copyLastResponse() (Model, tea.Cmd) {
	if m.Model.Storage == nil {
		return m, cmdError("no active session")
	}
	entries, err := m.Model.Storage.Entries(context.Background())
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to get entries: %v", err))
	}
	// Find the last assistant message
	var lastResponse string
	for i := len(entries) - 1; i >= 0; i-- {
		if session.EntryRole(entries[i]) == "assistant" {
			lastResponse = session.EntryText(entries[i])
			break
		}
	}
	if lastResponse == "" {
		return m, cmdError("no assistant response to copy")
	}
	if err := ionclipboard.WriteClipboardText(lastResponse); err != nil {
		return m, cmdError(fmt.Sprintf("failed to copy: %v", err))
	}
	m.terminalCommit().Entries(systemEntry("Copied last response to clipboard"))
	return m, nil
}

func (m Model) cloneSession() (Model, tea.Cmd) {
	if m.Model.Runner == nil {
		return m, cmdError("active runtime does not support clone")
	}
	if m.activeSession() == nil {
		return m, cmdError("no active session")
	}
	sessionID := m.activeSession().ID()
	runner := m.Model.Runner
	return m, func() tea.Msg {
		ctx := context.Background()
		bundle, err := ionexport.ExportSessionBundle(ctx, m.Model.Store, sessionID)
		if err != nil {
			return localErrorMsg{err: fmt.Errorf("export session: %w", err)}
		}
		// Clear the root session ID so import creates a new session
		bundle.RootSessionID = ""
		imported, err := runner.ImportSessionBundle(ctx, bundle)
		if err != nil {
			return localErrorMsg{err: fmt.Errorf("import session: %w", err)}
		}
		if len(imported) == 0 {
			return localErrorMsg{err: fmt.Errorf("no sessions imported")}
		}
		return sessionClonedMsg{newSessionID: imported}
	}
}

type sessionClonedMsg struct {
	newSessionID string
}

func (m Model) handleSessionCloned(msg sessionClonedMsg) (Model, tea.Cmd) {
	cmd := m.terminalCommit().Entries(systemEntry("Cloned session " + msg.newSessionID))
	resumeModel, resumeCmd := m.resumeStoredSessionByID(msg.newSessionID)
	return resumeModel, tea.Sequence(cmd, resumeCmd)
}

func (m Model) costBudgetNotice(inputTokens, outputTokens int, totalCost float64) string {
	totalTokens := inputTokens + outputTokens
	lines := []string{
		"Session cost",
		fmt.Sprintf("input tokens: %d", inputTokens),
		fmt.Sprintf("output tokens: %d", outputTokens),
		fmt.Sprintf("total tokens: %d", totalTokens),
		fmt.Sprintf("cost: $%.6f", totalCost),
	}
	if m.Model.Config != nil && m.Model.Config.MaxSessionCost > 0 {
		lines = append(lines, fmt.Sprintf("session limit: $%.6f", m.Model.Config.MaxSessionCost))
		remaining := m.Model.Config.MaxSessionCost - totalCost
		if remaining < 0 {
			remaining = 0
		}
		lines = append(lines, fmt.Sprintf("session remaining: $%.6f", remaining))
	}
	if m.Model.Config != nil && m.Model.Config.MaxTurnCost > 0 {
		lines = append(lines, fmt.Sprintf("turn limit: $%.6f", m.Model.Config.MaxTurnCost))
	}
	return strings.Join(lines, "\n")
}

func (m Model) handleSessionCompacted(msg sessionCompactedMsg) (Model, tea.Cmd) {
	m.progressReducer().completeCompaction()
	cmds := []tea.Cmd{m.terminalCommit().Entries(systemEntry(msg.notice))}
	if queued := m.turnReducer().PopQueuedTurn(); queued != "" {
		cmds = append(cmds, func() tea.Msg {
			return queuedTurnMsg{text: queued, rearmSessionEvents: false}
		})
	}
	return m, tea.Sequence(cmds...)
}

func (m Model) handleSessionCost(msg sessionCostMsg) (Model, tea.Cmd) {
	return m, m.terminalCommit().Entries(systemEntry(msg.notice))
}

func loadSessionUsageCmd(generation uint64, sess persistenceAdapter) tea.Cmd {
	if sess == nil {
		return nil
	}
	return func() tea.Msg {
		usage, err := sess.Usage(context.Background())
		return sessionUsageLoadedMsg{
			generation: generation,
			input:      usage.Input,
			output:     usage.Output,
			cost:       usage.Cost.Total,
			err:        err,
		}
	}
}

func (m Model) handleSessionUsageLoaded(msg sessionUsageLoadedMsg) (Model, tea.Cmd) {
	if msg.generation != m.Model.EventGeneration || msg.err != nil {
		return m, nil
	}
	m.progressReducer().applySessionUsage(msg.input, msg.output, msg.cost)
	return m, nil
}

func (m Model) sessionCostCmd() tea.Cmd {
	return func() tea.Msg {
		inputTokens := m.Progress.TokensSent
		outputTokens := m.Progress.TokensReceived
		totalCost := m.Progress.TotalCost
		if m.Model.Storage != nil {
			usage, err := m.Model.Storage.Usage(context.Background())
			if err != nil {
				return localErrorMsg{err: fmt.Errorf("failed to load session usage: %w", err)}
			}
			inputTokens = usage.Input
			outputTokens = usage.Output
			totalCost = usage.Cost.Total
		}
		if totalCost <= 0 {
			if m.Model.Config != nil &&
				(m.Model.Config.MaxSessionCost > 0 || m.Model.Config.MaxTurnCost > 0) {
				return sessionCostMsg{
					notice: m.costBudgetNotice(inputTokens, outputTokens, totalCost),
				}
			}
			return sessionCostMsg{notice: "No API cost tracked for this session"}
		}
		return sessionCostMsg{notice: m.costBudgetNotice(inputTokens, outputTokens, totalCost)}
	}
}

func (m Model) sessionInfoCmd() tea.Cmd {
	return func() tea.Msg {
		notice, err := m.sessionInfoNotice()
		if err != nil {
			return localErrorMsg{err: err}
		}
		return localEntriesMsg{
			entries: []session.Entry{systemEntry(notice)},
		}
	}
}

func (m Model) sessionInfoNotice() (string, error) {
	sessionID := ""
	if m.Model.Runtime.Materialized {
		sessionID = m.Model.Runtime.SessionID
	}
	if m.Model.Storage != nil {
		if sessionID == "" {
			sessionID = strings.TrimSpace(m.Model.Storage.ID())
		}
	} else if m.activeSession() != nil {
		sessionID = strings.TrimSpace(m.activeSession().ID())
	}
	if sessionID == "" {
		sessionID = "none"
	}

	provider := m.runtimeProvider()
	model := m.runtimeModel()
	if provider == "" {
		provider = "unknown"
	}
	if model == "" {
		model = "unknown"
	}

	inputTokens, outputTokens, totalCost := m.Progress.TokensSent, m.Progress.TokensReceived, m.Progress.TotalCost
	var entries []session.Entry
	if m.Model.Storage != nil {
		usage, err := m.Model.Storage.Usage(context.Background())
		if err != nil {
			return "", fmt.Errorf("failed to load session usage: %w", err)
		}
		inputTokens = usage.Input
		outputTokens = usage.Output
		totalCost = usage.Cost.Total
		loaded, err := m.Model.Storage.Entries(context.Background())
		if err != nil {
			return "", fmt.Errorf("failed to load session entries: %w", err)
		}
		entries = loaded
	}

	counts := sessionEntryCounts(entries)
	lines := []string{
		"Session",
		"id: " + sessionID,
		"provider: " + provider,
		"model: " + model,
	}
	if branch := strings.TrimSpace(m.App.Branch); branch != "" {
		lines = append(lines, "branch: "+branch)
	}
	lines = append(
		lines,
		fmt.Sprintf("messages: user %d, assistant %d, tools %d, total %d",
			counts.user, counts.agent, counts.tool, counts.total),
		fmt.Sprintf("tokens: input %d, output %d, total %d",
			inputTokens, outputTokens, inputTokens+outputTokens),
		fmt.Sprintf("cost: $%.6f", totalCost),
	)
	return strings.Join(lines, "\n"), nil
}

type sessionCounts struct {
	user  int
	agent int
	tool  int
	total int
}

func sessionEntryCounts(entries []session.Entry) sessionCounts {
	var counts sessionCounts
	for _, entry := range entries {
		counts.total++
		switch session.EntryRole(entry) {
		case "user":
			counts.user++
		case "assistant":
			counts.agent++
		case "tool_result":
			counts.tool++
		}
	}
	return counts
}

type externalEditorFinishedMsg struct {
	content string
	err     error
}

var (
	externalEditorName            = externalEditor
	writeExternalEditorBufferFile = writeExternalEditorBuffer
)

func (m Model) handleNewSessionCommand(fields []string, command string) (Model, tea.Cmd) {
	cfg, err := m.commandConfig()
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	runtimeCfg, err := m.runtimeConfigForActivePreset(cfg)
	if err != nil {
		return m, cmdError(fmt.Sprintf("failed to resolve active preset: %v", err))
	}
	if runtimeCfg.Provider == "" {
		runtimeCfg.Provider = m.runtimeProvider()
	}
	if runtimeCfg.Model == "" {
		runtimeCfg.Model = m.runtimeModel()
	}
	if runtimeCfg.Provider == "" || runtimeCfg.Model == "" {
		return m, cmdError("cannot " + command + " without an active provider and model")
	}
	appCfg := cfg
	if appCfg == nil {
		appCfg = &config.Config{}
	}
	if strings.TrimSpace(appCfg.Provider) == "" {
		updated := *appCfg
		updated.Provider = runtimeCfg.Provider
		appCfg = &updated
	}
	if configuredModelForPreset(appCfg, m.activePreset()) == "" {
		appCfg = updateModelForPreset(appCfg, runtimeCfg.Model, m.activePreset())
	}
	notice := "Started new session"
	if command == "/clear" {
		notice = "Started fresh session"
	}
	return m.switchRuntimeCommand(
		newRuntimeTransition(appCfg, runtimeCfg, m.activePreset(), ""),
		systemEntry(notice),
		"",
		false,
	)
}

func (m Model) handleCompactCommand() (Model, tea.Cmd) {
	runner := m.Model.Runner
	if runner == nil {
		return m, cmdError("no active runner")
	}
	m.progressReducer().beginCompaction()
	return m, func() tea.Msg {
		if err := runner.Compact(context.Background()); err != nil {
			return localErrorMsg{err: err}
		}
		return sessionCompactedMsg{notice: "Compacted current session context"}
	}
}

func (m Model) handleImportCommand(fields []string) (Model, tea.Cmd) {
	if len(fields) < 2 {
		return m, cmdError("usage: /import <filename>")
	}
	return m.importSession(fields[1])
}

func (m Model) handleNameCommand(fields []string) (Model, tea.Cmd) {
	if len(fields) < 2 {
		return m, cmdError("usage: /name <session-name>")
	}
	return m.nameSession(strings.Join(fields[1:], " "))
}

func (m Model) handleLabelCommand(fields []string) (Model, tea.Cmd) {
	if m.activeSession() == nil {
		return m, cmdError("no active session")
	}
	if m.Model.Store == nil {
		return m, cmdError("no store available")
	}
	runner := m.Model.Runner
	store := m.Model.Store
	leafID := store.GetLeafID()

	if len(fields) < 2 {
		// Show current label.
		return m, func() tea.Msg {
			label, err := runner.GetLabel(context.Background(), leafID)
			return labelShowMsg{label: label, err: err}
		}
	}
	// Set label.
	text := strings.Join(fields[1:], " ")
	return m, func() tea.Msg {
		_, err := runner.AppendLabel(context.Background(), leafID, text)
		return labelShowMsg{label: text, err: err}
	}
}

type labelShowMsg struct {
	label string
	err   error
}

func (m Model) handleLabelShow(msg labelShowMsg) (Model, tea.Cmd) {
	if msg.err != nil {
		m.terminalCommit().Entries(systemEntry(fmt.Sprintf("⚠ label: %v", msg.err)))
		return m, nil
	}
	if msg.label == "" {
		m.terminalCommit().Entries(systemEntry("ℹ no label set on current branch"))
	} else {
		m.terminalCommit().Entries(systemEntry(fmt.Sprintf("🏷 label: %s", msg.label)))
	}
	return m, nil
}
