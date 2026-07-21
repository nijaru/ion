package llm

import (
	"fmt"
	"strings"
)

const missingToolResultContent = "No result provided."

// TransformRequestForCapabilities adapts a unified request to a model's
// capability constraints while preserving transcript continuity when sessions
// move across providers.
func TransformRequestForCapabilities(req *Request, caps Capabilities) {
	if req == nil {
		return
	}
	if !caps.Tools {
		// Tool definitions are part of the provider request surface. A model
		// that cannot call tools must never receive them, even when the
		// neutral transcript still contains historical tool activity.
		req.Tools = nil
	}

	if caps.SystemRole != RoleSystem {
		rewriteSystemMessages(req, caps.SystemRole)
	}
	if !caps.Temperature {
		req.Temperature = 0
	}
	if caps.SupportsThinking() {
		if req.ThinkingBudget > 0 {
			if !caps.SupportsThinkingBudget(req.ThinkingBudget) {
				req.ThinkingBudget = 0
			}
			// The provider-native budget is authoritative when present.
			req.ReasoningEffort = ""
		} else if req.ReasoningEffort != "" {
			// Convert the shared user-facing level before the generic effort
			// validation below, since budget models have a different native
			// control shape.
			req.ThinkingBudget = caps.ThinkingBudgetForEffort(req.ReasoningEffort)
			if req.ThinkingBudget > 0 {
				req.ReasoningEffort = ""
			}
		}
	}
	if req.ReasoningEffort != "" && !caps.SupportsReasoningControl(req.ReasoningEffort) {
		req.ReasoningEffort = ""
	}
	if req.ThinkingBudget > 0 && !caps.SupportsThinkingBudget(req.ThinkingBudget) {
		req.ThinkingBudget = 0
	}

	normalizeToolIDs(req.Messages)
	if !caps.SupportsThinking() {
		flattenUnsupportedThinking(req.Messages)
	}
	synthesizeMissingToolResults(req)
}

// PrepareRequestForCapabilities returns a provider-ready copy of req adapted
// to caps. The original request remains neutral and can be prepared again for a
// different provider or model.
func PrepareRequestForCapabilities(req *Request, caps Capabilities) (*Request, error) {
	if err := ValidateRequest(req); err != nil {
		return nil, err
	}
	prepared := req.Clone()
	TransformRequestForCapabilities(prepared, caps)
	if err := ValidateRequest(prepared); err != nil {
		return nil, err
	}
	return prepared, nil
}
func rewriteSystemMessages(req *Request, targetRole Role) {
	for i, m := range req.Messages {
		if m.Role != RoleSystem {
			continue
		}
		content := m.TextContent()
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
		if block.Redacted {
			// Redacted content is intentionally omitted when replaying across
			// providers that do not support native thinking blocks.
			continue
		}
		if block.Thinking != "" {
			parts = append(parts, "<thinking>\n"+block.Thinking+"\n</thinking>")
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
