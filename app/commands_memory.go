package app

import (
	"context"
	"fmt"
	"strings"
	"unicode"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"
)

const (
	memoryCommandLimit    = 20
	maxMemoryDisplayBytes = 50 * 1024
)

func (m Model) handleMemoryCommand(fields []string) (Model, tea.Cmd) {
	if m.Model.Memory == nil {
		return m, cmdError("workspace memory is unavailable")
	}
	switch {
	case len(fields) == 1:
		return m.showMemory("", false)
	case strings.EqualFold(fields[1], "all") && len(fields) == 2:
		return m.showMemory("", true)
	case strings.EqualFold(fields[1], "audit") && len(fields) == 2:
		requestID := (&m).beginMemoryRequest()
		return m, memoryAuditCmd(
			m.runtimeOperationContext(),
			m.Model.Memory,
			m.Model.EventGeneration,
			requestID,
		)
	case strings.EqualFold(fields[1], "search") && len(fields) >= 3:
		return m.showMemory(strings.TrimSpace(strings.Join(fields[2:], " ")), false)
	case strings.EqualFold(fields[1], "forget") && len(fields) == 3:
		requestID := (&m).beginMemoryRequest()
		return m, memoryActionCmd(
			m.runtimeOperationContext(),
			m.Model.Memory,
			m.Model.EventGeneration,
			requestID,
			"forgot",
			strings.TrimSpace(fields[2]),
		)
	case strings.EqualFold(fields[1], "restore") && len(fields) == 3:
		requestID := (&m).beginMemoryRequest()
		return m, memoryActionCmd(
			m.runtimeOperationContext(),
			m.Model.Memory,
			m.Model.EventGeneration,
			requestID,
			"restored",
			strings.TrimSpace(fields[2]),
		)
	default:
		return m, cmdError("usage: /memory [search <query>|audit|forget <id>|restore <id>|all]")
	}
}

func (m Model) showMemory(query string, includeDeleted bool) (Model, tea.Cmd) {
	requestID := (&m).beginMemoryRequest()
	return m, memorySearchCmd(
		m.runtimeOperationContext(),
		m.Model.Memory,
		m.Model.EventGeneration,
		requestID,
		query,
		includeDeleted,
	)
}

func (m *Model) beginMemoryRequest() uint64 {
	m.Model.MemoryRequest++
	return m.Model.MemoryRequest
}

func memorySearchCmd(
	ctx context.Context,
	controller MemoryController,
	generation, requestID uint64,
	query string,
	includeDeleted bool,
) tea.Cmd {
	if ctx == nil {
		ctx = context.Background()
	}
	return func() tea.Msg {
		records, err := controller.Search(ctx, query, includeDeleted, memoryCommandLimit)
		return memorySearchMsg{
			generation:     generation,
			requestID:      requestID,
			query:          query,
			includeDeleted: includeDeleted,
			records:        records,
			err:            err,
		}
	}
}

func memoryAuditCmd(
	ctx context.Context,
	controller MemoryController,
	generation, requestID uint64,
) tea.Cmd {
	if ctx == nil {
		ctx = context.Background()
	}
	return func() tea.Msg {
		entries, err := controller.Audit(ctx, memoryCommandLimit)
		return memoryAuditMsg{
			generation: generation,
			requestID:  requestID,
			entries:    entries,
			err:        err,
		}
	}
}

func memoryActionCmd(
	ctx context.Context,
	controller MemoryController,
	generation, requestID uint64,
	action, id string,
) tea.Cmd {
	if ctx == nil {
		ctx = context.Background()
	}
	return func() tea.Msg {
		var err error
		switch action {
		case "forgot":
			err = controller.Delete(ctx, id)
		case "restored":
			err = controller.Restore(ctx, id)
		default:
			err = fmt.Errorf("unknown memory action %q", action)
		}
		return memoryActionMsg{
			generation: generation,
			requestID:  requestID,
			action:     action,
			id:         id,
			err:        err,
		}
	}
}

func (m Model) handleMemorySearch(msg memorySearchMsg) (Model, tea.Cmd) {
	if msg.generation != m.Model.EventGeneration || msg.requestID != m.Model.MemoryRequest {
		return m, nil
	}
	if msg.err != nil {
		return m, cmdError(msg.err.Error())
	}
	return m, m.terminalCommit().Help(formatMemoryRecords(msg.records, msg.includeDeleted))
}

func (m Model) handleMemoryAudit(msg memoryAuditMsg) (Model, tea.Cmd) {
	if msg.generation != m.Model.EventGeneration || msg.requestID != m.Model.MemoryRequest {
		return m, nil
	}
	if msg.err != nil {
		return m, cmdError(msg.err.Error())
	}
	return m, m.terminalCommit().Help(formatMemoryAudit(msg.entries))
}

func (m Model) handleMemoryAction(msg memoryActionMsg) (Model, tea.Cmd) {
	if msg.generation != m.Model.EventGeneration || msg.requestID != m.Model.MemoryRequest {
		return m, nil
	}
	if msg.err != nil {
		return m, cmdError(msg.err.Error())
	}
	notice := "Forgot"
	if msg.action == "restored" {
		notice = "Restored"
	}
	return m, m.terminalCommit().Help(notice + " workspace memory " + msg.id + ".")
}

func formatMemoryRecords(records []MemoryRecord, includeDeleted bool) string {
	if len(records) == 0 {
		if includeDeleted {
			return "No workspace memory records."
		}
		return "No active workspace memory records."
	}
	var output strings.Builder
	for _, record := range records {
		status := "active"
		if record.Deleted {
			status = "deleted"
		}
		fmt.Fprintf(&output, "%s  %-7s", sanitizeMemoryInline(record.ID), status)
		if tags := sanitizeMemoryInline(record.Tags); tags != "" {
			fmt.Fprintf(&output, "  [%s]", tags)
		}
		if !record.CreatedAt.IsZero() {
			fmt.Fprintf(&output, "  %s", record.CreatedAt.UTC().Format("2006-01-02 15:04:05Z"))
		}
		output.WriteByte('\n')
		content := strings.ReplaceAll(strings.TrimSpace(sanitizeMemoryContent(record.Content)), "\n", "\n  ")
		output.WriteString("  ")
		output.WriteString(content)
		output.WriteByte('\n')
	}
	return limitMemoryDisplay(strings.TrimRight(output.String(), "\n"))
}

func formatMemoryAudit(entries []MemoryAuditRecord) string {
	if len(entries) == 0 {
		return "No workspace memory audit entries."
	}
	var output strings.Builder
	for _, entry := range entries {
		fmt.Fprintf(
			&output,
			"%d  %-7s %s",
			entry.Sequence,
			sanitizeMemoryInline(entry.Operation),
			sanitizeMemoryInline(entry.MemoryID),
		)
		if !entry.At.IsZero() {
			fmt.Fprintf(&output, "  %s", entry.At.UTC().Format("2006-01-02 15:04:05Z"))
		}
		output.WriteByte('\n')
		output.WriteString("  ")
		output.WriteString(strings.ReplaceAll(strings.TrimSpace(sanitizeMemoryContent(entry.Content)), "\n", "\n  "))
		if tags := sanitizeMemoryInline(entry.Tags); tags != "" {
			fmt.Fprintf(&output, "\n  tags: %s", tags)
		}
		output.WriteByte('\n')
	}
	return limitMemoryDisplay(strings.TrimRight(output.String(), "\n"))
}

func sanitizeMemoryInline(value string) string {
	return sanitizeMemoryText(value, false)
}

func sanitizeMemoryContent(value string) string {
	return sanitizeMemoryText(value, true)
}

func sanitizeMemoryText(value string, preserveNewlines bool) string {
	var output strings.Builder
	for _, r := range value {
		if r == '\n' && preserveNewlines {
			output.WriteRune(r)
			continue
		}
		if unicode.IsControl(r) || unicode.Is(unicode.Cf, r) {
			if r <= 0xffff {
				fmt.Fprintf(&output, "\\u%04x", r)
			} else {
				fmt.Fprintf(&output, "\\U%08x", r)
			}
			continue
		}
		output.WriteRune(r)
	}
	return output.String()
}

func limitMemoryDisplay(output string) string {
	if len(output) <= maxMemoryDisplayBytes {
		return output
	}
	cut := maxMemoryDisplayBytes
	for cut > 0 && !utf8.ValidString(output[:cut]) {
		cut--
	}
	return strings.TrimRight(output[:cut], "\n") +
		"\n\n[memory output truncated; use /memory search with a narrower query.]"
}
