package tools

import (
	"context"
	"encoding/base64"
	"fmt"
	"os"
	"strings"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/tool"
)

// Read tool (formerly read_file)
type Read struct {
	FileTool
}

const (
	defaultReadLineLimit = 2000
	maxInlineImageBytes  = 5 * 1024 * 1024
)

var _ tool.ContentTool = (*Read)(nil)

func (r *Read) Spec() llm.Spec {
	return llm.Spec{
		Name:        "read",
		Description: "Read text files with line numbers, or return supported images (png, jpeg, gif, webp) as image attachments. For text, returns the full file or a specific line range (use offset/limit for large files).",
		Parameters:  readParameters(),
	}
}

func (r *Read) Execute(ctx context.Context, args string) (string, error) {
	parts, err := r.ExecuteContent(ctx, args)
	if err != nil {
		return "", err
	}
	return llm.Message{Parts: parts}.TextContent(), nil
}

func (r *Read) ExecuteContent(ctx context.Context, args string) ([]llm.ContentPart, error) {
	input, err := decodeToolArgs[readInput]("read", args)
	if err != nil {
		return nil, err
	}
	if input.Offset < 0 {
		return nil, fmt.Errorf("offset must be non-negative")
	}
	if input.Limit < 0 {
		return nil, fmt.Errorf("limit must be non-negative")
	}

	absPath, err := r.readPath(input.Path)
	if err != nil {
		return nil, err
	}
	if err := ctx.Err(); err != nil {
		return nil, toolContextErr("read", err)
	}

	content, err := os.ReadFile(absPath)
	if err != nil {
		return nil, err
	}
	if mimeType := detectSupportedImageMIMEType(content); mimeType != "" {
		return imageReadParts(mimeType, content), nil
	}

	output, err := numberedReadOutput(string(content), input.Offset, input.Limit)
	if err != nil {
		return nil, err
	}
	return []llm.ContentPart{llm.TextPart(output)}, nil
}

func imageReadParts(mimeType string, data []byte) []llm.ContentPart {
	note := fmt.Sprintf("Read image file [%s]", mimeType)
	if len(data) > maxInlineImageBytes {
		return []llm.ContentPart{llm.TextPart(fmt.Sprintf(
			"%s\n[Image omitted: file is %d bytes, exceeds %d byte inline image limit.]",
			note,
			len(data),
			maxInlineImageBytes,
		))}
	}
	return []llm.ContentPart{
		llm.TextPart(note),
		llm.ImagePart(mimeType, base64.StdEncoding.EncodeToString(data)),
	}
}

func detectSupportedImageMIMEType(data []byte) string {
	switch {
	case len(data) >= 8 &&
		data[0] == 0x89 &&
		string(data[1:4]) == "PNG" &&
		string(data[4:8]) == "\r\n\x1a\n":
		return "image/png"
	case len(data) >= 3 && data[0] == 0xff && data[1] == 0xd8 && data[2] == 0xff:
		return "image/jpeg"
	case len(data) >= 6 && (string(data[:6]) == "GIF87a" || string(data[:6]) == "GIF89a"):
		return "image/gif"
	case len(data) >= 12 && string(data[:4]) == "RIFF" && string(data[8:12]) == "WEBP":
		return "image/webp"
	default:
		return ""
	}
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
