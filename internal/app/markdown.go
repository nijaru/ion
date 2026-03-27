package app

import (
	"fmt"
	"strings"

	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"
	"github.com/yuin/goldmark"
	"github.com/yuin/goldmark/ast"
	"github.com/yuin/goldmark/extension"
	tableast "github.com/yuin/goldmark/extension/ast"
	"github.com/yuin/goldmark/parser"
	"github.com/yuin/goldmark/text"
)

type markdownTableRow struct {
	cells []string
}

func (m Model) renderMarkdownContent(content string) string {
	if strings.TrimSpace(content) == "" {
		return ""
	}

	md := goldmark.New(
		goldmark.WithExtensions(extension.GFM),
		goldmark.WithParserOptions(parser.WithAutoHeadingID()),
	)
	doc := md.Parser().Parse(text.NewReader([]byte(content)))
	lines := m.renderMarkdownNode(doc, []byte(content), 0)
	lines = normalizeMarkdownLines(lines)
	return strings.Join(lines, "\n")
}

func (m Model) renderMarkdownNode(node ast.Node, source []byte, depth int) []string {
	var lines []string
	for child := node.FirstChild(); child != nil; child = child.NextSibling() {
		switch n := child.(type) {
		case *ast.Heading:
			text := strings.TrimSpace(m.renderMarkdownInline(n, source))
			if text == "" {
				continue
			}
			if len(lines) > 0 && lines[len(lines)-1] != "" {
				lines = append(lines, "")
			}
			lines = append(lines, m.st.cyan.Bold(true).Render(text))
			lines = append(lines, "")
		case *ast.Paragraph:
			text := strings.TrimSpace(m.renderMarkdownInline(n, source))
			if text != "" {
				lines = append(lines, strings.Repeat("  ", depth)+text)
			}
			if depth == 0 {
				lines = append(lines, "")
			}
		case *ast.TextBlock:
			text := strings.TrimSpace(m.renderMarkdownInline(n, source))
			if text != "" {
				lines = append(lines, strings.Repeat("  ", depth)+text)
			}
			if depth == 0 {
				lines = append(lines, "")
			}
		case *ast.List:
			lines = append(lines, m.renderMarkdownList(n, source, depth)...)
			if depth == 0 {
				lines = append(lines, "")
			}
		case *ast.Blockquote:
			quoted := normalizeMarkdownLines(m.renderMarkdownNode(n, source, depth+1))
			for _, line := range quoted {
				if line == "" {
					lines = append(lines, "")
					continue
				}
				lines = append(lines, strings.Repeat("  ", depth)+m.st.dim.Render("> ")+line)
			}
			lines = append(lines, "")
		case *ast.FencedCodeBlock:
			lines = append(lines, m.renderMarkdownCodeBlock(n.Text(source), depth)...)
			lines = append(lines, "")
		case *ast.CodeBlock:
			lines = append(lines, m.renderMarkdownCodeBlock(n.Text(source), depth)...)
			lines = append(lines, "")
		case *tableast.Table:
			lines = append(lines, m.renderMarkdownTable(n, source, depth)...)
			lines = append(lines, "")
		case *ast.ThematicBreak:
			lines = append(lines, strings.Repeat("─", max(8, m.width-2)))
			lines = append(lines, "")
		default:
			if child.HasChildren() {
				lines = append(lines, m.renderMarkdownNode(child, source, depth)...)
			}
		}
	}
	return lines
}

func (m Model) renderMarkdownInline(node ast.Node, source []byte) string {
	var b strings.Builder
	for child := node.FirstChild(); child != nil; child = child.NextSibling() {
		switch n := child.(type) {
		case *ast.Text:
			b.Write(n.Value(source))
			if n.HardLineBreak() {
				b.WriteByte('\n')
			} else if n.SoftLineBreak() {
				b.WriteByte(' ')
			}
		case *ast.String:
			b.Write(n.Value)
		case *ast.CodeSpan:
			text := strings.TrimSpace(m.renderMarkdownInline(n, source))
			if text != "" {
				b.WriteString(m.st.cyan.Render(text))
			}
		case *ast.Emphasis:
			text := strings.TrimSpace(m.renderMarkdownInline(n, source))
			if text != "" {
				if n.Level >= 2 {
					b.WriteString(lipgloss.NewStyle().Bold(true).Render(text))
				} else {
					b.WriteString(text)
				}
			}
		case *ast.Link:
			text := strings.TrimSpace(m.renderMarkdownInline(n, source))
			if text != "" {
				b.WriteString(text)
			}
		case *ast.Image:
			text := strings.TrimSpace(m.renderMarkdownInline(n, source))
			if text != "" {
				b.WriteString(text)
			}
		case *ast.RawHTML:
			continue
		default:
			if child.HasChildren() {
				b.WriteString(m.renderMarkdownInline(child, source))
			}
		}
	}
	return b.String()
}

func (m Model) renderMarkdownList(list *ast.List, source []byte, depth int) []string {
	var lines []string
	indent := strings.Repeat("  ", depth)
	nextNumber := list.Start
	for item := list.FirstChild(); item != nil; item = item.NextSibling() {
		li, ok := item.(*ast.ListItem)
		if !ok {
			continue
		}
		itemLines := normalizeMarkdownLines(m.renderMarkdownNode(li, source, depth+1))
		if len(itemLines) == 0 {
			continue
		}
		prefix := "- "
		if list.IsOrdered() {
			prefix = fmt.Sprintf("%d. ", nextNumber)
			nextNumber++
		}
		lines = append(lines, indent+prefix+strings.TrimLeft(itemLines[0], " "))
		for _, line := range itemLines[1:] {
			if line == "" {
				lines = append(lines, "")
				continue
			}
			lines = append(lines, indent+"  "+line)
		}
	}
	return lines
}

func (m Model) renderMarkdownCodeBlock(content []byte, depth int) []string {
	text := strings.TrimRight(string(content), "\n")
	if strings.TrimSpace(text) == "" {
		return nil
	}
	indent := strings.Repeat("  ", depth) + "  "
	lines := strings.Split(text, "\n")
	out := make([]string, 0, len(lines))
	for _, line := range lines {
		out = append(out, m.st.dim.Render(indent+line))
	}
	return out
}

func (m Model) renderMarkdownTable(table *tableast.Table, source []byte, depth int) []string {
	var headers []string
	rows := make([]markdownTableRow, 0, 4)
	alignments := table.Alignments

	for child := table.FirstChild(); child != nil; child = child.NextSibling() {
		switch n := child.(type) {
		case *tableast.TableHeader:
			headers = renderTableCells(n, source)
		case *tableast.TableRow:
		rows = append(rows, markdownTableRow{cells: renderTableCells(n, source)})
		}
	}
	if len(headers) == 0 && len(rows) > 0 {
		headers = append([]string(nil), rows[0].cells...)
		rows = rows[1:]
	}
	if len(headers) == 0 {
		return nil
	}

	cols := len(headers)
	for _, r := range rows {
		if len(r.cells) > cols {
			cols = len(r.cells)
		}
	}
	if len(alignments) < cols {
		expanded := make([]tableast.Alignment, cols)
		copy(expanded, alignments)
		for i := len(alignments); i < cols; i++ {
			expanded[i] = tableast.AlignNone
		}
		alignments = expanded
	}

	widths := make([]int, cols)
	for i := 0; i < cols; i++ {
		if i < len(headers) {
			widths[i] = max(widths[i], ansi.StringWidth(headers[i]))
		}
		for _, r := range rows {
			if i < len(r.cells) {
				widths[i] = max(widths[i], ansi.StringWidth(r.cells[i]))
			}
		}
	}

	indent := strings.Repeat("  ", depth)
	total := 1
	for _, w := range widths {
		total += w + 3
	}
	if m.width > 0 && total > max(24, m.width-4) {
		return renderMarkdownTableFallback(indent, headers, rows)
	}

	borderTop := indent + "┌" + joinWidths(widths, "┬", "─") + "┐"
	borderMid := indent + "├" + joinWidths(widths, "┼", "─") + "┤"
	borderBottom := indent + "└" + joinWidths(widths, "┴", "─") + "┘"

	lines := []string{m.st.dim.Render(borderTop)}
	lines = append(lines, m.st.dim.Render(indent+"│"+renderTableRow(headers, widths, alignments, true)+"│"))
	lines = append(lines, m.st.dim.Render(borderMid))
	for _, r := range rows {
		lines = append(lines, m.st.dim.Render(indent+"│"+renderTableRow(r.cells, widths, alignments, false)+"│"))
	}
	lines = append(lines, m.st.dim.Render(borderBottom))
	return lines
}

func renderTableCells(node ast.Node, source []byte) []string {
	cells := make([]string, 0, 4)
	for cell := node.FirstChild(); cell != nil; cell = cell.NextSibling() {
		if cell.HasChildren() {
			cells = append(cells, strings.TrimSpace(renderTableInline(cell, source)))
			continue
		}
		cells = append(cells, strings.TrimSpace(renderTableInline(cell, source)))
	}
	return cells
}

func renderTableInline(node ast.Node, source []byte) string {
	var b strings.Builder
	for child := node.FirstChild(); child != nil; child = child.NextSibling() {
		switch n := child.(type) {
		case *ast.Text:
			b.Write(n.Value(source))
			if n.HardLineBreak() {
				b.WriteByte('\n')
			} else if n.SoftLineBreak() {
				b.WriteByte(' ')
			}
		case *ast.String:
			b.Write(n.Value)
		default:
			if child.HasChildren() {
				b.WriteString(renderTableInline(child, source))
			}
		}
	}
	return b.String()
}

func renderMarkdownTableFallback(indent string, headers []string, rows []markdownTableRow) []string {
	var lines []string
	for _, r := range rows {
		for i, header := range headers {
			value := ""
			if i < len(r.cells) {
				value = r.cells[i]
			}
			if value == "" {
				continue
			}
			lines = append(lines, indent+header+": "+value)
		}
		lines = append(lines, "")
	}
	return normalizeMarkdownLines(lines)
}

func renderTableRow(cells []string, widths []int, aligns []tableast.Alignment, header bool) string {
	parts := make([]string, len(widths))
	for i := range widths {
		value := ""
		if i < len(cells) {
			value = cells[i]
		}
		if header {
			value = strings.TrimSpace(value)
		}
		parts[i] = padTableCell(value, widths[i], aligns[i])
	}
	return " " + strings.Join(parts, " │ ") + " "
}

func padTableCell(value string, width int, align tableast.Alignment) string {
	if width <= 0 {
		return value
	}
	visible := ansi.StringWidth(value)
	if visible >= width {
		return value
	}
	pad := width - visible
	switch align {
	case tableast.AlignRight:
		return strings.Repeat(" ", pad) + value
	case tableast.AlignCenter:
		left := pad / 2
		right := pad - left
		return strings.Repeat(" ", left) + value + strings.Repeat(" ", right)
	default:
		return value + strings.Repeat(" ", pad)
	}
}

func joinWidths(widths []int, sep, fill string) string {
	parts := make([]string, 0, len(widths))
	for _, w := range widths {
		parts = append(parts, strings.Repeat(fill, max(1, w+2)))
	}
	return strings.Join(parts, sep)
}

func normalizeMarkdownLines(lines []string) []string {
	out := make([]string, 0, len(lines))
	prevBlank := true
	for _, line := range lines {
		blank := strings.TrimSpace(ansi.Strip(line)) == ""
		if blank {
			if !prevBlank {
				out = append(out, "")
			}
			prevBlank = true
			continue
		}
		out = append(out, line)
		prevBlank = false
	}
	for len(out) > 0 && strings.TrimSpace(ansi.Strip(out[len(out)-1])) == "" {
		out = out[:len(out)-1]
	}
	return out
}
