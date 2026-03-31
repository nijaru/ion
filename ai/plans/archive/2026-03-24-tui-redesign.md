# TUI Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rewrite `internal/app/` into five focused files using Bubbletea v2 inline mode — completed messages commit to terminal scrollback via `tea.Printf`, `View()` returns only the dynamic bottom area.

**Architecture:** Remove the `viewport` widget entirely. Split the monolithic `model.go` into `model.go` (struct + dispatch), `events.go` (key + session event handling), `render.go` (View + render helpers), `commands.go` (slash commands), and `styles.go` (lipgloss palette). Streaming text and active tools live in Plane B (ephemeral, in `View()`). Completed entries are committed to scrollback via `tea.Printf`.

**Tech Stack:** Go 1.26, `charm.land/bubbletea/v2` v2.0.2, `charm.land/bubbles/v2` (textarea, spinner), `charm.land/lipgloss/v2`, `github.com/aymanbagabas/go-udiff`

**Spec:** `ai/design/tui-redesign-2026-03-24.md`

---

## File Map

| File                         | Action        | Responsibility                                                                            |
| ---------------------------- | ------------- | ----------------------------------------------------------------------------------------- |
| `internal/app/styles.go`     | Create        | `styles` struct, `newStyles()`, color palette                                             |
| `internal/app/model.go`      | Rewrite       | `Model` struct, `New()`, `Init()`, `Update()` dispatch, `awaitSessionEvent()`             |
| `internal/app/render.go`     | Create        | `View()`, `renderPlaneB()`, `renderEntry()`, `progressLine()`, `statusLine()`, `layout()` |
| `internal/app/events.go`     | Create        | `handleKey()`, `handleSessionEvent()`, history navigation                                 |
| `internal/app/commands.go`   | Create        | `handleCommand()`, `renderDiff()`                                                         |
| `internal/app/model_test.go` | Keep + extend | Existing tests pass; add render unit tests                                                |

No changes to `cmd/ion/main.go` — the `app.New(b, sess)` signature is preserved.

---

## Task 1: Create styles.go

**Files:**

- Create: `internal/app/styles.go`

- [ ] **Step 1: Create styles.go with the styles struct and constructor**

```go
package app

import "charm.land/lipgloss/v2"

type styles struct {
	user      lipgloss.Style
	assistant lipgloss.Style
	system    lipgloss.Style
	tool      lipgloss.Style
	agent     lipgloss.Style
	dim       lipgloss.Style
	cyan      lipgloss.Style
	warn      lipgloss.Style
	sep       lipgloss.Style
	added     lipgloss.Style
	removed   lipgloss.Style
	modeRead  lipgloss.Style
	modeWrite lipgloss.Style
}

func newStyles() styles {
	return styles{
		user:      lipgloss.NewStyle().Bold(true).PaddingLeft(2),
		assistant: lipgloss.NewStyle().Foreground(lipgloss.Color("6")).PaddingLeft(2),
		system:    lipgloss.NewStyle().Foreground(lipgloss.Color("8")).Faint(true).PaddingLeft(2),
		tool:      lipgloss.NewStyle().Foreground(lipgloss.Color("10")).PaddingLeft(2),
		agent:     lipgloss.NewStyle().Foreground(lipgloss.Color("13")).PaddingLeft(2),
		dim:       lipgloss.NewStyle().Faint(true),
		cyan:      lipgloss.NewStyle().Foreground(lipgloss.Color("6")),
		warn:      lipgloss.NewStyle().Foreground(lipgloss.Color("3")),
		sep:       lipgloss.NewStyle().Foreground(lipgloss.Color("8")),
		added:     lipgloss.NewStyle().Foreground(lipgloss.Color("2")),
		removed:   lipgloss.NewStyle().Foreground(lipgloss.Color("1")),
		modeRead:  lipgloss.NewStyle().Foreground(lipgloss.Color("6")).Bold(true),
		modeWrite: lipgloss.NewStyle().Foreground(lipgloss.Color("3")).Bold(true),
	}
}
```

- [ ] **Step 2: Verify it compiles**

```bash
cd /Users/nick/github/nijaru/ion && go build ./internal/app/...
```

Expected: no errors (other packages might still fail — that's fine at this stage).

- [ ] **Step 3: Commit**

```bash
git add internal/app/styles.go
git commit -m "feat(tui): add styles struct to app package"
```

---

## Task 2: Rewrite model.go

**Files:**

- Modify: `internal/app/model.go` (full rewrite)

The existing `model.go` is ~770 lines combining struct, event handling, rendering, and
commands. Replace it entirely with the struct definition, constructor, and dispatch loop.

Key struct changes from current:

- Remove: `viewport viewport.Model`, all `*Style` fields
- Add: `st styles`, `reasonBuf string`, `streamBuf string`, `progress progressState`,
  `lastError string`, `history []string`, `historyIdx int`, `escPending bool`,
  `ctrlCPending bool`
- Keep: `pending *session.Entry` (used for both streaming assistant and active tool/agent)

- [ ] **Step 1: Write the new model.go**

```go
package app

import (
	"context"
	"os"
	"os/exec"
	"strings"
	"time"

	"charm.land/bubbles/v2/spinner"
	"charm.land/bubbles/v2/textarea"
	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/backend"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

const (
	minComposerHeight = 1
	maxComposerHeight = 10
)

type streamClosedMsg struct{}

type toolMode int

const (
	modeRead toolMode = iota
	modeWrite
)

type progressState int

const (
	stateReady      progressState = iota
	stateIonizing
	stateStreaming
	stateWorking
	stateApproval
	stateCancelled
	stateError
)

// Model is the Bubble Tea model for the ion TUI.
// It owns all UI state; rendering is in render.go, event handling in events.go.
type Model struct {
	width  int
	height int
	ready  bool

	// Backend and session
	backend backend.Backend
	session session.AgentSession
	storage storage.Session

	// In-flight state — Plane B content
	pending    *session.Entry   // streaming assistant, active tool, or active agent
	reasonBuf  string           // accumulates ThinkingDelta
	streamBuf  string           // accumulates AssistantDelta (mirrors pending.Content)

	// Approval
	pendingApproval *session.ApprovalRequest

	// Progress and status
	progress  progressState
	lastError string
	thinking  bool

	// Token / cost tracking
	tokensSent     int
	tokensReceived int
	totalCost      float64

	// Composer
	composer textarea.Model
	spinner  spinner.Model

	// Input history
	history     []string
	historyIdx  int
	historyDraft string

	// Double-tap tracking
	escPending    bool
	ctrlCPending  bool

	// Storage correlation
	lastToolUseID string

	// Workspace metadata
	status  string
	workdir string
	branch  string
	mode    toolMode

	// Styles (initialized once in New)
	st styles
}

func New(b backend.Backend, s storage.Session) Model {
	ta := textarea.New()
	ta.Placeholder = "Type a message..."
	ta.Prompt = "› "
	ta.ShowLineNumbers = false
	ta.SetHeight(minComposerHeight)
	ta.SetWidth(80)
	ta.MaxHeight = maxComposerHeight

	cwd, _ := os.Getwd()

	spt := spinner.New()
	spt.Spinner = spinner.Dot
	spt.Style = newStyles().cyan

	boot := b.Bootstrap()

	m := Model{
		backend:    b,
		session:    b.Session(),
		storage:    s,
		composer:   ta,
		spinner:    spt,
		status:     boot.Status,
		workdir:    cwd,
		branch:     currentBranch(),
		historyIdx: -1,
		st:         newStyles(),
	}

	if s != nil {
		if input, output, cost, err := s.Usage(context.Background()); err == nil {
			m.tokensSent = input
			m.tokensReceived = output
			m.totalCost = cost
		}
	}

	return m
}

func (m Model) Init() tea.Cmd {
	return tea.Batch(
		textarea.Blink,
		m.spinner.Tick,
		m.composer.Focus(),
		m.awaitSessionEvent(),
	)
}

func (m Model) awaitSessionEvent() tea.Cmd {
	return func() tea.Msg {
		ev, ok := <-m.session.Events()
		if !ok {
			return streamClosedMsg{}
		}
		return ev
	}
}

func (m Model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case spinner.TickMsg:
		var cmd tea.Cmd
		m.spinner, cmd = m.spinner.Update(msg)
		return m, cmd

	case tea.WindowSizeMsg:
		m.ready = true
		m.width = msg.Width
		m.height = msg.Height
		m.layout()
		return m, nil

	case streamClosedMsg:
		return m, nil

	case tea.KeyPressMsg:
		return m.handleKey(msg)

	case session.StatusChanged,
		session.TokenUsage,
		session.TurnStarted,
		session.TurnFinished,
		session.ThinkingDelta,
		session.AssistantDelta,
		session.AssistantMessage,
		session.ToolCallStarted,
		session.ToolOutputDelta,
		session.ToolResult,
		session.VerificationResult,
		session.ApprovalRequest,
		session.ChildRequested,
		session.ChildStarted,
		session.ChildDelta,
		session.ChildCompleted,
		session.ChildFailed,
		session.Error:
		return m.handleSessionEvent(msg.(session.Event))
	}

	// Pass remaining messages to composer
	var cmd tea.Cmd
	m.composer, cmd = m.composer.Update(msg)
	if m.ready {
		m.layout()
	}
	return m, cmd
}

// currentBranch returns the current git branch name, or "unknown".
func currentBranch() string {
	out, err := exec.Command("git", "rev-parse", "--abbrev-ref", "HEAD").Output()
	if err != nil {
		return "unknown"
	}
	return strings.TrimSpace(string(out))
}

// ifthen is a generic ternary helper.
func ifthen[T any](cond bool, a, b T) T {
	if cond {
		return a
	}
	return b
}

// now returns the current unix timestamp.
func now() int64 { return time.Now().Unix() }
```

- [ ] **Step 2: Verify existing tests still compile**

```bash
cd /Users/nick/github/nijaru/ion && go test ./internal/app/... 2>&1 | head -30
```

Expected: compile errors about undefined functions (`handleKey`, `handleSessionEvent`,
`View`, `layout`, `renderEntry`) — that's expected. The struct and stubs are in place.

- [ ] **Step 3: Commit**

```bash
git add internal/app/model.go
git commit -m "feat(tui): rewrite model.go — new struct with inline-mode fields"
```

---

## Task 3: Create render.go

**Files:**

- Create: `internal/app/render.go`
- Modify: `internal/app/model_test.go` (add render tests at end of file)

This is the core of the inline-mode approach. `View()` returns only the bottom dynamic
area. `renderEntry()` formats messages for `tea.Printf` scrollback commits.

- [ ] **Step 1: Write the failing render tests** (add to end of `model_test.go`)

```go
func TestStatusLineShowsProviderAndModelSeparately(t *testing.T) {
	m := readyModel(t)
	m.tokensSent = 5000
	m.tokensReceived = 2000
	m.totalCost = 0.042

	line := m.statusLine()

	if !strings.Contains(line, "stub") {
		t.Errorf("status line missing provider: %q", line)
	}
	if !strings.Contains(line, "stub-model") {
		t.Errorf("status line missing model: %q", line)
	}
	if !strings.Contains(line, "$0.042") {
		t.Errorf("status line missing cost: %q", line)
	}
}

func TestProgressLineStates(t *testing.T) {
	m := readyModel(t)

	m.progress = stateReady
	if line := m.progressLine(); !strings.Contains(line, "Ready") {
		t.Errorf("expected Ready, got %q", line)
	}

	m.progress = stateIonizing
	m.thinking = true
	if line := m.progressLine(); !strings.Contains(line, "Ionizing") {
		t.Errorf("expected Ionizing, got %q", line)
	}

	m.progress = stateStreaming
	if line := m.progressLine(); !strings.Contains(line, "Streaming") {
		t.Errorf("expected Streaming, got %q", line)
	}

	m.progress = stateWorking
	if line := m.progressLine(); !strings.Contains(line, "Working") {
		t.Errorf("expected Working, got %q", line)
	}

	m.progress = stateError
	m.lastError = "connection refused"
	if line := m.progressLine(); !strings.Contains(line, "connection refused") {
		t.Errorf("expected error message, got %q", line)
	}
}

func TestRenderEntryFormats(t *testing.T) {
	m := readyModel(t)

	user := session.Entry{Role: session.User, Content: "hello world"}
	got := m.renderEntry(user)
	if !strings.Contains(got, "hello world") {
		t.Errorf("user entry missing content: %q", got)
	}

	asst := session.Entry{Role: session.Assistant, Content: "reply text"}
	got = m.renderEntry(asst)
	if !strings.Contains(got, "reply text") {
		t.Errorf("assistant entry missing content: %q", got)
	}

	tool := session.Entry{Role: session.Tool, Title: "bash(ls)", Content: "file.go"}
	got = m.renderEntry(tool)
	if !strings.Contains(got, "bash(ls)") {
		t.Errorf("tool entry missing title: %q", got)
	}
}
```

- [ ] **Step 2: Run tests to see them fail**

```bash
cd /Users/nick/github/nijaru/ion && go test ./internal/app/... -run "TestStatusLine|TestProgress|TestRenderEntry" 2>&1
```

Expected: compile error — `statusLine`, `progressLine`, `renderEntry` not defined yet.

- [ ] **Step 3: Create render.go**

```go
package app

import (
	"fmt"
	"path/filepath"
	"strings"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"

	"github.com/nijaru/ion/internal/session"
)

func (m Model) View() tea.View {
	if !m.ready {
		return tea.NewView("loading...")
	}

	var b strings.Builder

	// Blank line separates scrollback from dynamic area
	b.WriteString("\n")

	// Plane B — ephemeral in-flight content
	planeB := m.renderPlaneB()
	if planeB != "" {
		b.WriteString(planeB)
		b.WriteString("\n")
	}

	// Progress line
	b.WriteString(m.progressLine())
	b.WriteString("\n")

	// Top separator
	b.WriteString(m.st.sep.Render(strings.Repeat("─", max(0, m.width))))
	b.WriteString("\n")

	// Composer
	b.WriteString(lipgloss.NewStyle().PaddingLeft(1).Render(m.composer.View()))
	b.WriteString("\n")

	// Bottom separator
	b.WriteString(m.st.sep.Render(strings.Repeat("─", max(0, m.width))))
	b.WriteString("\n")

	// Status line
	b.WriteString(m.statusLine())

	return tea.NewView(b.String())
}

// renderPlaneB renders all ephemeral in-flight content for the dynamic area.
// Returns empty string when there is nothing active.
func (m Model) renderPlaneB() string {
	if m.pending == nil && m.pendingApproval == nil && m.reasonBuf == "" {
		return ""
	}

	var b strings.Builder

	// Thinking/reasoning (shown while generating, dimmed)
	if m.reasonBuf != "" {
		b.WriteString(m.st.dim.Render("  • Thinking..."))
		b.WriteString("\n")
		for _, line := range strings.Split(m.reasonBuf, "\n") {
			b.WriteString(m.st.dim.PaddingLeft(4).Render(line))
			b.WriteString("\n")
		}
	}

	// Active in-flight entry (streaming assistant, tool, or agent)
	if m.pending != nil {
		b.WriteString(m.renderPendingEntry(*m.pending))
	}

	// Approval prompt
	if m.pendingApproval != nil {
		b.WriteString("\n")
		desc := m.pendingApproval.Description
		if m.pendingApproval.ToolName != "" {
			desc = fmt.Sprintf("%s(%s): %s",
				m.pendingApproval.ToolName,
				m.pendingApproval.Args,
				m.pendingApproval.Description)
		}
		b.WriteString(m.st.warn.PaddingLeft(2).Render("Approve " + desc + "? (y/n)"))
		b.WriteString("\n")
	}

	return b.String()
}

// renderPendingEntry renders an in-flight (not yet committed) entry.
// Used in Plane B — similar to renderEntry but without the "committed" framing.
func (m Model) renderPendingEntry(e session.Entry) string {
	switch e.Role {
	case session.Assistant:
		if e.Content == "" {
			return m.st.dim.PaddingLeft(2).Render("• ...")
		}
		return m.st.assistant.Render("• " + e.Content)
	case session.Tool:
		label := e.Title
		if label == "" {
			label = "tool"
		}
		var b strings.Builder
		b.WriteString(m.st.tool.Render("• " + label))
		if e.Content != "" {
			b.WriteString("\n")
			lines := strings.Split(strings.TrimRight(e.Content, "\n"), "\n")
			shown := lines
			if len(lines) > 10 {
				shown = lines[:10]
			}
			for _, l := range shown {
				b.WriteString(m.st.dim.PaddingLeft(4).Render(l))
				b.WriteString("\n")
			}
			if len(lines) > 10 {
				b.WriteString(m.st.dim.PaddingLeft(4).Render(
					fmt.Sprintf("... (%d more lines)", len(lines)-10)))
				b.WriteString("\n")
			}
		}
		return b.String()
	case session.Agent:
		label := e.Title
		if label == "" {
			label = "agent"
		}
		var b strings.Builder
		b.WriteString(m.st.agent.Render("↳ " + label))
		if e.Content != "" {
			b.WriteString("\n")
			b.WriteString(m.st.dim.PaddingLeft(4).Render(e.Content))
		}
		return b.String()
	default:
		return e.Content
	}
}

// renderEntry formats a completed entry for tea.Printf scrollback commit.
func (m Model) renderEntry(e session.Entry) string {
	switch e.Role {
	case session.User:
		return m.st.user.Render("› " + e.Content)

	case session.Assistant:
		var b strings.Builder
		if e.Reasoning != "" {
			b.WriteString(m.st.system.Render("• Thinking"))
			b.WriteString("\n")
			b.WriteString(m.st.dim.PaddingLeft(4).Render(e.Reasoning))
			b.WriteString("\n")
		}
		b.WriteString(m.st.assistant.Render("• " + e.Content))
		return b.String()

	case session.Tool:
		label := e.Title
		if label == "" {
			label = "tool"
		}
		icon := "•"
		if e.IsError {
			icon = "✗"
			label = m.st.warn.Render(icon+" "+label)
		} else {
			label = m.st.tool.Render(icon + " " + label)
		}
		if e.Content == "" {
			return label
		}
		var b strings.Builder
		b.WriteString(label)
		b.WriteString("\n")
		lines := strings.Split(strings.TrimRight(e.Content, "\n"), "\n")
		shown := lines
		if len(lines) > 10 {
			shown = lines[:10]
		}
		for _, l := range shown {
			b.WriteString(m.st.dim.PaddingLeft(4).Render(l))
			b.WriteString("\n")
		}
		if len(lines) > 10 {
			b.WriteString(m.st.dim.PaddingLeft(4).Render(
				fmt.Sprintf("... (%d more lines)", len(lines)-10)))
		}
		return b.String()

	case session.Agent:
		label := e.Title
		if label == "" {
			label = "agent"
		}
		var b strings.Builder
		b.WriteString(m.st.agent.Render("↳ " + label))
		b.WriteString("\n")
		b.WriteString(m.st.dim.PaddingLeft(4).Render(e.Content))
		return b.String()

	case session.System:
		return m.st.system.Render("  " + e.Content)

	default:
		return e.Content
	}
}

// progressLine renders the single-line progress indicator.
func (m Model) progressLine() string {
	switch m.progress {
	case stateIonizing:
		return m.st.cyan.Render("  " + m.spinner.View() + " Ionizing...")
	case stateStreaming:
		return m.st.cyan.Render("  " + m.spinner.View() + " Streaming...")
	case stateWorking:
		return m.st.cyan.Render("  " + m.spinner.View() + " Working...")
	case stateApproval:
		return m.st.warn.Render("  ⚠ Approval required")
	case stateCancelled:
		return m.st.dim.Render("  · Cancelled")
	case stateError:
		return m.st.warn.Render("  ✗ Error: " + m.lastError)
	default:
		return m.st.dim.Render("  · Ready")
	}
}

// statusLine renders the bottom info bar.
func (m Model) statusLine() string {
	sep := m.st.dim.Render(" · ")

	// Mode indicator — left-aligned, always first
	modeLabel := ifthen(m.mode == modeWrite,
		m.st.modeWrite.Render("[WRITE]"),
		m.st.modeRead.Render("[READ]"),
	)

	// Provider and model — shown separately
	provider := m.backend.Provider()
	model := m.backend.Model()

	var segments []string
	segments = append(segments, modeLabel)
	if provider != "" {
		segments = append(segments, m.st.dim.Render(provider))
	}
	if model != "" {
		segments = append(segments, m.st.dim.Render(model))
	}

	// Token usage
	total := m.tokensSent + m.tokensReceived
	limit := m.backend.ContextLimit()
	usage := ""
	if limit > 0 {
		pct := (total * 100) / limit
		usage = fmt.Sprintf("%dk/%dk (%d%%)", total/1000, limit/1000, pct)
	} else if total > 0 {
		usage = fmt.Sprintf("%dk tokens", total/1000)
	} else {
		usage = "0 tokens"
	}
	segments = append(segments, usage)

	// Cost
	if m.totalCost > 0 {
		segments = append(segments, fmt.Sprintf("$%.3f", m.totalCost))
	}

	// Dir and branch — hide on narrow terminals
	if m.width > 80 {
		segments = append(segments, "./"+filepath.Base(m.workdir))
	}
	if m.width > 60 && m.branch != "" {
		segments = append(segments, m.st.cyan.Render(m.branch))
	}

	return "  " + strings.Join(segments, sep)
}

// layout recomputes widget dimensions based on current terminal size.
func (m *Model) layout() {
	m.composer.SetWidth(max(20, m.width-4))
	m.composer.SetHeight(clamp(m.composer.LineCount(), minComposerHeight, maxComposerHeight))
}

func clamp(v, low, high int) int {
	if v < low {
		return low
	}
	if v > high {
		return high
	}
	return v
}

// Note: Go 1.21+ provides a builtin max() — do NOT define a custom max function.
// Use the builtin directly: max(a, b)
```

Note: `session.Entry` needs `IsError bool` and `Reasoning string` fields. Check
`internal/session/` — if these fields don't exist on `Entry`, add them there.

- [ ] **Step 4: Run the render tests**

```bash
cd /Users/nick/github/nijaru/ion && go test ./internal/app/... -run "TestStatusLine|TestProgress|TestRenderEntry" -v
```

Expected: all three tests pass.

- [ ] **Step 5: Run all current tests**

```bash
cd /Users/nick/github/nijaru/ion && go test ./internal/app/... -v 2>&1
```

Expected: compile errors about `handleKey`, `handleSessionEvent` still — those are
in the next tasks. The three new tests and the layout test should pass.

- [ ] **Step 6: Commit**

```bash
git add internal/app/render.go internal/app/model_test.go
git commit -m "feat(tui): add render.go with inline-mode View and render helpers"
```

---

## Task 4: Create events.go

**Files:**

- Create: `internal/app/events.go`

This implements all key handling and session event handling. The key insight: `handleKey`
intercepts specific keys before passing to the textarea; `handleSessionEvent` drives
the commit-to-scrollback flow.

- [ ] **Step 1: Write failing tests** (add to end of `model_test.go`)

```go
func TestEscDoubleTapClearsInput(t *testing.T) {
	m := readyModel(t)
	m.composer.SetValue("some text")

	// First Esc — sets escPending
	updated, _ := m.Update(tea.KeyPressMsg{Code: tea.KeyEscape})
	m = updated.(Model)
	if !m.escPending {
		t.Fatal("expected escPending after first Esc")
	}

	// Second Esc — clears input
	updated, _ = m.Update(tea.KeyPressMsg{Code: tea.KeyEscape})
	m = updated.(Model)
	if m.composer.Value() != "" {
		t.Errorf("expected composer cleared after double Esc, got %q", m.composer.Value())
	}
	if m.escPending {
		t.Error("escPending should be false after action")
	}
}

func TestHistoryNavigation(t *testing.T) {
	m := readyModel(t)
	m.history = []string{"first message", "second message"}
	m.historyIdx = -1

	// Up — should load last history entry
	updated, _ := m.Update(tea.KeyPressMsg{Code: tea.KeyUp})
	m = updated.(Model)
	if m.composer.Value() != "second message" {
		t.Errorf("expected second message on first Up, got %q", m.composer.Value())
	}

	// Up again — should load previous
	updated, _ = m.Update(tea.KeyPressMsg{Code: tea.KeyUp})
	m = updated.(Model)
	if m.composer.Value() != "first message" {
		t.Errorf("expected first message on second Up, got %q", m.composer.Value())
	}
}

func TestSessionEventProgressTransitions(t *testing.T) {
	m := readyModel(t)

	updated, _ := m.Update(session.TurnStarted{})
	m = updated.(Model)
	if m.progress != stateIonizing {
		t.Errorf("expected stateIonizing after TurnStarted, got %d", m.progress)
	}

	updated, _ = m.Update(session.AssistantDelta{Delta: "hi"})
	m = updated.(Model)
	if m.progress != stateStreaming {
		t.Errorf("expected stateStreaming after AssistantDelta, got %d", m.progress)
	}

	updated, _ = m.Update(session.ToolCallStarted{ToolName: "bash", Args: "ls"})
	m = updated.(Model)
	if m.progress != stateWorking {
		t.Errorf("expected stateWorking after ToolCallStarted, got %d", m.progress)
	}
}
```

- [ ] **Step 2: Run tests to see compile failure**

```bash
cd /Users/nick/github/nijaru/ion && go test ./internal/app/... -run "TestEsc|TestHistory|TestSession" 2>&1 | head -20
```

Expected: undefined `handleKey` / `handleSessionEvent`.

- [ ] **Step 3: Create events.go**

```go
package app

import (
	"context"
	"fmt"
	"strings"
	"time"

	"charm.land/bubbles/v2/textarea"
	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

// handleKey processes keyboard input from the composer.
func (m Model) handleKey(msg tea.KeyPressMsg) (Model, tea.Cmd) {
	var cmds []tea.Cmd

	// Approval gate takes priority — y/n are consumed here
	if m.pendingApproval != nil {
		switch msg.String() {
		case "y", "n":
			approved := msg.String() == "y"
			reqID := m.pendingApproval.RequestID
			desc := m.pendingApproval.Description
			m.pendingApproval = nil
			m.progress = stateReady

			label := ifthen(approved, "Approved", "Denied")
			notice := session.Entry{Role: session.System, Content: label + ": " + desc}
			m.session.Approve(context.Background(), reqID, approved)
			return m, tea.Printf("%s\n", m.renderEntry(notice))
		}
	}

	switch msg.String() {
	case "ctrl+c":
		if m.ctrlCPending || m.composer.Value() == "" {
			return m, tea.Quit
		}
		m.ctrlCPending = true
		m.composer.Reset()
		m.escPending = false
		return m, nil

	case "esc":
		m.ctrlCPending = false
		if m.thinking {
			// Cancel in-flight turn
			m.session.CancelTurn(context.Background())
			m.thinking = false
			m.progress = stateCancelled
			m.pending = nil
			m.streamBuf = ""
			m.reasonBuf = ""
			return m, nil
		}
		if m.escPending {
			m.composer.Reset()
			m.escPending = false
			return m, nil
		}
		m.escPending = true
		return m, nil

	case "shift+tab":
		m.ctrlCPending = false
		m.escPending = false
		if m.mode == modeWrite {
			m.mode = modeRead
		} else {
			m.mode = modeWrite
		}
		return m, nil

	case "enter":
		m.ctrlCPending = false
		m.escPending = false
		text := strings.TrimSpace(m.composer.Value())
		if text == "" || m.thinking {
			return m, nil
		}

		// Save to history
		m.history = append(m.history, text)
		m.historyIdx = -1
		m.historyDraft = ""

		userEntry := session.Entry{Role: session.User, Content: text}
		m.composer.Reset()
		m.layout()

		if m.storage != nil {
			m.storage.Append(context.Background(), storage.User{
				Type:    "user",
				Content: text,
				TS:      now(),
			})
		}

		if strings.HasPrefix(text, "/") {
			cmd := m.handleCommand(text)
			return m, tea.Batch(tea.Printf("%s\n", m.renderEntry(userEntry)), cmd)
		}

		m.progress = stateIonizing
		m.thinking = true
		m.session.SubmitTurn(context.Background(), text)
		return m, tea.Printf("%s\n", m.renderEntry(userEntry))

	case "shift+enter":
		m.ctrlCPending = false
		m.escPending = false
		// Let textarea handle it — inserts newline
		var cmd tea.Cmd
		m.composer, cmd = m.composer.Update(msg)
		m.layout()
		return m, cmd

	case "up":
		m.ctrlCPending = false
		m.escPending = false
		// Navigate history when composer cursor is on first line
		if m.composer.Line() == 0 && len(m.history) > 0 {
			if m.historyIdx == -1 {
				m.historyDraft = m.composer.Value()
				m.historyIdx = len(m.history) - 1
			} else if m.historyIdx > 0 {
				m.historyIdx--
			}
			m.composer.SetValue(m.history[m.historyIdx])
			return m, nil
		}
		// Otherwise let textarea handle cursor movement
		var cmd tea.Cmd
		m.composer, cmd = m.composer.Update(msg)
		return m, cmd

	case "down":
		m.ctrlCPending = false
		m.escPending = false
		// Navigate history when composer cursor is on last line
		if m.historyIdx != -1 {
			if m.historyIdx < len(m.history)-1 {
				m.historyIdx++
				m.composer.SetValue(m.history[m.historyIdx])
			} else {
				m.historyIdx = -1
				m.composer.SetValue(m.historyDraft)
				m.historyDraft = ""
			}
			return m, nil
		}
		var cmd tea.Cmd
		m.composer, cmd = m.composer.Update(msg)
		return m, cmd

	default:
		// Reset double-tap tracking on any other key
		m.ctrlCPending = false
		m.escPending = false
	}

	// Pass all other keys to textarea (handles Ctrl+A/E/W/U/K, Alt+B/F, etc.)
	var cmd tea.Cmd
	m.composer, cmd = m.composer.Update(msg)
	cmds = append(cmds, cmd)
	if m.ready {
		m.layout()
	}
	return m, tea.Batch(cmds...)
}

// handleSessionEvent processes events from the agent session channel.
// Each event either updates Plane B state or commits an entry to scrollback.
func (m Model) handleSessionEvent(ev session.Event) (Model, tea.Cmd) {
	switch msg := ev.(type) {
	case session.StatusChanged:
		m.status = msg.Status
		if m.storage != nil {
			m.storage.Append(context.Background(), storage.Status{
				Type:   "status",
				Status: msg.Status,
				TS:     now(),
			})
		}
		return m, m.awaitSessionEvent()

	case session.TokenUsage:
		m.tokensSent += msg.Input
		m.tokensReceived += msg.Output
		m.totalCost += msg.Cost
		if m.storage != nil {
			m.storage.Append(context.Background(), storage.TokenUsage{
				Type:   "token_usage",
				Input:  msg.Input,
				Output: msg.Output,
				Cost:   msg.Cost,
				TS:     now(),
			})
		}
		return m, m.awaitSessionEvent()

	case session.TurnStarted:
		m.thinking = true
		m.progress = stateIonizing
		m.pending = &session.Entry{Role: session.Assistant}
		return m, m.awaitSessionEvent()

	case session.TurnFinished:
		m.thinking = false
		m.progress = stateReady
		return m, m.awaitSessionEvent()

	case session.ThinkingDelta:
		m.reasonBuf += msg.Delta
		return m, m.awaitSessionEvent()

	case session.AssistantDelta:
		m.progress = stateStreaming
		if m.pending == nil {
			m.pending = &session.Entry{Role: session.Assistant}
		}
		m.pending.Content += msg.Delta
		m.streamBuf = m.pending.Content
		return m, m.awaitSessionEvent()

	case session.AssistantMessage:
		if m.pending != nil && m.pending.Role == session.Assistant {
			if msg.Message != "" {
				m.pending.Content = msg.Message
			}
			m.pending.Reasoning = m.reasonBuf
			entry := *m.pending

			m.pending = nil
			m.streamBuf = ""
			m.reasonBuf = ""

			if m.storage != nil {
				blocks := []storage.Block{}
				if entry.Reasoning != "" {
					blocks = append(blocks, storage.Block{
						Type:     "thinking",
						Thinking: &entry.Reasoning,
					})
				}
				blocks = append(blocks, storage.Block{
					Type: "text",
					Text: &entry.Content,
				})
				m.storage.Append(context.Background(), storage.Assistant{
					Type:    "assistant",
					Content: blocks,
					TS:      now(),
				})
			}
			return m, tea.Batch(
				tea.Printf("%s\n", m.renderEntry(entry)),
				m.awaitSessionEvent(),
			)
		}
		return m, m.awaitSessionEvent()

	case session.ToolCallStarted:
		m.progress = stateWorking
		m.lastToolUseID = session.ShortID()
		if m.storage != nil {
			m.storage.Append(context.Background(), storage.ToolUse{
				Type: "tool_use",
				ID:   m.lastToolUseID,
				Name: msg.ToolName,
				Input: map[string]string{
					"args": msg.Args,
				},
				TS: now(),
			})
		}
		m.pending = &session.Entry{
			Role:  session.Tool,
			Title: fmt.Sprintf("%s(%s)", msg.ToolName, msg.Args),
		}
		return m, m.awaitSessionEvent()

	case session.ToolOutputDelta:
		if m.pending != nil && m.pending.Role == session.Tool {
			m.pending.Content += msg.Delta
		}
		return m, m.awaitSessionEvent()

	case session.ToolResult:
		if m.pending != nil && m.pending.Role == session.Tool {
			m.pending.Content = msg.Result
			m.pending.IsError = msg.Error != nil
			entry := *m.pending
			m.pending = nil

			if m.storage != nil {
				m.storage.Append(context.Background(), storage.ToolResult{
					Type:      "tool_result",
					ToolUseID: m.lastToolUseID,
					Content:   msg.Result,
					IsError:   msg.Error != nil,
					TS:        now(),
				})
			}
			return m, tea.Batch(
				tea.Printf("%s\n", m.renderEntry(entry)),
				m.awaitSessionEvent(),
			)
		}
		return m, m.awaitSessionEvent()

	case session.VerificationResult:
		status := ifthen(msg.Passed, "PASSED", "FAILED")
		content := fmt.Sprintf("%s: %s\n%s", status, msg.Metric, msg.Output)
		entry := session.Entry{
			Role:    session.Tool,
			Title:   "verify: " + msg.Command,
			Content: content,
			IsError: !msg.Passed,
		}
		return m, tea.Batch(
			tea.Printf("%s\n", m.renderEntry(entry)),
			m.awaitSessionEvent(),
		)

	case session.ApprovalRequest:
		m.pendingApproval = &msg
		m.progress = stateApproval
		m.thinking = false
		return m, m.awaitSessionEvent()

	case session.ChildRequested:
		m.pending = &session.Entry{
			Role:    session.Agent,
			Title:   msg.AgentName,
			Content: msg.Query,
		}
		return m, m.awaitSessionEvent()

	case session.ChildStarted:
		if m.pending != nil && m.pending.Role == session.Agent {
			m.pending.Title = msg.AgentName
		}
		return m, m.awaitSessionEvent()

	case session.ChildDelta:
		if m.pending != nil && m.pending.Role == session.Agent {
			m.pending.Content += msg.Delta
		}
		return m, m.awaitSessionEvent()

	case session.ChildCompleted:
		if m.pending != nil && m.pending.Role == session.Agent {
			m.pending.Content = msg.Result
			entry := *m.pending
			m.pending = nil
			return m, tea.Batch(
				tea.Printf("%s\n", m.renderEntry(entry)),
				m.awaitSessionEvent(),
			)
		}
		return m, m.awaitSessionEvent()

	case session.ChildFailed:
		if m.pending != nil && m.pending.Role == session.Agent {
			m.pending.Content = "ERROR: " + msg.Error
			m.pending.IsError = true
			entry := *m.pending
			m.pending = nil
			return m, tea.Batch(
				tea.Printf("%s\n", m.renderEntry(entry)),
				m.awaitSessionEvent(),
			)
		}
		return m, m.awaitSessionEvent()

	case session.Error:
		// Clear all Plane B content on error
		m.pending = nil
		m.pendingApproval = nil
		m.streamBuf = ""
		m.reasonBuf = ""
		m.thinking = false
		m.progress = stateError
		m.lastError = msg.Err.Error()
		return m, m.awaitSessionEvent()
	}

	return m, m.awaitSessionEvent()
}
```

- [ ] **Step 4: Run the event tests**

```bash
cd /Users/nick/github/nijaru/ion && go test ./internal/app/... -run "TestEsc|TestHistory|TestSession" -v
```

Expected: all three new tests pass.

- [ ] **Step 5: Run all tests**

```bash
cd /Users/nick/github/nijaru/ion && go test ./internal/app/... -v
```

Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add internal/app/events.go internal/app/model_test.go
git commit -m "feat(tui): add events.go — key handling and session event processing"
```

---

## Task 5: Create commands.go

**Files:**

- Create: `internal/app/commands.go`
- Modify: `internal/app/model_test.go` (add command test)

- [ ] **Step 1: Write a failing test** (add to end of `model_test.go`)

```go
func TestHandleCommandModel(t *testing.T) {
	m := readyModel(t)
	cmd := m.handleCommand("/model claude-opus-4")
	if cmd == nil {
		t.Fatal("expected a command from /model")
	}
}
```

- [ ] **Step 2: Run it to see it fail**

```bash
cd /Users/nick/github/nijaru/ion && go test ./internal/app/... -run TestHandleCommandModel 2>&1 | head -10
```

Expected: `undefined: handleCommand`.

- [ ] **Step 3: Create commands.go**

```go
package app

import (
	"fmt"
	"strings"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/session"
)

// handleCommand dispatches a slash command entered by the user.
// The input string includes the leading slash (e.g. "/model claude-opus-4").
func (m *Model) handleCommand(input string) tea.Cmd {
	fields := strings.Fields(input)
	if len(fields) == 0 {
		return nil
	}

	switch fields[0] {
	case "/model":
		if len(fields) < 2 {
			return cmdError("usage: /model <model_name>")
		}
		name := strings.Join(fields[1:], " ")
		cfg, _ := config.Load()
		cfg.Model = name
		config.Save(cfg)
		m.backend.SetConfig(cfg)
		notice := session.Entry{Role: session.System, Content: "Switched model to " + name}
		return tea.Printf("%s\n", m.renderEntry(notice))

	case "/provider":
		if len(fields) < 2 {
			return cmdError("usage: /provider <provider_name>")
		}
		name := fields[1]
		cfg, _ := config.Load()
		cfg.Provider = name
		config.Save(cfg)
		m.backend.SetConfig(cfg)
		notice := session.Entry{Role: session.System, Content: "Switched provider to " + name}
		return tea.Printf("%s\n", m.renderEntry(notice))

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

// cmdError returns a Cmd that emits a session.Error with the given message.
func cmdError(msg string) tea.Cmd {
	return func() tea.Msg {
		return session.Error{Err: fmt.Errorf("%s", msg)}
	}
}

// renderDiff colorizes diff-format output (lines starting with + or -).
// Used when tool results for write/edit tools contain unified diff text.
// Falls back to plain output if the content doesn't look like a diff.
func (m Model) renderDiff(content string) string {
	lines := strings.Split(content, "\n")
	hasDiffMarkers := false
	for _, l := range lines {
		if strings.HasPrefix(l, "--- ") || strings.HasPrefix(l, "+++ ") || strings.HasPrefix(l, "@@ ") {
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
```

Note: `context` import is needed in `commands.go`.

- [ ] **Step 4: Run the command test**

```bash
cd /Users/nick/github/nijaru/ion && go test ./internal/app/... -run TestHandleCommandModel -v
```

Expected: PASS.

- [ ] **Step 5: Run all tests**

```bash
cd /Users/nick/github/nijaru/ion && go test ./internal/app/... -v
```

Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add internal/app/commands.go internal/app/model_test.go
git commit -m "feat(tui): add commands.go — slash command dispatch and diff colorizer"
```

---

## Task 6: Fix session.Entry fields and wire diff rendering

**Files:**

- Check/modify: `internal/session/` (Entry type)
- Modify: `internal/app/render.go` (use renderDiff for write/edit tools)
- Modify: `internal/app/events.go` (pass tool name to renderEntry for diff detection)

The `session.Entry` type is used throughout — it needs `IsError bool` and `Reasoning string`
fields. Check if they exist:

- [ ] **Step 1: Check session.Entry**

```bash
cd /Users/nick/github/nijaru/ion && grep -n "type Entry" internal/session/*.go
```

If `Entry` doesn't have `IsError bool` or `Reasoning string`, add them:

```go
// In whichever file defines Entry:
type Entry struct {
    Role      Role
    Title     string
    Content   string
    Reasoning string // extended thinking/reasoning text
    IsError   bool   // true if this entry represents an error result
}
```

- [ ] **Step 2: Wire renderDiff into renderEntry for write/edit tools**

In `render.go`, update the `session.Tool` case of `renderEntry` to detect and colorize diffs:

The tool entry's `Title` contains `toolName(args)`. Detect write/edit tool names and
apply `renderDiff` to the content:

```go
case session.Tool:
    // ... (existing label setup) ...

    // For write/edit tools, colorize diff-format output
    content := e.Content
    if isWriteTool(e.Title) && content != "" {
        content = m.renderDiff(content)
    }
    // ... (rest of rendering using content instead of e.Content) ...
```

Add helper:

```go
// isWriteTool returns true if the tool title looks like a write/edit operation.
func isWriteTool(title string) bool {
	for _, prefix := range []string{"write(", "edit(", "create(", "Write(", "Edit("} {
		if strings.HasPrefix(title, prefix) {
			return true
		}
	}
	return false
}
```

- [ ] **Step 3: Build and run tests**

```bash
cd /Users/nick/github/nijaru/ion && go test ./internal/app/... -v
```

Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add internal/session/ internal/app/render.go
git commit -m "feat(tui): wire diff colorizer for write/edit tool results"
```

---

## Task 7: Integration — build, run, final cleanup

**Files:**

- Modify: `internal/app/model.go` (remove unused imports, dead helpers)
- Check: `cmd/ion/main.go` (should need no changes)

- [ ] **Step 1: Build the full binary**

```bash
cd /Users/nick/github/nijaru/ion && go build ./cmd/ion/
```

Expected: no errors. Fix any remaining compile issues (missing imports, unused variables).

- [ ] **Step 2: Run full test suite**

```bash
cd /Users/nick/github/nijaru/ion && go test ./... 2>&1
```

Expected: all tests pass. If any other package tests broke, fix them.

- [ ] **Step 3: Smoke test — run the binary**

```bash
cd /Users/nick/github/nijaru/ion && ./ion --help 2>&1 || go run ./cmd/ion/ --help
```

Verify the binary starts and prints usage without panic.

- [ ] **Step 4: Remove dead code from model.go**

- Remove any imports no longer used (e.g. `"fmt"`, `"time"` if moved to events.go)
- Remove the old `appendHistory`, `renderEntry`, `handleCommand`, `layout`, `View`,
  `progressLine`, `statusLine` functions if any remain (they should have moved)
- Remove the `max`, `clamp`, `ifthen` helpers if they were duplicated between files
  (keep them in one place — `render.go` is fine)

```bash
cd /Users/nick/github/nijaru/ion && go vet ./internal/app/...
```

Expected: no issues.

- [ ] **Step 5: Run tests once more after cleanup**

```bash
cd /Users/nick/github/nijaru/ion && go test ./... -v 2>&1 | tail -20
```

Expected: all pass.

- [ ] **Step 6: Final commit**

```bash
git add -u
git commit -m "feat(tui): complete TUI redesign — inline mode, streaming Plane B, readline"
```

---

## Reference: Key Types

**`session.Entry` fields used in this plan:**

- `Role session.Role` — `session.User`, `session.Assistant`, `session.Tool`, `session.Agent`, `session.System`
- `Title string` — tool name + args: `"bash(ls)"`
- `Content string` — text body
- `Reasoning string` — extended thinking text (add if missing)
- `IsError bool` — true for error results (add if missing)

**Session events and their key fields** (from `internal/session/event.go`):

- `AssistantDelta{Delta string}`
- `AssistantMessage{Message string}`
- `ToolCallStarted{ToolName, Args string}`
- `ToolOutputDelta{Delta string}`
- `ToolResult{ToolName, Result string, Error error}`
- `VerificationResult{Command, Metric, Output string, Passed bool}`
- `ApprovalRequest{RequestID, Description, ToolName, Args string}`
- `ChildRequested{AgentName, Query string}`
- `ChildStarted{AgentName, SessionID string}`
- `ChildDelta{AgentName, Delta string}`
- `ChildCompleted{AgentName, Result string}`
- `ChildFailed{AgentName, Error string}`
- `TokenUsage{Input, Output int, Cost float64}`
- `Error{Err error, Fatal bool}`

**`tea.Printf` for scrollback commits** — `tea.Printf("%s\n", content)` emits `content`
to the terminal above the TUI area, permanently. Not available in alt-screen mode (but
we are not using alt-screen).

**Textarea readline keybindings** — `charm.land/bubbles/v2/textarea` handles
`Ctrl+A/E/W/U/K` and word movement natively. Custom intercept needed only for
`Enter` (send vs newline), `Esc`, `Ctrl+C`, `Up/Down` (history), `Shift+Tab` (mode).
