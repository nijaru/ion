package app

import (
	"context"
	"errors"
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/session"
)

// treePickerState holds the interactive session tree navigator state.
type treePickerState struct {
	tree    *SessionTree
	cursor  int
	entries []treePickerEntry
	loading bool
	err     string
}

type treePickerEntry struct {
	indent  int    // tree depth for display indentation
	id      string // entry ID
	title   string // human-readable title
	isLeaf  bool   // true if this is the current position
	kind    string // entry kind classification
	summary string // branch summary if available
}

// treePickerLoadedMsg carries the loaded session tree.
type treePickerLoadedMsg struct {
	tree SessionTree
	err  error
}

// treePickerMoveMsg confirms that a NavigateTree operation completed.
type treePickerMoveMsg struct {
	err       error
	cancelled bool
}

func (m Model) openTreePicker() (Model, tea.Cmd) {
	if m.Model.Store == nil {
		m.showTreeUnavailable()
		return m, nil
	}
	if m.activeSession() == nil {
		m.showTreeUnavailable()
		return m, nil
	}
	leafID := m.Model.Store.GetLeafID()
	if leafID == "" {
		m.showTreeUnavailable()
		return m, nil
	}

	m.Picker.Tree = &treePickerState{loading: true}

	return m, func() tea.Msg {
		tree, err := loadSessionTree(context.Background(), m.Model.Store, leafID)
		return treePickerLoadedMsg{tree: tree, err: err}
	}
}

// loadSessionTree projects the persisted entry graph into the picker view.
// Store owns the graph; app owns this display projection. Keeping the
// projection here avoids adding a UI-shaped interface to session or making
// SQLite import app types.
func loadSessionTree(ctx context.Context, store session.Store, leafID string) (SessionTree, error) {
	entries, err := store.Entries(ctx)
	if err != nil {
		return SessionTree{}, err
	}

	byID := make(map[string]session.Entry, len(entries))
	for _, entry := range entries {
		if entry != nil {
			byID[entry.ID()] = entry
		}
	}
	current, ok := byID[leafID]
	if !ok {
		return SessionTree{}, fmt.Errorf("session tree leaf %q not found", leafID)
	}

	var reverseLineage []session.Entry
	seen := map[string]bool{leafID: true}
	for parentID := current.ParentID(); parentID != ""; {
		if seen[parentID] {
			return SessionTree{}, fmt.Errorf("session tree contains a parent cycle at %q", parentID)
		}
		seen[parentID] = true
		parent, ok := byID[parentID]
		if !ok {
			return SessionTree{}, fmt.Errorf("session tree parent %q not found", parentID)
		}
		reverseLineage = append(reverseLineage, parent)
		parentID = parent.ParentID()
	}

	lineage := make([]session.Entry, len(reverseLineage))
	for i := range reverseLineage {
		lineage[len(reverseLineage)-1-i] = reverseLineage[i]
	}

	children := make([]session.Entry, 0)
	for _, entry := range entries {
		if entry != nil && entry.ParentID() == leafID {
			children = append(children, entry)
		}
	}

	return SessionTree{Current: current, Lineage: lineage, Children: children}, nil
}

func (m Model) closeTreePicker() Model {
	m.Picker.Tree = nil
	m.Picker.BranchSummary = nil
	return m
}

func (m Model) showTreeUnavailable() {
	msg := "session tree is not available (no store, no session, or unsupported)"
	m.terminalCommit().Entries(systemEntry("⚠ " + msg))
}

// handleTreePickerLoaded processes the loaded session tree.
func (m Model) handleTreePickerLoaded(msg treePickerLoadedMsg) (Model, tea.Cmd) {
	if msg.err != nil {
		m.Picker.Tree = &treePickerState{
			err:     msg.err.Error(),
			loading: false,
		}
		return m, nil
	}

	m.Picker.Tree = &treePickerState{
		tree:    &msg.tree,
		loading: false,
	}
	m.Picker.Tree.buildEntries()
	return m, nil
}

// buildEntries flattens the session tree into a displayable list.
func (t *treePickerState) buildEntries() {
	if t.tree == nil {
		return
	}
	t.entries = t.entries[:0]

	// Add current (leaf) entry.
	t.entries = append(t.entries, treePickerEntry{
		indent:  0,
		id:      t.tree.Current.ID(),
		title:   entryDisplayTitle(t.tree.Current),
		isLeaf:  true,
		kind:    classifyEntry(t.tree.Current),
		summary: entrySummary(t.tree.Current),
	})

	// Add lineage (root → parent).
	for _, e := range t.tree.Lineage {
		t.entries = append(t.entries, treePickerEntry{
			indent:  1,
			id:      e.ID(),
			title:   entryDisplayTitle(e),
			isLeaf:  false,
			kind:    classifyEntry(e),
			summary: entrySummary(e),
		})
	}

	// Add children.
	for _, e := range t.tree.Children {
		t.entries = append(t.entries, treePickerEntry{
			indent:  1,
			id:      e.ID(),
			title:   entryDisplayTitle(e),
			isLeaf:  false,
			kind:    classifyEntry(e),
			summary: entrySummary(e),
		})
	}
}

func entryDisplayTitle(e session.Entry) string {
	title := session.EntryTitle(e)
	if title != "" {
		return title
	}
	// Fall back to truncated ID.
	id := e.ID()
	if len(id) > 8 {
		id = id[:8]
	}
	return id
}

func entrySummary(e session.Entry) string {
	switch e := e.(type) {
	case *session.BranchSummaryEntry:
		s := e.Summary
		// Use first line of summary.
		if idx := strings.Index(s, "\n"); idx >= 0 {
			s = s[:idx]
		}
		if len(s) > 80 {
			s = s[:80] + "…"
		}
		return s
	}
	return ""
}

func classifyEntry(e session.Entry) string {
	switch e.(type) {
	case *session.MessageEntry:
		return "message"
	case *session.CompactionEntry:
		return "compaction"
	case *session.BranchSummaryEntry:
		return "branch summary"
	case *session.SessionInfoEntry:
		return "session info"
	case *session.ModelChangeEntry:
		return "model change"
	case *session.CustomMessageEntry:
		return "custom msg"
	case *session.LeafEntry:
		return "leaf"
	default:
		return "entry"
	}
}

// handleTreePickerKey processes keyboard input in the tree picker.
func (m Model) handleTreePickerKey(msg tea.KeyPressMsg) (Model, tea.Cmd) {
	if m.Picker.Tree == nil {
		return m, nil
	}
	t := m.Picker.Tree

	switch msg.String() {
	case "esc", "ctrl+c", "ctrl+d":
		return m.closeTreePicker(), nil

	case "up":
		if t.cursor > 0 {
			t.cursor--
		}

	case "down":
		if t.cursor < len(t.entries)-1 {
			t.cursor++
		}

	case "enter":
		if t.cursor >= 0 && t.cursor < len(t.entries) {
			selected := t.entries[t.cursor]
			if selected.isLeaf {
				// Already at current position — no-op.
				break
			}
			return m.openBranchSummaryPrompt(selected.id)
		}

	case "ctrl+r":
		// Refresh tree from store.
		if m.Model.Store != nil {
			if m.activeSession() != nil {
				m.Picker.Tree.loading = true
				leafID := m.Model.Store.GetLeafID()
				return m, func() tea.Msg {
					tree, err := loadSessionTree(context.Background(), m.Model.Store, leafID)
					return treePickerLoadedMsg{tree: tree, err: err}
				}
			}
		}
	}

	return m, nil
}

// handleTreePickerMove processes a tree navigation result.
func (m Model) handleTreePickerMove(msg treePickerMoveMsg) (Model, tea.Cmd) {
	if msg.cancelled || errors.Is(msg.err, context.Canceled) {
		m.Picker.BranchSummary = nil
		m.terminalCommit().Entries(systemEntry("branch navigation cancelled"))
		return m, nil
	}
	if msg.err != nil {
		m.terminalCommit().Entries(systemEntry(fmt.Sprintf("⚠ tree navigation failed: %v", msg.err)))
		m.Picker.BranchSummary = nil
		m = m.closeTreePicker()
		return m, nil
	}
	// Close tree picker and replay entries from the new branch position.
	m = m.closeTreePicker()
	if m.activeSession() == nil {
		return m, nil
	}
	return m, m.replayCurrentBranch()
}

// replayCurrentBranch loads entries from the current session branch and replays them.
func (m Model) replayCurrentBranch() tea.Cmd {
	sess := m.activeSession()
	store := m.Model.Store
	if sess == nil {
		return nil
	}
	return func() tea.Msg {
		var entries []session.Entry
		if store != nil {
			entries, _ = store.Branch(context.Background())
		}
		return replayBranchMsg{entries: entries}
	}
}

type replayBranchMsg struct {
	entries []session.Entry
}

func (m Model) handleReplayBranch(msg replayBranchMsg) (Model, tea.Cmd) {
	var lines []string
	lines = append(lines, "--- moved to branch ---")
	if len(msg.entries) > 0 {
		lines = append(lines, "")
		lines = append(lines, m.RenderEntries(msg.entries...)...)
	}
	m.terminalCommit().MarkPrinted()
	return m, deferredTerminalCommitCmd(lines...)
}

// renderTreePicker renders the interactive tree selector.
func (m Model) renderTreePicker() string {
	t := m.Picker.Tree
	if t == nil {
		return ""
	}

	var b strings.Builder
	b.WriteString("\n")
	b.WriteString(m.cardTopBorder("Session Tree"))
	b.WriteString("\n")

	if t.err != "" {
		b.WriteString(m.cardPaddedLine(m.st.warn, "  Error: "+t.err))
		b.WriteString("\n")
		b.WriteString(m.cardBottomBorder())
		return b.String()
	}

	if t.loading || len(t.entries) == 0 {
		if t.loading {
			b.WriteString(m.cardPaddedLine(m.st.dim, "  Loading tree…"))
		} else {
			b.WriteString(m.cardPaddedLine(m.st.dim, "  (empty tree)"))
		}
		b.WriteString("\n")
		b.WriteString(m.cardBottomBorder())
		return b.String()
	}

	b.WriteString(m.cardDivider())
	b.WriteString("\n")

	// Determine visible window (scroll when needed).
	const maxVisible = 15
	start := 0
	if len(t.entries) > maxVisible {
		start = t.cursor - maxVisible/2
		if start < 0 {
			start = 0
		}
		if start > len(t.entries)-maxVisible {
			start = len(t.entries) - maxVisible
		}
	}
	end := min(start+maxVisible, len(t.entries))

	// Scroll indicator at top.
	if start > 0 {
		b.WriteString(m.cardPaddedLine(m.st.dim, fmt.Sprintf("  ↑ %d more above", start)))
		b.WriteString("\n")
	}

	for i := start; i < end; i++ {
		e := t.entries[i]
		prefix := "    "
		if i == t.cursor {
			prefix = "  › "
		} else {
			prefix = "    "
		}
		indent := strings.Repeat("  ", e.indent)

		line := prefix + indent + e.title

		// Mark current leaf position.
		if e.isLeaf {
			line += " " + m.st.cyan.Render("[current]")
		}

		// Show kind and summary on separate or appended lines.
		cursorStyle := m.st.cyan.Bold(true)
		if e.summary != "" {
			if i == t.cursor {
				line += " " + m.st.dim.Render(e.kind)
				b.WriteString(m.cardPaddedLine(cursorStyle, line))
				b.WriteString("\n")
				b.WriteString(m.cardPaddedLine(m.st.dim, "     "+indent+"  └ "+e.summary))
			} else {
				b.WriteString(m.cardPaddedLine(m.st.dim, line))
			}
		} else {
			if i == t.cursor {
				b.WriteString(m.cardPaddedLine(cursorStyle, line))
			} else {
				b.WriteString(m.cardPaddedLine(m.st.dim, line))
			}
		}
		b.WriteString("\n")
	}

	// Scroll indicator at bottom.
	if end < len(t.entries) {
		b.WriteString(m.cardPaddedLine(m.st.dim, fmt.Sprintf("  ↓ %d more below", len(t.entries)-end)))
		b.WriteString("\n")
	}

	b.WriteString(m.cardBottomBorder())
	return b.String()
}
