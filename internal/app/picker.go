package app

import (
	"context"
	"fmt"
	"os"
	"slices"
	"strings"
	"unicode/utf8"

	"charm.land/catwalk/pkg/catwalk"

	"github.com/nijaru/ion/internal/backend/registry"
)

var listModels = registry.ListModels

func providerItems() []pickerItem {
	items := []pickerItem{
		{Label: "anthropic", Value: "anthropic", Detail: providerDetail("anthropic")},
		{Label: "openai", Value: "openai", Detail: providerDetail("openai")},
		{Label: "openrouter", Value: "openrouter", Detail: providerDetail("openrouter")},
		{Label: "gemini", Value: "gemini", Detail: providerDetail("gemini")},
		{Label: "claude-pro", Value: "claude-pro", Detail: "ACP"},
		{Label: "gemini-advanced", Value: "gemini-advanced", Detail: "ACP"},
		{Label: "gh-copilot", Value: "gh-copilot", Detail: "ACP"},
		{Label: "chatgpt", Value: "chatgpt", Detail: "ACP"},
		{Label: "codex", Value: "codex", Detail: "ACP"},
	}

	slices.SortFunc(items, func(a, b pickerItem) int {
		return strings.Compare(a.Label, b.Label)
	})
	return items
}

func pickerIndex(items []pickerItem, value string) int {
	for i, item := range items {
		if strings.EqualFold(item.Value, value) || strings.EqualFold(item.Label, value) {
			return i
		}
	}
	return 0
}

func modelItemsForProvider(provider string) ([]pickerItem, error) {
	resolved := catwalkProvider(provider)
	models, err := listModels(context.Background(), resolved)
	if err != nil {
		return nil, err
	}

	var items []pickerItem
	for _, model := range models {
		items = append(items, pickerItem{
			Label:  model.ID,
			Value:  model.ID,
			Detail: modelDetail(model),
		})
	}

	slices.SortFunc(items, func(a, b pickerItem) int {
		return strings.Compare(a.Label, b.Label)
	})
	return items, nil
}

func providerDetail(provider string) string {
	switch strings.ToLower(strings.TrimSpace(provider)) {
	case "claude-pro", "gemini-advanced", "gh-copilot", "chatgpt", "codex":
		return "ACP"
	case "anthropic":
		return keyDetail("ANTHROPIC_API_KEY")
	case "openai":
		return keyDetail("OPENAI_API_KEY")
	case "openrouter":
		return keyDetail("OPENROUTER_API_KEY")
	case "gemini":
		if os.Getenv("GEMINI_API_KEY") != "" || os.Getenv("GOOGLE_API_KEY") != "" {
			return "API key set"
		}
		return "API key missing"
	default:
		return "API key"
	}
}

func keyDetail(env string) string {
	if strings.TrimSpace(os.Getenv(env)) != "" {
		return "API key set"
	}
	return "API key missing"
}

func modelDetail(meta registry.ModelMetadata) string {
	var parts []string
	if meta.ContextLimit > 0 {
		if meta.ContextLimit >= 1000 {
			parts = append(parts, fmt.Sprintf("%dk ctx", meta.ContextLimit/1000))
		} else {
			parts = append(parts, fmt.Sprintf("%d ctx", meta.ContextLimit))
		}
	}
	if meta.InputPrice > 0 || meta.OutputPrice > 0 {
		parts = append(parts, fmt.Sprintf("$%.4f/$%.4f", meta.InputPrice, meta.OutputPrice))
	}
	if len(parts) == 0 {
		return ""
	}
	return strings.Join(parts, " · ")
}

func catwalkProvider(provider string) string {
	switch strings.ToLower(strings.TrimSpace(provider)) {
	case "claude-pro":
		return string(catwalk.InferenceProviderAnthropic)
	case "gemini-advanced":
		return string(catwalk.InferenceProviderGemini)
	case "gh-copilot":
		return string(catwalk.InferenceProviderCopilot)
	case "chatgpt", "codex":
		return string(catwalk.InferenceProviderOpenAI)
	default:
		return strings.ToLower(strings.TrimSpace(provider))
	}
}

func pickerWindow(title string, items []pickerItem, selected int) string {
	var b strings.Builder
	b.WriteString(title)
	b.WriteString("\n")
	for i, item := range items {
		prefix := "  "
		if i == selected {
			prefix = "› "
		}
		line := prefix + item.Label
		if item.Detail != "" {
			line += " · " + item.Detail
		}
		b.WriteString(line)
		b.WriteString("\n")
	}
	b.WriteString("esc cancel · enter select")
	return b.String()
}

func refreshPickerFilter(m *Model) {
	if m.picker == nil {
		return
	}
	query := strings.TrimSpace(m.picker.query)
	if query == "" {
		m.picker.filtered = append([]pickerItem(nil), m.picker.items...)
	} else {
		filtered := make([]pickerItem, 0, len(m.picker.items))
		for _, item := range m.picker.items {
			if pickerMatches(query, item) {
				filtered = append(filtered, item)
			}
		}
		m.picker.filtered = filtered
	}
	if len(m.picker.filtered) == 0 {
		m.picker.index = 0
		return
	}
	if m.picker.index >= len(m.picker.filtered) {
		m.picker.index = len(m.picker.filtered) - 1
	}
}

func pickerDisplayItems(p *pickerState) []pickerItem {
	if p == nil {
		return nil
	}
	if len(p.filtered) > 0 || p.query != "" {
		return p.filtered
	}
	return p.items
}

func pickerMatches(query string, item pickerItem) bool {
	candidate := strings.ToLower(strings.Join([]string{item.Label, item.Value, item.Detail}, " "))
	q := strings.ToLower(strings.TrimSpace(query))
	if q == "" {
		return true
	}
	if strings.Contains(candidate, q) {
		return true
	}
	idx := 0
	for _, r := range q {
		next := strings.IndexRune(candidate[idx:], r)
		if next < 0 {
			return false
		}
		idx += next + utf8.RuneLen(r)
	}
	return true
}
