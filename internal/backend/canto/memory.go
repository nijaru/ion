package canto

import (
	"context"
	"fmt"
	"strings"

	"github.com/nijaru/canto/memory"
)

func (b *Backend) MemoryView(ctx context.Context, query string) (string, error) {
	if b.memory == nil || b.coreMemory == nil {
		return "", fmt.Errorf("memory is not initialized")
	}
	cwd := b.Meta()["cwd"]
	namespace := memory.Namespace{Scope: memory.ScopeWorkspace, ID: cwd}
	query = strings.TrimSpace(query)
	if query == "" {
		snapshot, err := memory.NewIndex(b.coreMemory).Snapshot(ctx, memory.IndexQuery{
			Namespaces: []memory.Namespace{namespace},
			Limit:      64,
		})
		if err != nil {
			return "", err
		}
		out := snapshot.String()
		if strings.TrimSpace(out) == "" {
			return "No workspace memory indexed yet", nil
		}
		return out, nil
	}
	memories, err := b.memory.Search(ctx, memory.Query{
		Namespaces:  []memory.Namespace{namespace},
		Text:        query,
		Limit:       10,
		IncludeCore: true,
	})
	if err != nil {
		return "", err
	}
	if len(memories) == 0 {
		return "No memory results for " + query, nil
	}
	var sb strings.Builder
	for _, item := range memories {
		if sb.Len() > 0 {
			sb.WriteString("\n\n")
		}
		label := string(item.Role)
		if item.Key != "" {
			label += ":" + item.Key
		}
		sb.WriteString(label)
		sb.WriteString("\n")
		sb.WriteString(strings.TrimSpace(item.Content))
	}
	return sb.String(), nil
}
