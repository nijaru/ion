package llm

import (
	"fmt"
	"strings"
)

func synthesizeMissingToolResults(req *Request) {
	if len(req.Messages) == 0 {
		req.Messages = nil
		return
	}

	type pendingCall struct {
		id       string
		name     string
		inPrefix bool
	}

	var transformed []Message
	var pending []pendingCall
	insertedBeforePrefix := 0
	originalPrefix := req.CachePrefixMessages

	flushPending := func() {
		for _, call := range pending {
			if call.inPrefix {
				insertedBeforePrefix++
			}
			transformed = append(transformed, Message{
				Role:    RoleTool,
				Name:    call.name,
				ToolID:  call.id,
				Content: missingToolResultContent,
			})
		}
		pending = pending[:0]
	}

	for i, msg := range req.Messages {
		if msg.Role != RoleTool && len(pending) > 0 {
			flushPending()
		}

		transformed = append(transformed, msg)

		if msg.Role == RoleAssistant {
			for _, call := range msg.Calls {
				pending = append(pending, pendingCall{
					id:       call.ID,
					name:     call.Function.Name,
					inPrefix: originalPrefix > 0 && i < originalPrefix,
				})
			}
			continue
		}

		if msg.Role != RoleTool || msg.ToolID == "" || len(pending) == 0 {
			continue
		}

		for i := 0; i < len(pending); i++ {
			if pending[i].id != msg.ToolID {
				continue
			}
			pending = append(pending[:i], pending[i+1:]...)
			break
		}
	}

	if len(pending) > 0 {
		flushPending()
	}

	req.Messages = transformed
	if originalPrefix > 0 {
		req.CachePrefixMessages = originalPrefix + insertedBeforePrefix
	}
}

func normalizeToolIDs(messages []Message) {
	used := make(map[string]int)
	pending := make(map[string][]string)

	popPending := func(original string) (string, bool) {
		queue := pending[original]
		if len(queue) == 0 {
			return "", false
		}
		normalized := queue[0]
		queue = queue[1:]
		if len(queue) == 0 {
			delete(pending, original)
		} else {
			pending[original] = queue
		}
		return normalized, true
	}

	for i := range messages {
		msg := &messages[i]
		switch msg.Role {
		case RoleAssistant:
			clear(pending)
			for j := range msg.Calls {
				original := msg.Calls[j].ID
				normalized := uniqueToolCallID(original, used)
				msg.Calls[j].ID = normalized
				pending[original] = append(pending[original], normalized)
			}
		case RoleTool:
			if msg.ToolID == "" {
				continue
			}
			if normalized, ok := popPending(msg.ToolID); ok {
				msg.ToolID = normalized
				continue
			}
			msg.ToolID = uniqueToolCallID(msg.ToolID, used)
		default:
			clear(pending)
		}
	}
}

func uniqueToolCallID(id string, used map[string]int) string {
	base := normalizeToolCallID(id)
	if base == "" {
		base = "tool"
	}

	n := used[base]
	if n == 0 {
		used[base] = 1
		return base
	}

	for {
		n++
		suffix := fmt.Sprintf("-%d", n)
		trimmed := base
		if len(trimmed)+len(suffix) > 64 {
			trimmed = trimmed[:64-len(suffix)]
		}
		candidate := trimmed + suffix
		if used[candidate] == 0 {
			used[base] = n
			used[candidate] = 1
			return candidate
		}
	}
}

func normalizeToolCallID(id string) string {
	if id == "" {
		return ""
	}
	var b strings.Builder
	b.Grow(len(id))
	for _, r := range id {
		switch {
		case r >= 'a' && r <= 'z':
			b.WriteRune(r)
		case r >= 'A' && r <= 'Z':
			b.WriteRune(r)
		case r >= '0' && r <= '9':
			b.WriteRune(r)
		case r == '-' || r == '_':
			b.WriteRune(r)
		default:
			b.WriteByte('_')
		}
		if b.Len() >= 64 {
			break
		}
	}
	return strings.Trim(b.String(), "_")
}
