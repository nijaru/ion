package llm

import "fmt"

func rewriteSystemMessages(req *Request, targetRole Role) {
	for i, m := range req.Messages {
		if m.Role != RoleSystem {
			continue
		}
		content := m.Content
		if targetRole == RoleUser {
			content = fmt.Sprintf("Instructions:\n%s", content)
		}
		req.Messages[i] = Message{
			Role:         targetRole,
			Content:      content,
			CacheControl: m.CacheControl,
		}
	}
}
