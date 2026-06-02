package tool

import (
	"bytes"
	"fmt"
	"os"
	"strings"
	"unicode/utf8"
)

const (
	bashMaxOutputLines = defaultReadLineLimit
	bashTempFilePrefix = "ion-bash-"
)

type bashOutputTruncation struct {
	Content         string
	Truncated       bool
	TruncatedBy     string
	TotalLines      int
	TotalBytes      int
	OutputLines     int
	OutputBytes     int
	LastLinePartial bool
	MaxLines        int
	MaxBytes        int
}

type bashOutputSnapshot struct {
	Content        string
	Truncation     bashOutputTruncation
	FullOutputPath string
}

type bashOutputAccumulator struct {
	maxLines int
	maxBytes int

	rawChunks [][]byte
	tail      []byte

	tailStartsAtLineBoundary bool
	totalBytes               int
	completedLines           int
	currentLineBytes         int
	hasOpenLine              bool

	tempFile *os.File
	tempPath string
}

func newBashOutputAccumulator() *bashOutputAccumulator {
	return &bashOutputAccumulator{
		maxLines:                 bashMaxOutputLines,
		maxBytes:                 MaxToolOutputSize,
		tailStartsAtLineBoundary: true,
	}
}

func (a *bashOutputAccumulator) append(data []byte) error {
	if len(data) == 0 {
		return nil
	}

	a.totalBytes += len(data)
	a.updateLineCounters(data)
	a.appendTail(data)

	if a.tempFile != nil || a.truncated() {
		if err := a.ensureTempFile(); err != nil {
			return err
		}
		_, err := a.tempFile.Write(data)
		return err
	}

	a.rawChunks = append(a.rawChunks, bytes.Clone(data))
	return nil
}

func (a *bashOutputAccumulator) snapshot(persistIfTruncated bool) (bashOutputSnapshot, error) {
	truncation := truncateBashTail(a.snapshotText(), a.maxLines, a.maxBytes)
	truncated := a.truncated()
	truncatedBy := truncation.TruncatedBy
	if truncated && truncatedBy == "" {
		if a.totalBytes > a.maxBytes {
			truncatedBy = "bytes"
		} else {
			truncatedBy = "lines"
		}
	}
	truncation.Truncated = truncated
	truncation.TruncatedBy = truncatedBy
	truncation.TotalLines = a.totalLines()
	truncation.TotalBytes = a.totalBytes
	truncation.MaxLines = a.maxLines
	truncation.MaxBytes = a.maxBytes

	if persistIfTruncated && truncation.Truncated {
		if err := a.ensureTempFile(); err != nil {
			return bashOutputSnapshot{}, err
		}
	}

	return bashOutputSnapshot{
		Content:        truncation.Content,
		Truncation:     truncation,
		FullOutputPath: a.tempPath,
	}, nil
}

func (a *bashOutputAccumulator) closeTempFile() error {
	if a.tempFile == nil {
		return nil
	}
	err := a.tempFile.Close()
	a.tempFile = nil
	return err
}

func (a *bashOutputAccumulator) lastLineBytes() int {
	return a.currentLineBytes
}

func (a *bashOutputAccumulator) truncated() bool {
	return a.totalLines() > a.maxLines || a.totalBytes > a.maxBytes
}

func (a *bashOutputAccumulator) updateLineCounters(data []byte) {
	newlines := bytes.Count(data, []byte{'\n'})
	if newlines == 0 {
		a.currentLineBytes += len(data)
		a.hasOpenLine = true
		return
	}

	a.completedLines += newlines
	lastNewline := bytes.LastIndexByte(data, '\n')
	a.currentLineBytes = len(data) - lastNewline - 1
	a.hasOpenLine = a.currentLineBytes > 0
}

func (a *bashOutputAccumulator) totalLines() int {
	if a.hasOpenLine {
		return a.completedLines + 1
	}
	return a.completedLines
}

func (a *bashOutputAccumulator) appendTail(data []byte) {
	a.tail = append(a.tail, data...)

	maxRollingBytes := a.maxBytes * 2
	if len(a.tail) <= maxRollingBytes*2 {
		return
	}

	start := len(a.tail) - maxRollingBytes
	for start < len(a.tail) && !utf8.RuneStart(a.tail[start]) {
		start++
	}
	if start >= len(a.tail) {
		a.tail = a.tail[:0]
		a.tailStartsAtLineBoundary = true
		return
	}
	a.tailStartsAtLineBoundary = a.tail[start-1] == '\n'
	a.tail = append([]byte(nil), a.tail[start:]...)
}

func (a *bashOutputAccumulator) snapshotText() string {
	if a.tailStartsAtLineBoundary {
		return string(a.tail)
	}
	firstNewline := bytes.IndexByte(a.tail, '\n')
	if firstNewline == -1 {
		return string(a.tail)
	}
	return string(a.tail[firstNewline+1:])
}

func (a *bashOutputAccumulator) ensureTempFile() error {
	if a.tempFile != nil {
		return nil
	}
	file, err := os.CreateTemp("", bashTempFilePrefix+"*.log")
	if err != nil {
		return fmt.Errorf("create full bash output file: %w", err)
	}
	for _, chunk := range a.rawChunks {
		if _, err := file.Write(chunk); err != nil {
			_ = file.Close()
			return fmt.Errorf("write full bash output file: %w", err)
		}
	}
	a.rawChunks = nil
	a.tempFile = file
	a.tempPath = file.Name()
	return nil
}

func formatBashSnapshot(
	snapshot bashOutputSnapshot,
	acc *bashOutputAccumulator,
	emptyText string,
) string {
	text := snapshot.Content
	if text == "" {
		text = emptyText
	}

	truncation := snapshot.Truncation
	if !truncation.Truncated {
		return text
	}

	startLine := truncation.TotalLines - truncation.OutputLines + 1
	if startLine < 1 {
		startLine = 1
	}
	endLine := truncation.TotalLines
	fullOutput := snapshot.FullOutputPath
	if fullOutput == "" {
		fullOutput = "(unavailable)"
	}

	var note string
	if truncation.LastLinePartial {
		note = fmt.Sprintf(
			"Showing last %s of line %d (line is %s). Full output: %s",
			formatSize(truncation.OutputBytes),
			endLine,
			formatSize(acc.lastLineBytes()),
			fullOutput,
		)
	} else if truncation.TruncatedBy == "lines" {
		note = fmt.Sprintf(
			"Showing lines %d-%d of %d. Full output: %s",
			startLine,
			endLine,
			truncation.TotalLines,
			fullOutput,
		)
	} else {
		note = fmt.Sprintf(
			"Showing lines %d-%d of %d (%s limit). Full output: %s",
			startLine,
			endLine,
			truncation.TotalLines,
			formatSize(truncation.MaxBytes),
			fullOutput,
		)
	}

	text = strings.TrimRight(text, "\n")
	if text == "" {
		return "[" + note + "]"
	}
	return text + "\n\n[" + note + "]"
}

func truncateBashTail(content string, maxLines, maxBytes int) bashOutputTruncation {
	totalBytes := len(content)
	lines := splitBashLinesForCounting(content)
	totalLines := len(lines)
	if totalLines <= maxLines && totalBytes <= maxBytes {
		return bashOutputTruncation{
			Content:         content,
			Truncated:       false,
			TotalLines:      totalLines,
			TotalBytes:      totalBytes,
			OutputLines:     totalLines,
			OutputBytes:     totalBytes,
			LastLinePartial: false,
			MaxLines:        maxLines,
			MaxBytes:        maxBytes,
		}
	}

	outputLines := make([]string, 0, min(len(lines), maxLines))
	outputBytes := 0
	truncatedBy := "lines"
	lastLinePartial := false
	for i := len(lines) - 1; i >= 0 && len(outputLines) < maxLines; i-- {
		line := lines[i]
		lineBytes := len(line)
		if len(outputLines) > 0 {
			lineBytes++
		}
		if outputBytes+lineBytes > maxBytes {
			truncatedBy = "bytes"
			if len(outputLines) == 0 {
				truncatedLine := truncateStringToBytesFromEnd(line, maxBytes)
				outputLines = append(outputLines, truncatedLine)
				outputBytes = len(truncatedLine)
				lastLinePartial = true
			}
			break
		}
		outputLines = append(outputLines, line)
		outputBytes += lineBytes
	}

	for left, right := 0, len(outputLines)-1; left < right; left, right = left+1, right-1 {
		outputLines[left], outputLines[right] = outputLines[right], outputLines[left]
	}
	if len(outputLines) >= maxLines && outputBytes <= maxBytes {
		truncatedBy = "lines"
	}
	output := strings.Join(outputLines, "\n")

	return bashOutputTruncation{
		Content:         output,
		Truncated:       true,
		TruncatedBy:     truncatedBy,
		TotalLines:      totalLines,
		TotalBytes:      totalBytes,
		OutputLines:     len(outputLines),
		OutputBytes:     len(output),
		LastLinePartial: lastLinePartial,
		MaxLines:        maxLines,
		MaxBytes:        maxBytes,
	}
}

func splitBashLinesForCounting(content string) []string {
	if content == "" {
		return nil
	}
	lines := strings.Split(content, "\n")
	if strings.HasSuffix(content, "\n") {
		lines = lines[:len(lines)-1]
	}
	return lines
}

func truncateStringToBytesFromEnd(text string, maxBytes int) string {
	if len(text) <= maxBytes {
		return text
	}
	start := len(text) - maxBytes
	for start < len(text) && !utf8.RuneStart(text[start]) {
		start++
	}
	if start >= len(text) {
		return ""
	}
	return text[start:]
}

func formatSize(bytes int) string {
	switch {
	case bytes < 1024:
		return fmt.Sprintf("%dB", bytes)
	case bytes < 1024*1024:
		return fmt.Sprintf("%.1fKB", float64(bytes)/1024)
	default:
		return fmt.Sprintf("%.1fMB", float64(bytes)/(1024*1024))
	}
}
