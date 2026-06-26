package app

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
)

// inputReducer handles input state mutations.
type inputReducer struct {
	input        *InputState
	pasteMarkers *map[string]pasteMarker
}

func (m *Model) inputReducer() inputReducer {
	return inputReducer{
		input:        &m.Input,
		pasteMarkers: &m.PasteMarkers,
	}
}

func (r inputReducer) updateComposer(msg tea.Msg) tea.Cmd {
	var cmd tea.Cmd
	r.input.Composer, cmd = r.input.Composer.Update(msg)
	return cmd
}

func (r inputReducer) insertComposerText(value string) {
	r.input.Composer.InsertString(value)
}

func (r inputReducer) resetComposerDraft() {
	r.input.Composer.Reset()
	r.clearCompletion()
	if r.pasteMarkers != nil {
		*r.pasteMarkers = make(map[string]pasteMarker)
	}
}

func (r inputReducer) setComposerDraft(value string) {
	r.input.Composer.SetValue(value)
}

func (r inputReducer) resetHistoryCursor() {
	r.input.HistoryIdx = -1
	r.input.HistoryDraft = ""
}

func (r inputReducer) appendHistory(text string) (string, bool) {
	text = strings.TrimSpace(text)
	if text == "" {
		return "", false
	}
	if len(r.input.History) > 0 && r.input.History[len(r.input.History)-1] == text {
		r.resetHistoryCursor()
		return "", false
	}
	r.input.History = append(r.input.History, text)
	if overflow := len(r.input.History) - maxInputHistoryEntries; overflow > 0 {
		r.input.History = append([]string(nil), r.input.History[overflow:]...)
	}
	r.resetHistoryCursor()
	return text, true
}

func (r inputReducer) setHistory(inputs []string) {
	r.input.History = inputs
	r.resetHistoryCursor()
}

func (r inputReducer) setCompletionItems(items []completionItem) {
	if len(items) == 0 {
		r.clearCompletion()
		return
	}
	r.input.Completion = &completionState{items: items}
}

func (r inputReducer) clearCompletion() {
	r.input.Completion = nil
}

func (r inputReducer) beginFileCompletionRequest() uint64 {
	r.input.FileCompletionRequest++
	return r.input.FileCompletionRequest
}

func (r inputReducer) invalidateFileCompletionRequest() {
	r.input.FileCompletionRequest++
}

func (r inputReducer) clearPasteMarkers() {
	if r.pasteMarkers == nil {
		return
	}
	*r.pasteMarkers = make(map[string]pasteMarker)
}

func (r inputReducer) previousHistoryDraft(current string) (string, bool) {
	if len(r.input.History) == 0 {
		return "", false
	}
	if r.input.HistoryIdx == -1 {
		r.input.HistoryDraft = current
		r.input.HistoryIdx = len(r.input.History) - 1
		return r.input.History[r.input.HistoryIdx], true
	}
	if r.input.HistoryIdx <= 0 {
		return "", false
	}
	r.input.HistoryIdx--
	return r.input.History[r.input.HistoryIdx], true
}

func (r inputReducer) nextHistoryDraft() (string, bool) {
	if r.input.HistoryIdx == -1 {
		return "", false
	}
	if r.input.HistoryIdx < len(r.input.History)-1 {
		r.input.HistoryIdx++
		return r.input.History[r.input.HistoryIdx], true
	}
	draft := r.input.HistoryDraft
	r.resetHistoryCursor()
	return draft, true
}

func (r inputReducer) browsingHistory() bool {
	return r.input.HistoryIdx != -1
}

func (r inputReducer) clearPendingAction() {
	r.input.Pending = pendingActionNone
}

func (r inputReducer) armPendingAction(action pendingAction) {
	r.input.Pending = action
}

func (r inputReducer) holdEnter(delay time.Duration) {
	if delay > r.input.PrintHoldDelay {
		r.input.PrintHoldDelay = delay
	}
	r.input.DelayNextEnter = true
}

func (r inputReducer) startDeferredEnter(until time.Time) {
	r.input.DelayNextEnter = false
	r.input.DeferredEnter = true
	r.input.PrintHoldUntil = until
}

func (r inputReducer) markDeferredEnter() {
	r.input.DeferredEnter = true
}

func (r inputReducer) finishDeferredEnter() {
	r.input.DeferredEnter = false
	r.input.PrintHoldDelay = 0
}

const maxInputHistoryEntries = 200

// Model-level input methods (delegate to inputReducer).

func (m *Model) updateComposer(msg tea.Msg) tea.Cmd {
	cmd := m.inputReducer().updateComposer(msg)
	m.relayoutComposer()
	return tea.Batch(cmd, m.refreshComposerCompletions())
}

func (m *Model) insertComposerText(value string) tea.Cmd {
	m.inputReducer().insertComposerText(value)
	m.relayoutComposer()
	return m.refreshComposerCompletions()
}

func (m *Model) clearPasteMarkers() {
	m.inputReducer().clearPasteMarkers()
}

func (m *Model) resetHistoryCursor() {
	m.inputReducer().resetHistoryCursor()
}

func (m *Model) resetComposerDraft() {
	m.inputReducer().resetComposerDraft()
	m.inputReducer().invalidateFileCompletionRequest()
	m.relayoutComposer()
}

func (m *Model) setComposerDraft(value string) tea.Cmd {
	m.inputReducer().setComposerDraft(value)
	m.relayoutComposer()
	return m.refreshComposerCompletions()
}

func (m *Model) appendInputHistory(text string) (string, bool) {
	return m.inputReducer().appendHistory(text)
}

func (m *Model) loadInputHistory(ctx context.Context) {
	if m.Model.Store == nil || strings.TrimSpace(m.App.Workdir) == "" {
		return
	}
	inputs, err := m.Model.Store.GetInputs(ctx, m.App.Workdir, maxInputHistoryEntries)
	if err != nil {
		return
	}
	slices.Reverse(inputs)
	m.inputReducer().setHistory(inputs)
}

func (m Model) persistInputHistory(ctx context.Context, text string) tea.Cmd {
	if m.Model.Store == nil || strings.TrimSpace(m.App.Workdir) == "" {
		return nil
	}
	store := m.Model.Store
	workdir := m.App.Workdir
	return func() tea.Msg {
		if err := store.AddInput(ctx, workdir, text); err != nil {
			return localErrorMsg{err: fmt.Errorf("persist input history: %w", err)}
		}
		return nil
	}
}

// Status line and layout.

func (m Model) statusLine() string {
	width := m.shellWidth()
	contentWidth := width - 1
	if contentWidth <= 0 {
		return insetStatusLine("", width)
	}
	if hint := strings.TrimSpace(m.pendingActionStatus()); hint != "" {
		return insetStatusLine(m.st.warn.Render(hint), width)
	}

	sep := m.st.sep.Render(" • ")

	provider := m.runtimeProvider()
	model := m.runtimeModel()
	limit := 0
	if m.Model.Backend != nil {
		limit = m.Model.Backend.ContextLimit()
	}
	if provider != "" {
		provider = m.st.dim.Render(providerDisplayName(provider))
	}
	if model != "" {
		model = m.st.dim.Render(m.statusModelLabel(model))
	}
	thinking := m.st.dim.Render(normalizeThinkingValue(m.Progress.ReasoningEffort))
	dir := m.st.dim.Render(statusWorkdirLabel(m.App.Workdir))
	branch := ""
	if m.App.Branch != "" {
		branch = m.st.dim.Render(m.App.Branch)
	}
	gitDiff := ""
	if value := strings.TrimSpace(m.App.GitDiff); value != "" {
		gitDiff = m.st.dim.Render(value)
	}

	usage := m.renderContextUsage(m.Progress.ContextTokens, limit)

	cost := ""
	if label := m.costBudgetLabel(m.Progress.TotalCost); label != "" {
		cost = m.st.dim.Render(label)
	}

	candidates := [][]string{
		{
			provider,
			model,
			thinking,
			usage,
			cost,
			dir,
			branch,
			gitDiff,
		},
		{provider, model, thinking, usage, cost, gitDiff},
		{provider, model, thinking, usage, cost},
		{model, thinking, usage, cost},
		{thinking, usage, cost},
	}
	for _, segments := range candidates {
		line := joinLineSegments(sep, segments...)
		if ansi.StringWidth(line) <= contentWidth {
			return insetStatusLine(line, width)
		}
	}

	return insetStatusLine(joinLineSegments(sep, thinking, usage, cost), width)
}

func insetStatusLine(line string, width int) string {
	if width <= 0 {
		return ""
	}
	return " " + fitLine(line, width-1)
}

func (m Model) statusModelLabel(model string) string {
	if m.activePreset() != presetFast {
		return model
	}
	return model + " (fast)"
}

func (m Model) renderContextUsage(total, limit int) string {
	if total <= 0 {
		return ""
	}
	if limit <= 0 {
		return m.st.dim.Render(fmt.Sprintf("%s tokens", compactCount(total)))
	}

	pct := (total * 100) / limit
	label := fmt.Sprintf("%s/%s (%d%%)", compactCount(total), compactCount(limit), pct)
	switch {
	case pct >= 80:
		return m.st.warn.Render(label)
	case pct >= 50:
		return m.st.caution.Render(label)
	default:
		return m.st.success.Render(label)
	}
}

func statusWorkdirLabel(workdir string) string {
	if strings.TrimSpace(workdir) == "" {
		return ""
	}
	label := filepath.Base(filepath.Clean(workdir))
	if label == string(os.PathSeparator) {
		return label
	}
	return label + string(os.PathSeparator)
}

func (m *Model) layout() {
	width := m.shellWidth()
	if width <= 0 {
		width = 1
	}
	contentWidth := width - composerPromptWidth()
	if contentWidth <= 0 {
		contentWidth = 1
	}
	m.Input.Composer.SetWidth(contentWidth)
}

func (m Model) handleWindowSize(msg tea.WindowSizeMsg) (Model, tea.Cmd) {
	oldWidth := m.App.Width
	m.App.Ready = true
	m.App.Width = msg.Width
	m.App.Height = msg.Height
	m.layout()
	if oldWidth > 0 && msg.Width > 0 && msg.Width < oldWidth {
		return m, clearVisibleScreenCmd()
	}
	return m, nil
}

func (m Model) headerLine() string {
	return m.headerLineFor(m.App.Branch)
}

func (m Model) headerLineFor(branch string) string {
	sep := m.st.dim.Render(" • ")

	home, _ := os.UserHomeDir()
	dir := shortenHomePath(m.App.Workdir, home)

	pathParts := []string{m.st.dim.Render(dir)}
	if branch != "" {
		pathParts = append(pathParts, m.st.dim.Render(branch))
	}
	return strings.Join(pathParts, sep)
}

func shortenHomePath(path, home string) string {
	if home == "" || path == "" {
		return path
	}
	if path == home {
		return "~"
	}
	prefix := strings.TrimRight(home, string(os.PathSeparator)) + string(os.PathSeparator)
	if strings.HasPrefix(path, prefix) {
		return "~" + string(os.PathSeparator) + strings.TrimPrefix(path, prefix)
	}
	return path
}
