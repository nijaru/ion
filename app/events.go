package app

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"slices"
	"strconv"
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
	if m.Picker.Approval != nil {
		return m.handleApprovalKey(msg)
	}
	if m.Picker.Session != nil {
		return m.handleSessionPickerKey(msg)
	}
	if m.Picker.UserMessage != nil {
		return m.handleUserMessageForkPickerKey(msg)
	}
	if m.Picker.Setup != nil {
		return m.handleSetupPromptKey(msg)
	}
	if m.Picker.BranchSummary != nil {
		return m.handleBranchSummaryPromptKey(msg)
	}
	if m.Picker.Tree != nil {
		return m.handleTreePickerKey(msg)
	}
	if m.Picker.Overlay != nil {
		return m.handlePickerKey(msg)
	}

	switch msg.String() {
	case "ctrl+g", "alt+e", "meta+e":
		m.clearPendingAction()
		return m.openExternalEditor()

	case "ctrl+l", "ctrl+p":
		m.clearPendingAction()
		return m.cycleScopedModelCommand(true)

	case "ctrl+shift+l", "ctrl+shift+p":
		m.clearPendingAction()
		return m.cycleScopedModelCommand(false)

	case "ctrl+x":
		m.clearPendingAction()
		return m.copyLastResponse()

	case "ctrl+t":
		m.clearPendingAction()
		if m.Model.Config != nil && m.Model.Config.ThinkingVerbosity == "expanded" {
			m.Model.Config.ThinkingVerbosity = "collapsed"
			return m, m.terminalCommit().Entries(systemEntry("Thinking blocks: collapsed"))
		}
		if m.Model.Config != nil {
			m.Model.Config.ThinkingVerbosity = "expanded"
			return m, m.terminalCommit().Entries(systemEntry("Thinking blocks: expanded"))
		}
		return m, nil

	case "ctrl+o":
		m.clearPendingAction()
		m.ToolOutputExpanded = !m.ToolOutputExpanded
		return m, nil

	case "ctrl+-", "ctrl+_", "ctrl+minus":
		m.clearPendingAction()
		return m.handleUndoCommand()

	case "ctrl+r":
		m.clearPendingAction()
		return m.openHistoryPicker(), nil

	case "ctrl+z":
		m.clearPendingAction()
		return m, tea.Suspend

	case "ctrl+v":
		m.clearPendingAction()
		return m.pasteImageFromClipboard()

	case "ctrl+c":
		// Ctrl+C clears a non-empty editor.
		// Escape cancels/aborts. Ctrl+D exits when empty.
		if m.Input.Composer.Value() != "" {
			m.clearPendingAction()
			m.resetComposerDraft()
			return m, nil
		}
		// Editor is empty — arm quit on second press
		if m.Input.Pending == pendingActionQuitCtrlC {
			return m, tea.Quit
		}
		return m, m.armPendingAction(pendingActionQuitCtrlC)

	case "ctrl+d":
		if m.Input.Composer.Value() != "" {
			m.clearPendingAction()
			return m, m.updateComposer(msg)
		}
		if m.InFlight.Thinking || m.InFlight.AwaitingSettlement {
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
		if m.Input.Completion != nil && len(m.Input.Completion.items) > 0 {
			m.clearPendingAction()
			m.inputReducer().clearCompletion()
			return m, nil
		}
		if m.Progress.Compacting {
			m.clearPendingAction()
			if m.Model.compactionCancel != nil {
				m.Model.compactionCancel()
				return m, m.terminalCommit().Entries(systemEntry("Canceling compaction..."))
			}
			return m, nil
		}
		// Double Escape opens tree or user message fork selector when idle (matching Pi).
		if !m.InFlight.Thinking && !m.InFlight.AwaitingSettlement &&
			m.Input.Pending == pendingActionNone {
			now := time.Now()
			if !m.Picker.LastEscAt.IsZero() && now.Sub(m.Picker.LastEscAt) < 500*time.Millisecond {
				m.Picker.LastEscAt = time.Time{}
				if m.Model.Config != nil && m.Model.Config.DoubleEscapeAction == "fork" {
					return m.openUserMessageForkPicker()
				}
				return m.openTreePicker()
			}
			m.Picker.LastEscAt = now
			return m, nil
		}
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
		// Shift+Tab opens the thinking-level picker.
		m.clearPendingAction()
		return m.openThinkingPicker()

	case "tab":
		if m.Input.Completion != nil && len(m.Input.Completion.items) > 0 {
			if next, cmd, ok := m.applyActiveCompletion(); ok {
				return next, cmd
			}
		}
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

	case "shift+enter", "ctrl+j":
		m.clearPendingAction()
		return m, m.insertComposerText("\n")

	case "alt+enter":
		m.clearPendingAction()
		if m.InFlight.Thinking || m.InFlight.AwaitingSettlement {
			// Queue follow-up message when agent is streaming or settling.
			return m.queueFollowUp()
		}
		// When idle, insert newline
		return m, m.insertComposerText("\n")

	case "alt+up":
		m.clearPendingAction()
		if next, cmd, ok := m.recallQueuedInputToComposer(); ok {
			return next, cmd
		}
		if m.Input.Composer.Line() == 0 && len(m.Input.History) > 0 {
			if draft, ok := m.inputReducer().previousHistoryDraft(
				m.Input.Composer.Value(),
			); ok {
				return m, m.setComposerDraft(draft)
			}
		}
		return m, m.updateComposer(msg)

	case "up":
		m.clearPendingAction()
		if m.Input.Completion != nil && len(m.Input.Completion.items) > 1 {
			m.inputReducer().moveCompletionUp()
			return m, nil
		}
		if m.Input.Composer.Line() == 0 && len(m.Input.History) > 0 {
			if draft, ok := m.inputReducer().previousHistoryDraft(
				m.Input.Composer.Value(),
			); ok {
				return m, m.setComposerDraft(draft)
			}
		}
		return m, m.updateComposer(msg)

	case "down":
		m.clearPendingAction()
		if m.Input.Completion != nil && len(m.Input.Completion.items) > 1 {
			m.inputReducer().moveCompletionDown()
			return m, nil
		}
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

func (m Model) applyActiveCompletion() (Model, tea.Cmd, bool) {
	item, ok := m.inputReducer().selectedCompletionItem()
	if !ok {
		return m, nil, false
	}
	text := m.Input.Composer.Value()
	if strings.HasPrefix(text, "/") && !strings.ContainsAny(text, " \t\r\n") {
		m.inputReducer().clearCompletion()
		return m, m.setComposerDraft(item.Label + " "), true
	}
	if start, _, ok := fileReferenceCompletionToken(text); ok {
		replacement := item.Label
		if !strings.HasSuffix(replacement, "/") {
			replacement += " "
		}
		m.inputReducer().clearCompletion()
		return m, m.setComposerDraft(text[:start] + replacement), true
	}
	if strings.HasPrefix(text, "//") {
		m.inputReducer().clearCompletion()
		return m, m.setComposerDraft(item.Label + " "), true
	}
	return m, nil, false
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
		m.Model.EventGeneration,
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
	return []string{"auto", "off", "minimal", "low", "medium", "high", "xhigh", "max"}
}

func settingsCompletionKeys() []string {
	return []string{"retry", "tool", "tool_mode", "read", "write", "bash", "thinking", "reasoning", "busy"}
}

func normalizeSettingsCompletionKey(value string) string {
	switch strings.ToLower(strings.TrimSpace(value)) {
	case "tools":
		return "tool"
	case "toolmode", "active_tools":
		return "tool_mode"
	case "thinking_level", "reasoning_effort":
		return "reasoning"
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

func (m Model) openHistoryPicker() Model {
	var items []pickerItem
	seen := make(map[string]bool)
	for i := len(m.Input.History) - 1; i >= 0; i-- {
		text := strings.TrimSpace(m.Input.History[i])
		if text == "" || seen[text] {
			continue
		}
		seen[text] = true
		preview := text
		if len(preview) > 80 {
			preview = preview[:77] + "..."
		}
		items = append(items, pickerItem{
			Label: preview,
			Value: text,
		})
	}
	if m.Model.InputHistory != nil {
		if persistent, err := m.Model.InputHistory.GetInputs(context.Background(), m.App.Workdir, 100); err == nil {
			for _, text := range persistent {
				text = strings.TrimSpace(text)
				if text == "" || seen[text] {
					continue
				}
				seen[text] = true
				preview := text
				if len(preview) > 80 {
					preview = preview[:77] + "..."
				}
				items = append(items, pickerItem{
					Label: preview,
					Value: text,
				})
			}
		}
	}
	if len(items) == 0 {
		return m
	}
	m.pickerReducer().openOverlay(pickerOverlayState{
		title:    "Search prompt history",
		items:    items,
		filtered: clonePickerItems(items),
		index:    0,
		purpose:  pickerPurposeHistory,
	})
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

	requestID, ctx := m.inputReducer().beginSkillCompletionRequest(m.runtimeOperationContext())
	m.inputReducer().clearCompletion()
	return m, loadSkillCompletion(
		ctx,
		m.Model.EventGeneration,
		requestID,
		text,
		true,
	), true
}

func (m Model) applyCustomCommandCompletion(
	text string,
	summaries []ionskills.Summary,
) (Model, tea.Cmd) {
	matches := matchingCustomCommandsFromSummaries(text, summaries)
	switch len(matches) {
	case 0:
		return m, nil
	case 1:
		return m, m.setComposerDraft(matches[0] + " ")
	}

	prefix := commonPrefix(matches)
	if prefix != "" && prefix != text {
		return m, m.setComposerDraft(prefix)
	}

	return m.openCustomCommandPicker(text, summaries), nil
}

func matchingCustomCommandsFromSummaries(query string, summaries []ionskills.Summary) []string {
	query = strings.TrimPrefix(strings.TrimSpace(query), "//")
	items := skillPickerItems(summaries)
	if query == "" {
		out := make([]string, 0, len(items))
		for _, item := range items {
			out = append(out, item.Value)
		}
		return out
	}

	ranked := rankedPickerItems(items, query)
	out := make([]string, 0, len(ranked))
	for _, item := range ranked {
		out = append(out, item.Value)
	}
	return out
}

func (m Model) openCustomCommandPicker(prefix string, summaries []ionskills.Summary) Model {
	items := skillPickerItems(summaries)
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

const gitDiffStatsTimeout = 1500 * time.Millisecond

var (
	gitDiffInsertionsPattern = regexp.MustCompile(`(\d+) insertion`)
	gitDiffDeletionsPattern  = regexp.MustCompile(`(\d+) deletion`)
)

func loadGitDiffStats(ctx context.Context, generation uint64, workdir string) tea.Cmd {
	workdir = strings.TrimSpace(workdir)
	if workdir == "" {
		return nil
	}
	if ctx == nil {
		ctx = context.Background()
	}
	return func() tea.Msg {
		if err := ctx.Err(); err != nil {
			return nil
		}
		return gitDiffStatsMsg{
			generation: generation,
			workdir:    workdir,
			stats:      currentGitDiffStats(ctx, workdir),
		}
	}
}

func (m Model) handleGitDiffStats(msg gitDiffStatsMsg) (Model, tea.Cmd) {
	if msg.generation != m.Model.EventGeneration {
		return m, nil
	}
	if msg.workdir == m.App.Workdir {
		m.App.GitDiff = msg.stats
	}
	return m, nil
}

func currentGitDiffStats(parent context.Context, workdir string) string {
	if parent == nil {
		parent = context.Background()
	}
	ctx, cancel := context.WithTimeout(parent, gitDiffStatsTimeout)
	defer cancel()

	out, err := exec.CommandContext(ctx, "git", "-C", workdir, "diff", "--shortstat", "HEAD", "--").
		Output()
	if err != nil {
		return ""
	}
	return parseGitDiffShortstat(string(out))
}

func parseGitDiffShortstat(output string) string {
	insertions := parseGitDiffCount(gitDiffInsertionsPattern, output)
	deletions := parseGitDiffCount(gitDiffDeletionsPattern, output)

	var parts []string
	if insertions > 0 {
		parts = append(parts, "+"+strconv.Itoa(insertions))
	}
	if deletions > 0 {
		parts = append(parts, "-"+strconv.Itoa(deletions))
	}
	return strings.Join(parts, "/")
}

func parseGitDiffCount(pattern *regexp.Regexp, output string) int {
	match := pattern.FindStringSubmatch(output)
	if len(match) != 2 {
		return 0
	}
	value, err := strconv.Atoi(match[1])
	if err != nil {
		return 0
	}
	return value
}
