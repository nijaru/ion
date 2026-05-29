package prompt

import (
	"context"

	"github.com/go-json-experiment/json"

	"github.com/nijaru/ion/llm"
	"github.com/nijaru/ion/session"
	"github.com/nijaru/ion/tool"
)

const (
	// DefaultLazyThreshold is the tool count above which lazy loading activates.
	DefaultLazyThreshold = 20
)

// LazyTools conditionally loads tool specs.
//
// Explicitly deferred tools stay out of the default request until searched and
// unlocked. Once the registry grows beyond Threshold, all tool specs are kept
// out of the initial request except the search_tools meta-tool and any tools
// previously unlocked from session history.
type LazyTools struct {
	Registry  *tool.Registry
	Threshold int // default: DefaultLazyThreshold
}

// NewLazyTools creates a LazyTools with the given registry.
func NewLazyTools(reg *tool.Registry) *LazyTools {
	return &LazyTools{Registry: reg, Threshold: DefaultLazyThreshold}
}

func (p *LazyTools) WithToolRegistry(reg *tool.Registry) RequestProcessor {
	clone := *p
	clone.Registry = reg
	return &clone
}

func (p *LazyTools) ApplyRequest(
	ctx context.Context,
	pr llm.Provider,
	model string,
	sess *session.Session,
	req *llm.Request,
) error {
	if p.Registry == nil {
		return nil
	}

	threshold := p.Threshold
	if threshold <= 0 {
		threshold = DefaultLazyThreshold
	}

	entries := p.Registry.Entries()
	if len(entries) == 0 {
		return nil
	}

	unlocked, err := SearchUnlockedTools(sess)
	if err != nil {
		return err
	}

	hiddenCount := 0
	for _, entry := range entries {
		if entry.Name == tool.SearchToolName {
			continue
		}
		if shouldHideTool(entry, len(entries), threshold) {
			if _, ok := unlocked[entry.Name]; ok {
				spec := entry.Spec
				req.Tools = append(req.Tools, &spec)
				continue
			}
			hiddenCount++
			continue
		}
		spec := entry.Spec
		req.Tools = append(req.Tools, &spec)
	}

	if hiddenCount == 0 {
		return nil
	}

	searchSpec := tool.NewSearchTool(p.Registry).Spec()
	req.Tools = append([]*llm.Spec{&searchSpec}, req.Tools...)
	injectSystemHint(
		req,
		"Additional tools are available via search_tools. Search by capability, keyword, category, or exact tool name before calling a hidden tool.",
	)

	return nil
}

// SearchUnlockedTools derives the set of tools previously unlocked via
// successful search_tools results recorded in the session's tool-completed
// events.
func SearchUnlockedTools(sess *session.Session) (map[string]struct{}, error) {
	unlocked := make(map[string]struct{})
	var errOut error
	for e := range sess.All() {
		result, ok, err := e.ToolCompletedData()
		if err != nil {
			errOut = err
			break
		}
		if !ok || result.Tool != tool.SearchToolName {
			continue
		}

		var specs []llm.Spec
		if err := json.Unmarshal([]byte(result.Output), &specs); err != nil {
			continue
		}
		for _, spec := range specs {
			if spec.Name == "" || spec.Name == tool.SearchToolName {
				continue
			}
			unlocked[spec.Name] = struct{}{}
		}
	}
	return unlocked, errOut
}

func shouldHideTool(entry tool.ToolEntry, total, threshold int) bool {
	return entry.Metadata.Deferred || total > threshold
}

// injectSystemHint prepends a system message with the hint text.
func injectSystemHint(req *llm.Request, hint string) {
	for i, m := range req.Messages {
		if m.Role == llm.RoleSystem {
			req.Messages[i].Content += "\n\n" + hint
			return
		}
	}
	// No system message yet — prepend one.
	sys := llm.Message{Role: llm.RoleSystem, Content: hint}
	req.PrependMessage(sys)
}
