package app

import (
	"context"
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"

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
		if cfg.Provider == "" {
			return m.openProviderPickerWithIntent(pickerPurposeProvider, cfg)
		}
		notice := session.Entry{Role: session.System, Content: "Switched model to " + name}
		return m.switchRuntimeCommand(cfg, notice)

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
		if cfg.Model == "" && !isACPProvider(name) {
			return m.openModelPickerWithConfig(cfg, pickerPurposeProvider)
		}
		notice := session.Entry{Role: session.System, Content: "Switched provider to " + name}
		return m.switchRuntimeCommand(cfg, notice)

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
	return m.openProviderPickerWithIntent(pickerPurposeProvider, cfg)
}

func (m *Model) openProviderPickerWithIntent(intent pickerPurpose, cfg *config.Config) tea.Cmd {
	items := providerItems()
	m.picker = &pickerState{
		title:   "Pick a provider",
		items:   items,
		index:   pickerIndex(items, cfg.Provider),
		purpose: pickerPurposeProvider,
		intent:  intent,
		cfg:     cfg,
	}
	return nil
}

func (m *Model) openModelPicker() tea.Cmd {
	cfg, err := config.Load()
	if err != nil {
		return cmdError(fmt.Sprintf("failed to load config: %v", err))
	}
	return m.openModelPickerWithConfig(cfg, pickerPurposeModel)
}

func (m *Model) openModelPickerWithConfig(cfg *config.Config, intent pickerPurpose) tea.Cmd {
	if cfg.Provider == "" {
		return m.openProviderPickerWithIntent(intent, cfg)
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
		intent:  intent,
		cfg:     cfg,
	}
	return nil
}

func (m *Model) handlePickerKey(msg tea.KeyPressMsg) (Model, tea.Cmd) {
	switch msg.String() {
	case "esc", "ctrl+c":
		m.picker = nil
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
	intent := m.picker.intent

	switch m.picker.purpose {
	case pickerPurposeProvider:
		cfg.Provider = selected.Value
		if err := config.Save(&cfg); err != nil {
			return *m, cmdError(fmt.Sprintf("failed to save config: %v", err))
		}
		m.picker = nil
		if intent == pickerPurposeModel {
			return *m, m.openModelPickerWithConfig(&cfg, pickerPurposeModel)
		}
		if cfg.Model == "" && !isACPProvider(selected.Value) {
			return *m, m.openModelPickerWithConfig(&cfg, pickerPurposeProvider)
		}
		notice := session.Entry{Role: session.System, Content: "Switched provider to " + selected.Value}
		return *m, m.switchRuntimeCommand(&cfg, notice)

	case pickerPurposeModel:
		cfg.Model = selected.Value
		if err := config.Save(&cfg); err != nil {
			return *m, cmdError(fmt.Sprintf("failed to save config: %v", err))
		}
		m.picker = nil
		notice := session.Entry{Role: session.System, Content: "Switched model to " + selected.Value}
		return *m, m.switchRuntimeCommand(&cfg, notice)
	default:
		m.picker = nil
		return *m, nil
	}
}

func (m *Model) switchRuntimeCommand(cfg *config.Config, notice session.Entry) tea.Cmd {
	if m.switcher == nil {
		m.backend.SetConfig(cfg)
		return tea.Printf("%s\n", m.renderEntry(notice))
	}

	oldSession := m.session
	switcher := m.switcher
	cfgCopy := *cfg

	return func() tea.Msg {
		if oldSession != nil {
			_ = oldSession.CancelTurn(context.Background())
		}
		backend, sess, storageSess, err := switcher(context.Background(), &cfgCopy)
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
