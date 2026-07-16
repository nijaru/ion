package app

import (
	"context"
	"fmt"
	"strings"
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
	ctx := context.Background()
	switch {
	case len(fields) == 1:
		return m.showMemory(ctx, "", false)
	case strings.EqualFold(fields[1], "all") && len(fields) == 2:
		return m.showMemory(ctx, "", true)
	case strings.EqualFold(fields[1], "search") && len(fields) >= 3:
		return m.showMemory(ctx, strings.TrimSpace(strings.Join(fields[2:], " ")), false)
	case strings.EqualFold(fields[1], "forget") && len(fields) == 3:
		if err := m.Model.Memory.Delete(ctx, strings.TrimSpace(fields[2])); err != nil {
			return m, cmdError(err.Error())
		}
		return m, m.terminalCommit().Help("Forgot workspace memory " + strings.TrimSpace(fields[2]) + ".")
	case strings.EqualFold(fields[1], "restore") && len(fields) == 3:
		if err := m.Model.Memory.Restore(ctx, strings.TrimSpace(fields[2])); err != nil {
			return m, cmdError(err.Error())
		}
		return m, m.terminalCommit().Help("Restored workspace memory " + strings.TrimSpace(fields[2]) + ".")
	default:
		return m, cmdError("usage: /memory [search <query>|forget <id>|restore <id>|all]")
	}
}

func (m Model) showMemory(ctx context.Context, query string, includeDeleted bool) (Model, tea.Cmd) {
	records, err := m.Model.Memory.Search(ctx, query, includeDeleted, memoryCommandLimit)
	if err != nil {
		return m, cmdError(err.Error())
	}
	return m, m.terminalCommit().Help(formatMemoryRecords(records, includeDeleted))
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
		fmt.Fprintf(&output, "%s  %-7s", record.ID, status)
		if strings.TrimSpace(record.Tags) != "" {
			fmt.Fprintf(&output, "  [%s]", strings.TrimSpace(record.Tags))
		}
		if !record.CreatedAt.IsZero() {
			fmt.Fprintf(&output, "  %s", record.CreatedAt.UTC().Format("2006-01-02 15:04:05Z"))
		}
		output.WriteByte('\n')
		content := strings.ReplaceAll(strings.TrimSpace(record.Content), "\n", "\n  ")
		output.WriteString("  ")
		output.WriteString(content)
		output.WriteByte('\n')
	}
	return limitMemoryDisplay(strings.TrimRight(output.String(), "\n"))
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
