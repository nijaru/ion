package app

import (
	"context"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/session"
)

// handleKey is the source of truth for core TUI hotkey semantics.
func (m Model) handleKey(msg tea.KeyPressMsg) (Model, tea.Cmd) {
	if m.Picker.Session != nil {
		return m.handleSessionPickerKey(msg)
	}
	if m.Picker.Overlay != nil {
		return m.handlePickerKey(msg)
	}

	// Approval gate: y/n/a consumed before any other handling
	if m.Approval.Pending != nil {
		switch msg.String() {
		case "y", "n":
			approved := msg.String() == "y"
			reqID := m.Approval.Pending.RequestID
			desc := m.Approval.Pending.Description
			m.Approval.Pending = nil
			m.Progress.Mode = stateReady

			label := ifthen(approved, "Approved", "Denied")
			notice := session.Entry{Role: session.System, Content: label + ": " + desc}
			if err := m.Model.Session.Approve(context.Background(), reqID, approved); err != nil {
				return m, persistErrorCmd("send approval", err)
			}
			return m, m.printEntries(notice)
		case "a":
			reqID := m.Approval.Pending.RequestID
			toolName := m.Approval.Pending.ToolName
			desc := m.Approval.Pending.Description
			m.Approval.Pending = nil
			m.Progress.Mode = stateReady

			m.Model.Session.AllowCategory(toolName)
			notice := session.Entry{Role: session.System, Content: "Always: " + desc}
			if err := m.Model.Session.Approve(context.Background(), reqID, true); err != nil {
				return m, persistErrorCmd("send approval", err)
			}
			return m, m.printEntries(notice)
		}
	}

	switch msg.String() {
	case "ctrl+m":
		m.clearPendingAction()
		if m.activePreset() == presetFast {
			return m.switchPresetCommand(presetPrimary)
		}
		return m.switchPresetCommand(presetFast)

	case "ctrl+t":
		m.clearPendingAction()
		return m.openThinkingPicker()

	case "ctrl+c":
		if m.Input.Composer.Value() != "" {
			m.clearPendingAction()
			m.Input.Composer.Reset()
			m.PasteMarkers = make(map[string]pasteMarker)
			m.relayoutComposer()
			return m, nil
		}
		if m.InFlight.Thinking {
			m.clearPendingAction()
			return m, nil
		}
		if m.Input.CtrlCPending {
			return m, tea.Quit
		}
		return m, m.armPendingAction(pendingActionQuitCtrlC)

	case "ctrl+d":
		if m.Input.Composer.Value() != "" || m.InFlight.Thinking {
			m.clearPendingAction()
			return m, nil
		}
		if m.Input.CtrlCPending {
			return m, tea.Quit
		}
		return m, m.armPendingAction(pendingActionQuitCtrlD)

	case "?":
		if strings.TrimSpace(m.Input.Composer.Value()) == "" {
			m.clearPendingAction()
			return m, m.printHelp(helpText())
		}

	case "esc":
		if m.InFlight.Thinking {
			m.clearPendingAction()
			return m.cancelRunningTurn("Canceled by user")
		}
		m.clearPendingAction()
		return m, nil

	case "shift+tab":
		m.clearPendingAction()
		switch m.Mode {
		case session.ModeRead:
			next, cmd := m.setModeCommand(session.ModeEdit)
			return next, cmd
		case session.ModeEdit:
			next, cmd := m.setModeCommand(session.ModeRead)
			return next, cmd
		default:
			next, cmd := m.setModeCommand(session.ModeEdit)
			return next, cmd
		}

	case "tab":
		if next, cmd, ok := m.completeSlashCommand(); ok {
			return next, cmd
		}
		if next, cmd, ok := m.completeFileReference(); ok {
			return next, cmd
		}

	case "enter":
		if m.Input.DelayNextEnter {
			m.clearPendingAction()
			m.Input.DelayNextEnter = false
			m.Input.DeferredEnter = true
			m.Input.PrintHoldUntil = time.Now().Add(m.Input.PrintHoldDelay)
			return m, m.scheduleDeferredEnter()
		}
		if m.printHoldActive() {
			m.clearPendingAction()
			m.Input.DeferredEnter = true
			return m, m.scheduleDeferredEnter()
		}
		return m.submitComposer()

	case "shift+enter", "alt+enter":
		m.clearPendingAction()
		var cmd tea.Cmd
		m.Input.Composer, cmd = m.Input.Composer.Update(msg)
		m.layout()
		return m, cmd

	case "up", "ctrl+p":
		m.clearPendingAction()
		if m.Input.Composer.Line() == 0 && len(m.Input.History) > 0 {
			if m.Input.HistoryIdx == -1 {
				m.Input.HistoryDraft = m.Input.Composer.Value()
				m.Input.HistoryIdx = len(m.Input.History) - 1
				m.Input.Composer.SetValue(m.Input.History[m.Input.HistoryIdx])
				m.relayoutComposer()
				return m, nil
			} else if m.Input.HistoryIdx > 0 {
				m.Input.HistoryIdx--
				m.Input.Composer.SetValue(m.Input.History[m.Input.HistoryIdx])
				m.relayoutComposer()
				return m, nil
			}
		}
		var cmd tea.Cmd
		m.Input.Composer, cmd = m.Input.Composer.Update(msg)
		return m, cmd

	case "down", "ctrl+n":
		m.clearPendingAction()
		if m.Input.Composer.Line() == m.Input.Composer.LineCount()-1 && m.Input.HistoryIdx != -1 {
			if m.Input.HistoryIdx < len(m.Input.History)-1 {
				m.Input.HistoryIdx++
				m.Input.Composer.SetValue(m.Input.History[m.Input.HistoryIdx])
				m.relayoutComposer()
				return m, nil
			} else {
				m.Input.HistoryIdx = -1
				m.Input.Composer.SetValue(m.Input.HistoryDraft)
				m.Input.HistoryDraft = ""
				m.relayoutComposer()
				return m, nil
			}
		}
		var cmd tea.Cmd
		m.Input.Composer, cmd = m.Input.Composer.Update(msg)
		return m, cmd

	default:
		m.clearPendingAction()
	}

	// Pass all other keys to textarea (Ctrl+A/E/W/U/K, Alt+B/F, etc.)
	var cmd tea.Cmd
	m.Input.Composer, cmd = m.Input.Composer.Update(msg)
	if m.App.Ready {
		m.layout()
	}
	return m, cmd
}

func (m Model) submitComposer() (Model, tea.Cmd) {
	m.clearPendingAction()
	text := strings.TrimSpace(m.Input.Composer.Value())
	if text == "" {
		return m, nil
	}
	if strings.HasPrefix(text, "/") {
		return m.submitText(text)
	}
	if m.InFlight.Thinking || m.Progress.Compacting {
		m.InFlight.QueuedTurns = append(m.InFlight.QueuedTurns, text)
		m.Input.Composer.Reset()
		m.PasteMarkers = make(map[string]pasteMarker)
		m.relayoutComposer()
		return m, m.printEntries(
			session.Entry{Role: session.System, Content: "Queued follow-up"},
		)
	}

	return m.submitText(text)
}

func (m Model) completeSlashCommand() (Model, tea.Cmd, bool) {
	text := m.Input.Composer.Value()
	if !strings.HasPrefix(text, "/") || strings.ContainsAny(text, " \t\r\n") {
		return m, nil, false
	}

	matches := matchingSlashCommands(text)
	switch len(matches) {
	case 0:
		return m, nil, true
	case 1:
		m.Input.Composer.SetValue(matches[0] + " ")
		m.relayoutComposer()
		return m, nil, true
	}

	prefix := commonPrefix(matches)
	if prefix != "" && prefix != text {
		m.Input.Composer.SetValue(prefix)
		m.relayoutComposer()
		return m, nil, true
	}

	return m.openCommandPicker(text), nil, true
}

func (m Model) completeFileReference() (Model, tea.Cmd, bool) {
	text := m.Input.Composer.Value()
	start := lastTokenStart(text)
	token := text[start:]
	if !strings.HasPrefix(token, "@") {
		return m, nil, false
	}

	matches := matchingWorkspaceFileReferences(m.App.Workdir, strings.TrimPrefix(token, "@"))
	switch len(matches) {
	case 0:
		return m, nil, true
	case 1:
		completion := matches[0].reference
		if !matches[0].isDir {
			completion += " "
		}
		m.Input.Composer.SetValue(text[:start] + completion)
		m.relayoutComposer()
		return m, nil, true
	}

	values := make([]string, 0, len(matches))
	for _, match := range matches {
		values = append(values, match.reference)
	}
	if prefix := commonPrefix(values); prefix != "" && prefix != token {
		m.Input.Composer.SetValue(text[:start] + prefix)
		m.relayoutComposer()
	}
	return m, nil, true
}

func lastTokenStart(text string) int {
	idx := strings.LastIndexAny(text, " \t\r\n")
	if idx < 0 {
		return 0
	}
	return idx + 1
}

type fileReferenceMatch struct {
	reference string
	isDir     bool
}

func matchingWorkspaceFileReferences(workdir, query string) []fileReferenceMatch {
	workdir = filepath.Clean(workdir)
	dirPart, base := filepath.Split(filepath.FromSlash(query))
	dirPart = filepath.Clean(dirPart)
	if dirPart == "." {
		dirPart = ""
	}
	dir := filepath.Join(workdir, dirPart)
	rel, err := filepath.Rel(workdir, dir)
	if err != nil || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
		return nil
	}

	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil
	}
	matches := make([]fileReferenceMatch, 0, len(entries))
	for _, entry := range entries {
		name := entry.Name()
		if strings.HasPrefix(name, ".") || !strings.HasPrefix(name, base) {
			continue
		}
		ref := filepath.ToSlash(filepath.Join(dirPart, name))
		if entry.IsDir() {
			ref += "/"
		}
		matches = append(matches, fileReferenceMatch{
			reference: "@" + ref,
			isDir:     entry.IsDir(),
		})
	}
	slices.SortFunc(matches, func(a, b fileReferenceMatch) int {
		return strings.Compare(a.reference, b.reference)
	})
	return matches
}

func matchingSlashCommands(prefix string) []string {
	var matches []string
	for _, command := range slashCommands() {
		if strings.HasPrefix(command, prefix) {
			matches = append(matches, command)
		}
	}
	return matches
}

func commonPrefix(values []string) string {
	if len(values) == 0 {
		return ""
	}
	prefix := values[0]
	for _, value := range values[1:] {
		for !strings.HasPrefix(value, prefix) {
			prefix = prefix[:len(prefix)-1]
			if prefix == "" {
				return ""
			}
		}
	}
	return prefix
}

func (m Model) openCommandPicker(prefix string) Model {
	items := slashCommandItems()
	query := strings.TrimPrefix(strings.TrimSpace(prefix), "/")
	m.Picker.Overlay = &pickerOverlayState{
		title:    "Pick a command",
		items:    items,
		filtered: clonePickerItems(items),
		index:    0,
		query:    query,
		purpose:  pickerPurposeCommand,
	}
	refreshPickerFilter(&m)
	return m
}

func (m *Model) relayoutComposer() {
	if m.App.Ready {
		m.layout()
	}
}
