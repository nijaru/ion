package app

import (
	"context"
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"
	"github.com/nijaru/ion/internal/runtime"
	"github.com/nijaru/ion/session"
)

// treePickerState holds the interactive session tree navigator state.
type treePickerState struct {
	tree     *runtime.SessionTree
	cursor   int
	entries  []treePickerEntry
	loading  bool
	err      string
	rendered string
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
	tree runtime.SessionTree
	err  error
}

// treePickerMoveMsg confirms that a MoveTo navigation completed.
type treePickerMoveMsg struct {
	err error
}

func (m Model) openTreePicker() (Model, tea.Cmd) {
	if m.Model.Store == nil {
		m.showTreeUnavailable()
		return m, nil
	}
	reader, ok := m.Model.Store.(runtime.SessionTreeReader)
	if !ok {
		m.showTreeUnavailable()
		return m, nil
	}
	if m.Model.Session == nil {
		m.showTreeUnavailable()
		return m, nil
	}
	sessionID := m.Model.Session.ID()

	m.Picker.Tree = &treePickerState{loading: true}

	return m, func() tea.Msg {
		tree, err := reader.SessionTree(context.Background(), sessionID)
		return treePickerLoadedMsg{tree: tree, err: err}
	}
}

func (m Model) closeTreePicker() Model {
	m.Picker.Tree = nil
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
			return m.moveToTreeEntry(selected.id)
		}

	case "ctrl+r":
		// Refresh tree from store.
		if m.Model.Store != nil {
			reader, ok := m.Model.Store.(runtime.SessionTreeReader)
			if ok && m.Model.Session != nil {
				m.Picker.Tree.loading = true
				return m, func() tea.Msg {
					tree, err := reader.SessionTree(context.Background(), m.Model.Session.ID())
					return treePickerLoadedMsg{tree: tree, err: err}
				}
			}
		}
	}

	return m, nil
}

// moveToTreeEntry navigates the session to the selected tree entry.
func (m Model) moveToTreeEntry(entryID string) (Model, tea.Cmd) {
	if m.Model.Session == nil {
		return m, nil
	}

	sess := m.Model.Session

	return m, func() tea.Msg {
		// MoveTo with no branch summary.
		_, err := sess.MoveTo(context.Background(), entryID, nil)
		return treePickerMoveMsg{err: err}
	}
}

// handleTreePickerMove processes a tree navigation result.
func (m Model) handleTreePickerMove(msg treePickerMoveMsg) (Model, tea.Cmd) {
	if msg.err != nil {
		m.terminalCommit().Entries(systemEntry(fmt.Sprintf("⚠ tree navigation failed: %v", msg.err)))
		m.closeTreePicker()
		return m, nil
	}
	// Refresh tree after successful move.
	m.closeTreePicker()
	return m.openTreePicker()
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
