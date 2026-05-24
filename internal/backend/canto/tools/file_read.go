package tools

import (
	"context"
	"fmt"
	"os"
	"strings"

	"github.com/nijaru/canto/llm"
)

// Read tool (formerly read_file)
type Read struct {
	FileTool
}

const defaultReadLineLimit = 2000

func (r *Read) Spec() llm.Spec {
	return llm.Spec{
		Name:        "read",
		Description: "Read file contents with line numbers. Returns the full file or a specific line range (use offset/limit for large files).",
		Parameters:  readParameters(),
	}
}

func (r *Read) Execute(ctx context.Context, args string) (string, error) {
	input, err := decodeToolArgs[readInput]("read", args)
	if err != nil {
		return "", err
	}
	if input.Offset < 0 {
		return "", fmt.Errorf("offset must be non-negative")
	}
	if input.Limit < 0 {
		return "", fmt.Errorf("limit must be non-negative")
	}

	absPath, err := r.absolutePath(input.Path)
	if err != nil {
		return "", err
	}

	content, err := os.ReadFile(absPath)
	if err != nil {
		return "", err
	}

	output, err := numberedReadOutput(string(content), input.Offset, input.Limit)
	if err != nil {
		return "", err
	}
	return output, nil
}

func numberedReadOutput(content string, offset, limit int) (string, error) {
	if content == "" {
		return "", nil
	}

	lines := strings.Split(content, "\n")
	if strings.HasSuffix(content, "\n") && len(lines) > 0 {
		lines = lines[:len(lines)-1]
	}
	start := 0
	if offset > 0 {
		start = offset - 1
	}
	if start >= len(lines) {
		return "", fmt.Errorf(
			"offset %d is beyond end of file (%d lines total)",
			offset,
			len(lines),
		)
	}

	maxEnd := len(lines)
	userLimited := limit > 0
	if limit > 0 {
		maxEnd = min(start+limit, len(lines))
	} else {
		maxEnd = min(start+defaultReadLineLimit, len(lines))
	}
	output, end, byteLimited := numberedLinesLimited(lines, start, maxEnd, maxToolOutputSize)
	if byteLimited {
		output += readContinuationNotice(start, end, len(lines), true)
	} else if userLimited && end < len(lines) {
		remaining := len(lines) - end
		output += fmt.Sprintf(
			"\n\n[%d more line(s) in file. Use offset=%d to continue.]",
			remaining,
			end+1,
		)
	} else if !userLimited && end < len(lines) {
		output += readContinuationNotice(start, end, len(lines), byteLimited)
	}
	return output, nil
}

func numberedLinesLimited(lines []string, start, end, byteLimit int) (string, int, bool) {
	var b strings.Builder
	byteLimited := false
	written := 0
	for i := start; i < end; i++ {
		line := numberedLine(i, lines[i])
		if b.Len() > 0 {
			line = "\n" + line
		}
		if byteLimit > 0 && b.Len()+len(line) > byteLimit {
			byteLimited = true
			break
		}
		b.WriteString(line)
		written++
	}
	return b.String(), start + written, byteLimited
}

func numberedLine(index int, line string) string {
	if index == 0 {
		line = strings.TrimPrefix(line, "\ufeff")
	}
	line = strings.TrimSuffix(line, "\r")
	return fmt.Sprintf("%6d\t%s", index+1, line)
}

func readContinuationNotice(start, end, total int, byteLimited bool) string {
	if end <= start {
		return fmt.Sprintf(
			"[Line %d exceeds %d bytes after numbering. Use offset=%d with a narrower command or bash to inspect it.]",
			start+1,
			maxToolOutputSize,
			start+1,
		)
	}
	limitReason := fmt.Sprintf("%d line limit", defaultReadLineLimit)
	if byteLimited {
		limitReason = fmt.Sprintf("%d byte limit", maxToolOutputSize)
	}
	return fmt.Sprintf(
		"\n\n[Showing lines %d-%d of %d (%s). Use offset=%d to continue.]",
		start+1,
		end,
		total,
		limitReason,
		end+1,
	)
}
