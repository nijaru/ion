package llm

import "strings"

func flattenUnsupportedThinking(messages []Message) {
	for i := range messages {
		msg := &messages[i]
		if msg.Reasoning == "" && len(msg.ThinkingBlocks) == 0 {
			continue
		}
		msg.Content = appendThinkingText(msg.Content, msg.Reasoning, msg.ThinkingBlocks)
		msg.Reasoning = ""
		msg.ThinkingBlocks = nil
	}
}

func appendThinkingText(content, reasoning string, blocks []ThinkingBlock) string {
	var parts []string
	if reasoning != "" {
		parts = append(parts, "<thinking>\n"+reasoning+"\n</thinking>")
	}
	for _, block := range blocks {
		switch block.Type {
		case "thinking":
			if block.Thinking != "" {
				parts = append(parts, "<thinking>\n"+block.Thinking+"\n</thinking>")
			}
		case "redacted_thinking":
			// Redacted content is intentionally omitted when replaying across
			// providers that do not support native thinking blocks.
		}
	}
	if len(parts) == 0 {
		return content
	}
	if content == "" {
		return strings.Join(parts, "\n\n")
	}
	return content + "\n\n" + strings.Join(parts, "\n\n")
}
