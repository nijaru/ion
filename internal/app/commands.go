package app

import (
	"context"
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
)

// handleCommand dispatches a slash command entered by the user.
func (m *Model) handleCommand(input string) tea.Cmd {
	fields := strings.Fields(input)
	if len(fields) == 0 {
		return nil
	}

	switch fields[0] {
	case "/resume":
		if len(fields) < 2 {
			return m.openSessionPicker()
		}
		return m.resumeStoredSessionByID(fields[1])
	case "/model":
		if len(fields) < 2 {
			return m.openModelPicker()
		}
		name := strings.Join(fields[1:], " ")
		cfg, err := config.Load()
		if err != nil {
			return cmdError(fmt.Sprintf("failed to load config: %v", err))
		}
		cfg.Model = name
		if err := config.Save(cfg); err != nil {
			return cmdError(fmt.Sprintf("failed to save config: %v", err))
		}
		m.backend.SetConfig(cfg)
		if cfg.Provider == "" {
			return tea.Printf("%s\n", m.renderEntry(session.Entry{Role: session.System, Content: "Set model to " + name}))
		}
		return m.switchRuntimeCommand(cfg, session.Entry{Role: session.System, Content: "Switched model to " + name}, m.session.ID())

	case "/provider":
		if len(fields) < 2 {
			return m.openProviderPicker()
		}
		name := fields[1]
		cfg, err := config.Load()
		if err != nil {
			return cmdError(fmt.Sprintf("failed to load config: %v", err))
		}
		cfg.Provider = name
		if err := config.Save(cfg); err != nil {
			return cmdError(fmt.Sprintf("failed to save config: %v", err))
		}
		m.backend.SetConfig(cfg)
		if cfg.Model == "" {
			return tea.Printf("%s\n", m.renderEntry(session.Entry{Role: session.System, Content: "Set provider to " + name}))
		}
		return m.switchRuntimeCommand(cfg, session.Entry{Role: session.System, Content: "Switched provider to " + name}, m.session.ID())

	case "/mcp":
		if len(fields) < 3 || fields[1] != "add" {
			return cmdError("usage: /mcp add <command> [args...]")
		}
		mcpCmd := fields[2]
		mcpArgs := fields[3:]
		sess := m.session
		return func() tea.Msg {
			if err := sess.RegisterMCPServer(context.Background(), mcpCmd, mcpArgs...); err != nil {
				return session.Error{Err: err}
			}
			return nil
		}

	case "/compact":
		compactor, ok := m.backend.(backend.Compactor)
		if !ok {
			return cmdError("current backend does not support /compact")
		}
		return func() tea.Msg {
			compacted, err := compactor.Compact(context.Background())
			if err != nil {
				return session.Error{Err: err}
			}
			if compacted {
				return sessionCompactedMsg{notice: "Compacted current session context"}
			}
			return sessionCompactedMsg{notice: "Session is already within compaction limits"}
		}

	case "/exit", "/quit":
		return tea.Quit

	default:
		return cmdError(fmt.Sprintf("unknown command: %s", fields[0]))
	}
}

func (m *Model) openProviderPicker() tea.Cmd {
	cfg, err := config.Load()
	if err != nil {
		return cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	return m.openProviderPickerWithConfig(cfg)
}

func (m *Model) openProviderPickerWithConfig(cfg *config.Config) tea.Cmd {
	items := providerItems()
	m.picker = &pickerState{
		title:   "Pick a provider",
		items:   items,
		index:   pickerIndex(items, cfg.Provider),
		purpose: pickerPurposeProvider,
		cfg:     cfg,
	}
	return nil
}

func (m *Model) openModelPicker() tea.Cmd {
	cfg, err := config.Load()
	if err != nil {
		return cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	return m.openModelPickerWithConfig(cfg)
}

func (m *Model) openModelPickerWithConfig(cfg *config.Config) tea.Cmd {
	if cfg.Provider == "" {
		return m.openProviderPickerWithConfig(cfg)
	}
	items, err := modelItemsForProvider(cfg.Provider)
	if err != nil {
		return cmdError(fmt.Sprintf("failed to list models for %s: %v", cfg.Provider, err))
	}
	if len(items) == 0 {
		return cmdError(fmt.Sprintf("no models available for provider %s", cfg.Provider))
	}
	m.picker = &pickerState{
		title:   "Pick a model for " + cfg.Provider,
		items:   items,
		index:   pickerIndex(items, cfg.Model),
		purpose: pickerPurposeModel,
		cfg:     cfg,
	}
	return nil
}

func (m *Model) handlePickerKey(msg tea.KeyPressMsg) (Model, tea.Cmd) {
	switch msg.String() {
	case "esc", "ctrl+c":
		m.picker = nil
		return *m, nil
	case "tab":
		if m.picker.purpose == pickerPurposeProvider {
			if m.picker.cfg != nil && m.picker.cfg.Provider != "" {
				return *m, m.openModelPickerWithConfig(m.picker.cfg)
			}
			return *m, nil
		}
		if m.picker.purpose == pickerPurposeModel {
			return *m, m.openProviderPickerWithConfig(m.picker.cfg)
		}
		return *m, nil
	case "up":
		if m.picker.index > 0 {
			m.picker.index--
		}
		return *m, nil
	case "down":
		if m.picker.index < len(m.picker.items)-1 {
			m.picker.index++
		}
		return *m, nil
	case "enter":
		return m.commitPickerSelection()
	default:
		return *m, nil
	}
}

func (m *Model) commitPickerSelection() (Model, tea.Cmd) {
	if m.picker == nil || len(m.picker.items) == 0 {
		m.picker = nil
		return *m, nil
	}

	selected := m.picker.items[m.picker.index]
	cfg := *m.picker.cfg

	switch m.picker.purpose {
	case pickerPurposeProvider:
		cfg.Provider = selected.Value
		m.picker = nil
		cfg.Model = ""
		if err := config.Save(&cfg); err != nil {
			return *m, cmdError(fmt.Sprintf("failed to save config: %v", err))
		}
		m.openModelPickerWithConfig(&cfg)
		return *m, tea.Printf("%s\n", m.renderEntry(session.Entry{Role: session.System, Content: "Set provider to " + selected.Value}))

	case pickerPurposeModel:
		cfg.Model = selected.Value
		if err := config.Save(&cfg); err != nil {
			return *m, cmdError(fmt.Sprintf("failed to save config: %v", err))
		}
		m.picker = nil
		notice := session.Entry{Role: session.System, Content: "Switched model to " + selected.Value}
		return *m, m.switchRuntimeCommand(&cfg, notice, m.session.ID())
	default:
		m.picker = nil
		return *m, nil
	}
}

func (m *Model) resumeStoredSessionByID(sessionID string) tea.Cmd {
	if m.store == nil {
		return cmdError("session store not available")
	}

	resumed, err := m.store.ResumeSession(context.Background(), sessionID)
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
	return m.switchRuntimeCommand(cfg, notice, sessionID)
}

func (m *Model) switchRuntimeCommand(cfg *config.Config, notice session.Entry, sessionID string) tea.Cmd {
	if m.switcher == nil {
		m.backend.SetConfig(cfg)
		return tea.Printf("%s\n", m.renderEntry(notice))
	}

	oldSession := m.session
	switchID := sessionID
	if switchID == "" && oldSession != nil {
		switchID = oldSession.ID()
	}
	switcher := m.switcher
	cfgCopy := *cfg

	return func() tea.Msg {
		if oldSession != nil {
			_ = oldSession.CancelTurn(context.Background())
		}
		backend, sess, storageSess, err := switcher(context.Background(), &cfgCopy, switchID)
		if err != nil {
			return session.Error{Err: err}
		}
		if oldSession != nil {
			_ = oldSession.Close()
		}
		return runtimeSwitchedMsg{
			backend: backend,
			session: sess,
			storage: storageSess,
			status:  backend.Bootstrap().Status,
			notice:  notice.Content,
		}
	}
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

// cmdError returns a Cmd that emits a session.Error with the given message.
func cmdError(msg string) tea.Cmd {
	return func() tea.Msg {
		return session.Error{Err: fmt.Errorf("%s", msg)}
	}
}

// renderDiff colorizes diff-format output.
// Falls back to plain output if the content doesn't look like a unified diff.
func (m Model) renderDiff(content string) string {
	lines := strings.Split(content, "\n")
	hasDiffMarkers := false
	for _, l := range lines {
		if strings.HasPrefix(l, "--- ") || strings.HasPrefix(l, "+++ ") ||
			strings.HasPrefix(l, "@@ ") {
			hasDiffMarkers = true
			break
		}
	}
	if !hasDiffMarkers {
		return content
	}

	var b strings.Builder
	for _, l := range lines {
		switch {
		case strings.HasPrefix(l, "+") && !strings.HasPrefix(l, "+++"):
			b.WriteString(m.st.added.Render(l))
		case strings.HasPrefix(l, "-") && !strings.HasPrefix(l, "---"):
			b.WriteString(m.st.removed.Render(l))
		case strings.HasPrefix(l, "@@ "):
			b.WriteString(m.st.cyan.Render(l))
		default:
			b.WriteString(m.st.dim.Render(l))
		}
		b.WriteString("\n")
	}
	return b.String()
}

// isWriteTool returns true if the tool title looks like a write/edit operation.
func isWriteTool(title string) bool {
	for _, prefix := range []string{"write(", "edit(", "create(", "Write(", "Edit("} {
		if strings.HasPrefix(title, prefix) {
			return true
		}
	}
	return false
}
