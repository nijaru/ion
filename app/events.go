package app

import (
	"github.com/nijaru/ion/config"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"time"
	"unicode"

	tea "charm.land/bubbletea/v2"
	ionskills "github.com/nijaru/ion/internal/skills"
)

func keyTextInput(msg tea.KeyPressMsg) (string, bool) {
	if msg.Text == "" {
		return "", false
	}
	for _, r := range msg.Text {
		if unicode.IsControl(r) {
			return "", false
		}
	}
	return msg.Text, true
}

// handleKey is the source of truth for core TUI hotkey semantics.
func (m Model) handleKey(msg tea.KeyPressMsg) (Model, tea.Cmd) {
	if m.Picker.Session != nil {
		return m.handleSessionPickerKey(msg)
	}
	if m.Picker.Setup != nil {
		return m.handleSetupPromptKey(msg)
	}
	if m.Picker.Overlay != nil {
		return m.handlePickerKey(msg)
	}

	if msg.Keystroke() == "ctrl+x" || msg.String() == "\x18" {
		m.clearPendingAction()
		return m.openExternalEditor()
	}

	switch msg.String() {
	case "ctrl+g":
		m.clearPendingAction()
		return m.recallQueuedTurns()

	case "ctrl+m":
		m.clearPendingAction()
		if m.activePreset() == presetFast {
			return m.switchPresetCommand(presetPrimary)
		}
		// If no fast model is configured, open the model picker for fast preset
		cfg, _ := m.commandConfig()
		if cfg != nil && strings.TrimSpace(cfg.FastModel) == "" {
			return m.openModelPickerForPreset(cfg, presetFast)
		}
		return m.switchPresetCommand(presetFast)

	case "ctrl+t":
		m.clearPendingAction()
		return m.openThinkingPicker()

	case "ctrl+c":
		if m.Input.Composer.Value() != "" {
			m.clearPendingAction()
			m.resetComposerDraft()
			return m, nil
		}
		if m.InFlight.Thinking {
			m.clearPendingAction()
			return m.cancelRunningTurn("Canceled by user")
		}
		if m.Input.Pending == pendingActionQuitCtrlC {
			return m, tea.Quit
		}
		return m, m.armPendingAction(pendingActionQuitCtrlC)

	case "ctrl+d":
		if m.Input.Composer.Value() != "" {
			m.clearPendingAction()
			return m, m.updateComposer(msg)
		}
		if m.InFlight.Thinking {
			m.clearPendingAction()
			return m, nil
		}
		if m.Input.Pending == pendingActionQuitCtrlD {
			return m, tea.Quit
		}
		return m, m.armPendingAction(pendingActionQuitCtrlD)

	case "?":
		if strings.TrimSpace(m.Input.Composer.Value()) == "" {
			m.clearPendingAction()
			return m, m.terminalCommit().Help(helpText())
		}

	case "esc":
		if m.InFlight.Thinking {
			if !m.Picker.OverlayClosedAt.IsZero() && time.Since(m.Picker.OverlayClosedAt) < 250*time.Millisecond {
				m.clearPendingAction()
				return m, nil
			}
			m.clearPendingAction()
			return m.cancelRunningTurn("Canceled by user")
		}
		m.clearPendingAction()
		return m, nil

	case "shift+tab":
		m.clearPendingAction()
		return m, nil

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
			m.inputReducer().startDeferredEnter(time.Now().Add(m.Input.PrintHoldDelay))
			return m, m.scheduleDeferredEnter()
		}
		if m.printHoldActive() {
			m.clearPendingAction()
			m.inputReducer().markDeferredEnter()
			return m, m.scheduleDeferredEnter()
		}
		return m.submitComposer()

	case "shift+enter", "alt+enter", "ctrl+j":
		m.clearPendingAction()
		return m, m.insertComposerText("\n")

	case "up", "ctrl+p":
		m.clearPendingAction()
		if m.Input.Composer.Line() == 0 && len(m.Input.History) > 0 {
			if draft, ok := m.inputReducer().previousHistoryDraft(
				m.Input.Composer.Value(),
			); ok {
				return m, m.setComposerDraft(draft)
			}
		}
		return m, m.updateComposer(msg)

	case "down", "ctrl+n":
		m.clearPendingAction()
		if m.Input.Composer.Line() == m.Input.Composer.LineCount()-1 &&
			m.inputReducer().browsingHistory() {
			if draft, ok := m.inputReducer().nextHistoryDraft(); ok {
				return m, m.setComposerDraft(draft)
			}
		}
		return m, m.updateComposer(msg)

	default:
		m.clearPendingAction()
	}

	// Pass all other keys to textarea (Ctrl+A/E/W/U/K, Alt+B/F, etc.)
	return m, m.updateComposer(msg)
}

func (m Model) completeSlashCommand() (Model, tea.Cmd, bool) {
	text := m.Input.Composer.Value()
	if strings.HasPrefix(text, "//") {
		return m.completeCustomCommand()
	}
	if !strings.HasPrefix(text, "/") || strings.ContainsAny(text, "\r\n") {
		return m, nil, false
	}
	if strings.ContainsAny(text, " \t") {
		return m.completeSlashArgument(text)
	}

	matches := matchingSlashCommands(text)
	switch len(matches) {
	case 0:
		return m, nil, true
	case 1:
		return m, m.setComposerDraft(matches[0] + " "), true
	}

	prefix := commonPrefix(matches)
	if prefix != "" && prefix != text {
		return m, m.setComposerDraft(prefix), true
	}

	return m.openCommandPicker(text), nil, true
}

func (m Model) completeSlashArgument(text string) (Model, tea.Cmd, bool) {
	fields := strings.Fields(text)
	if len(fields) == 0 {
		return m, nil, false
	}
	trailingSpace := strings.HasSuffix(text, " ") || strings.HasSuffix(text, "\t")
	switch fields[0] {
	case "/thinking":
		if len(fields) == 1 && trailingSpace {
			return m, nil, true
		}
		if len(fields) == 2 && !trailingSpace {
			return m.completeLastSlashToken(text, thinkingCompletionValues())
		}
	case "/settings":
		if len(fields) == 1 && trailingSpace {
			return m, nil, true
		}
		if len(fields) == 2 && !trailingSpace {
			return m.completeLastSlashToken(text, settingsCompletionKeys())
		}
		if len(fields) == 2 && trailingSpace {
			return m, nil, true
		}
		if len(fields) == 3 && !trailingSpace {
			switch normalizeSettingsCompletionKey(fields[1]) {
			case "retry":
				return m.completeLastSlashToken(text, []string{"on", "off"})
			case "tool":
				return m.completeLastSlashToken(
					text,
					[]string{"auto", "full", "collapsed", "hidden"},
				)
			case "read":
				return m.completeLastSlashToken(text, []string{"full", "summary", "hidden"})
			case "write":
				return m.completeLastSlashToken(text, []string{"diff", "summary", "hidden"})
			case "bash":
				return m.completeLastSlashToken(text, []string{"full", "summary", "hidden"})
			case "thinking":
				return m.completeLastSlashToken(text, []string{"full", "collapsed", "hidden"})
			case "busy":
				return m.completeLastSlashToken(text, []string{"queue", "steer"})
			}
		}
	}
	return m, nil, false
}

func (m Model) completeLastSlashToken(text string, values []string) (Model, tea.Cmd, bool) {
	start := lastTokenStart(text)
	prefix := text[start:]
	matches := matchingValues(prefix, values)
	switch len(matches) {
	case 0:
		return m, nil, true
	case 1:
		return m, m.setComposerDraft(text[:start] + matches[0] + " "), true
	default:
		if common := commonPrefix(matches); common != "" && common != prefix {
			return m, m.setComposerDraft(text[:start] + common), true
		}
		return m, nil, true
	}
}

func (m Model) completeFileReference() (Model, tea.Cmd, bool) {
	text := m.Input.Composer.Value()
	start, token, ok := fileReferenceCompletionToken(text)
	if !ok {
		return m, nil, false
	}
	requestID := m.inputReducer().beginFileCompletionRequest()
	m.inputReducer().clearCompletion()
	return m, loadFileReferenceCompletion(
		requestID,
		m.App.Workdir,
		text,
		start,
		token,
		true,
	), true
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
	if strings.TrimSpace(workdir) == "" {
		return nil
	}
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
	if !pathInsideWorkspace(workdir, dir) {
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

func pathInsideWorkspace(workdir, path string) bool {
	realWorkdir, err := filepath.EvalSymlinks(workdir)
	if err != nil {
		return false
	}
	realPath, err := filepath.EvalSymlinks(path)
	if err != nil {
		return false
	}
	rel, err := filepath.Rel(realWorkdir, realPath)
	return err == nil && rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator))
}

func matchingSlashCommands(query string) []string {
	query = strings.TrimPrefix(strings.TrimSpace(query), "/")
	if query == "" {
		return slashCommands()
	}

	search := preparePickerSearchQuery(query)
	var ranked []rankedPickerItem
	for i, item := range slashCommandItems() {
		fields := []pickerSearchField{
			{value: normalizeSearchQuery(item.Label), weight: 0},
			{value: normalizeSearchQuery(strings.TrimPrefix(item.Value, "/")), weight: 5},
		}
		score, ok := pickerSearchScorePrepared(search, fields)
		if !ok {
			continue
		}
		ranked = append(ranked, rankedPickerItem{
			item:     item,
			score:    score,
			index:    i,
			labelKey: strings.ToLower(item.Label),
			valueKey: strings.ToLower(item.Value),
		})
	}

	slices.SortFunc(ranked, func(a, b rankedPickerItem) int {
		if a.score != b.score {
			return a.score - b.score
		}
		if cmp := strings.Compare(a.labelKey, b.labelKey); cmp != 0 {
			return cmp
		}
		if cmp := strings.Compare(a.valueKey, b.valueKey); cmp != 0 {
			return cmp
		}
		return a.index - b.index
	})

	if len(ranked) > 0 {
		best := ranked[0].score
		var filtered []string
		for _, r := range ranked {
			if r.score <= best+50 {
				filtered = append(filtered, r.item.Value)
			}
		}
		return filtered
	}

	return nil
}

func matchingValues(prefix string, values []string) []string {
	prefix = strings.ToLower(prefix)
	var matches []string
	for _, value := range values {
		if strings.HasPrefix(value, prefix) {
			matches = append(matches, value)
		}
	}
	return matches
}

func thinkingCompletionValues() []string {
	return []string{"auto", "off", "minimal", "low", "medium", "high", "xhigh"}
}

func settingsCompletionKeys() []string {
	return []string{"retry", "tool", "read", "write", "bash", "thinking", "busy"}
}

func normalizeSettingsCompletionKey(value string) string {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "tools":
		return "tool"
	case "busy_input":
		return "busy"
	default:
		return strings.ToLower(strings.TrimSpace(value))
	}
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
	m.pickerReducer().openOverlay(pickerOverlayState{
		title:    "Pick a command",
		items:    items,
		filtered: clonePickerItems(items),
		index:    0,
		query:    query,
		purpose:  pickerPurposeCommand,
	})
	refreshPickerFilter(&m)
	return m
}

func (m *Model) relayoutComposer() {
	if m.App.Ready {
		m.layout()
	}
}

func (m Model) completeCustomCommand() (Model, tea.Cmd, bool) {
	text := m.Input.Composer.Value()
	if !strings.HasPrefix(text, "//") || strings.ContainsAny(text, "\r\n") {
		return m, nil, false
	}
	if strings.ContainsAny(text, " \t") {
		return m, nil, true
	}

	matches := m.matchingCustomCommands(text)
	switch len(matches) {
	case 0:
		return m, nil, true
	case 1:
		return m, m.setComposerDraft(matches[0] + " "), true
	}

	prefix := commonPrefix(matches)
	if prefix != "" && prefix != text {
		return m, m.setComposerDraft(prefix), true
	}

	return m.openCustomCommandPicker(text), nil, true
}

func (m Model) matchingCustomCommands(query string) []string {
	query = strings.TrimPrefix(strings.TrimSpace(query), "//")
	dir, err := config.DefaultSkillsDir()
	if err != nil {
		return nil
	}
	skillSummaries, err := ionskills.List(dir)
	if err != nil {
		return nil
	}

	var pickerItems []pickerItem
	for _, skill := range skillSummaries {
		search := pickerSearchIndex(
			"//"+skill.Name,
			skill.Name,
			skill.Description,
			"Skills",
			nil,
		)
		pickerItems = append(pickerItems, pickerItem{
			Label:  "//" + skill.Name,
			Value:  "//" + skill.Name,
			Detail: skill.Description,
			Group:  "Skills",
			Search: search,
		})
	}

	if query == "" {
		out := make([]string, 0, len(pickerItems))
		for _, item := range pickerItems {
			out = append(out, item.Value)
		}
		return out
	}

	ranked := rankedPickerItems(pickerItems, query)
	out := make([]string, 0, len(ranked))
	for _, item := range ranked {
		out = append(out, item.Value)
	}
	return out
}

func (m Model) openCustomCommandPicker(prefix string) Model {
	dir, err := config.DefaultSkillsDir()
	if err != nil {
		return m
	}
	skillSummaries, err := ionskills.List(dir)
	if err != nil {
		return m
	}
	var items []pickerItem
	for _, skill := range skillSummaries {
		search := pickerSearchIndex(
			"//"+skill.Name,
			skill.Name,
			skill.Description,
			"Skills",
			nil,
		)
		items = append(items, pickerItem{
			Label:  "//" + skill.Name,
			Value:  "//" + skill.Name,
			Detail: skill.Description,
			Group:  "Skills",
			Search: search,
		})
	}

	query := strings.TrimPrefix(strings.TrimSpace(prefix), "//")
	m.pickerReducer().openOverlay(pickerOverlayState{
		title:    "Pick a skill",
		items:    items,
		filtered: clonePickerItems(items),
		index:    0,
		query:    query,
		purpose:  pickerPurposeCommand,
	})
	refreshPickerFilter(&m)
	return m
}
