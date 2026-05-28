package llm

import "fmt"

// ValidateRequest checks provider-facing invariants for unified LLM requests.
func ValidateRequest(req *Request) error {
	if req == nil {
		return nil
	}

	seenTranscript := false
	pendingTools := make(map[string]int)
	for i, msg := range req.Messages {
		if !validRole(msg.Role) {
			return fmt.Errorf("llm request: invalid message role %q at index %d", msg.Role, i)
		}
		if msg.Role == RoleAssistant && !assistantHasPayload(msg) {
			return fmt.Errorf("llm request: empty assistant message at index %d", i)
		}
		if isPrivilegedRole(msg.Role) {
			if seenTranscript {
				return fmt.Errorf(
					"llm request: privileged %q message at index %d after transcript messages",
					msg.Role,
					i,
				)
			}
			continue
		}
		seenTranscript = true
		if msg.Role == RoleTool {
			if msg.ToolID == "" || pendingTools[msg.ToolID] == 0 {
				return fmt.Errorf("llm request: unmatched tool result at index %d", i)
			}
			pendingTools[msg.ToolID]--
			if pendingTools[msg.ToolID] == 0 {
				delete(pendingTools, msg.ToolID)
			}
			continue
		}
		clear(pendingTools)
		if msg.Role == RoleAssistant {
			for _, call := range msg.Calls {
				if call.ID != "" {
					pendingTools[call.ID]++
				}
			}
		}
	}
	return nil
}

func validRole(role Role) bool {
	switch role {
	case RoleSystem, RoleDeveloper, RoleUser, RoleAssistant, RoleTool:
		return true
	default:
		return false
	}
}

func isPrivilegedRole(role Role) bool {
	return role == RoleSystem || role == RoleDeveloper
}

func assistantHasPayload(msg Message) bool {
	return msg.HasAssistantPayload()
}
