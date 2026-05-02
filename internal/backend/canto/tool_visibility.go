package canto

import (
	"context"

	"github.com/nijaru/canto/llm"
	"github.com/nijaru/canto/prompt"
	"github.com/nijaru/canto/session"
	"github.com/nijaru/ion/internal/backend"
)

func toolVisibilityProcessor(policy *backend.PolicyEngine) prompt.RequestProcessor {
	return prompt.RequestProcessorFunc(
		func(ctx context.Context, p llm.Provider, model string, sess *session.Session, req *llm.Request) error {
			if policy == nil || len(req.Tools) == 0 {
				return nil
			}
			names := make([]string, 0, len(req.Tools))
			for _, spec := range req.Tools {
				if spec != nil {
					names = append(names, spec.Name)
				}
			}
			visibleNames := policy.VisibleToolNames(names)
			if len(visibleNames) == len(names) {
				return nil
			}
			visible := make(map[string]struct{}, len(visibleNames))
			for _, name := range visibleNames {
				visible[name] = struct{}{}
			}
			filtered := make([]*llm.Spec, 0, len(visibleNames))
			for _, spec := range req.Tools {
				if spec == nil {
					continue
				}
				if _, ok := visible[spec.Name]; ok {
					filtered = append(filtered, spec)
				}
			}
			req.Tools = filtered
			return nil
		},
	)
}
