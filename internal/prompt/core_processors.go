package prompt

import (
	"context"
	"regexp"
	"strings"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
	"github.com/nijaru/ion/tool"
)

// History appends the effective model-visible session history to the request.
func History() RequestProcessor {
	return RequestProcessorFunc(
		func(ctx context.Context, p llm.Provider, model string, sess *session.Session, req *llm.Request) error {
			entries, err := sess.EffectiveEntries()
			if err != nil {
				return err
			}
			req.CachePrefixMessages = len(req.Messages) + countPrefixContextMessages(entries)
			for _, entry := range entries {
				req.AppendMessage(entry.Message)
			}
			return nil
		},
	)
}

func countPrefixContextMessages(entries []session.HistoryEntry) int {
	count := 0
	for _, entry := range entries {
		if entry.EventType == session.ContextAdded &&
			entry.ContextPlacement == session.ContextPlacementPrefix {
			count++
			continue
		}
		if count > 0 {
			break
		}
	}
	return count
}

// Tools appends tool definitions to the LLM request.
func Tools(reg *tool.Registry) RequestProcessor {
	return &toolSpecsProcessor{Registry: reg}
}

type toolSpecsProcessor struct {
	Registry *tool.Registry
}

func (p *toolSpecsProcessor) WithToolRegistry(reg *tool.Registry) RequestProcessor {
	return &toolSpecsProcessor{Registry: reg}
}

func (p *toolSpecsProcessor) ApplyRequest(
	ctx context.Context,
	pr llm.Provider,
	model string,
	sess *session.Session,
	req *llm.Request,
) error {
	if p.Registry == nil {
		return nil
	}
	req.Tools = append(req.Tools, p.Registry.Specs()...)
	return nil
}

// Instructions prepends instructions as a system message.
//
// The system prompt is sourced from the session's SystemPrompt field.
// The message role is determined by the provider's capabilities
// (system vs developer).
func Instructions(instructions string) RequestProcessor {
	return RequestProcessorFunc(
		func(ctx context.Context, p llm.Provider, model string, sess *session.Session, req *llm.Request) error {
			// Combine explicit instructions with session's system prompt
			systemPrompt := sess.SystemPrompt()
			combined := instructions
			if systemPrompt != "" {
				if combined != "" {
					combined = combined + "\n\n" + systemPrompt
				} else {
					combined = systemPrompt
				}
			}
			if combined == "" {
				return nil
			}

			// Determine the appropriate role from provider capabilities
			role := llm.RoleSystem
			if p != nil {
				caps := p.Capabilities(model)
				if caps.SystemRole != "" {
					role = caps.SystemRole
				}
			}

			for i, m := range req.Messages {
				if m.Role == llm.RoleSystem || m.Role == llm.RoleDeveloper {
					req.Messages[i].Content = combined + "\n\n" + m.Content
					req.Messages[i].Role = role
					return nil
				}
			}

			sys := llm.Message{Role: role, Content: combined}
			req.PrependMessage(sys)
			return nil
		},
	)
}

// InjectContextBlock inserts a request-specific context block after the stable
// cache prefix, replacing any existing block matched by blockRegex.
func InjectContextBlock(req *llm.Request, blockRegex *regexp.Regexp, block string) {
	for i, m := range req.Messages {
		if m.Role == llm.RoleSystem || m.Role == llm.RoleDeveloper {
			if loc := blockRegex.FindStringIndex(m.Content); loc != nil {
				req.Messages[i].Content = strings.TrimSpace(m.Content[:loc[0]] + m.Content[loc[1]:])
			}
			continue
		}
		if loc := blockRegex.FindStringIndex(m.Content); loc != nil {
			req.Messages[i].Content = m.Content[:loc[0]] + block + "\n\n" + m.Content[loc[1]:]
			req.Messages[i].Role = llm.RoleUser
			return
		}
	}

	msg := llm.Message{Role: llm.RoleUser, Content: block}
	req.InsertAfterCachePrefix(msg)
}

// injectSystemBlock prepends block into the first system/developer message in req,
// replacing any existing match of blockRegex. If no system message exists,
// a new one is prepended.
func injectSystemBlock(req *llm.Request, blockRegex *regexp.Regexp, block string) {
	for i, m := range req.Messages {
		if m.Role != llm.RoleSystem && m.Role != llm.RoleDeveloper {
			continue
		}
		if loc := blockRegex.FindStringIndex(m.Content); loc != nil {
			req.Messages[i].Content = m.Content[:loc[0]] + block + "\n\n" + m.Content[loc[1]:]
		} else {
			req.Messages[i].Content = block + "\n\n" + m.Content
		}
		return
	}
	sys := llm.Message{Role: llm.RoleSystem, Content: block}
	req.PrependMessage(sys)
}
