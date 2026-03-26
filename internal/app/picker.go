package app

import (
	"context"
	"slices"
	"strings"

	"charm.land/catwalk/pkg/catwalk"
)

func providerItems() []pickerItem {
	items := []pickerItem{
		{Label: "anthropic", Value: "anthropic", Detail: "API key"},
		{Label: "openai", Value: "openai", Detail: "API key"},
		{Label: "openrouter", Value: "openrouter", Detail: "API key"},
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
	client := catwalk.New()
	providers, err := client.GetProviders(context.Background(), "")
	if err != nil {
		return nil, err
	}

	var items []pickerItem
	for _, p := range providers {
		if !strings.EqualFold(p.Name, resolved) && !strings.EqualFold(string(p.ID), resolved) {
			continue
		}
		for _, model := range p.Models {
			items = append(items, pickerItem{
				Label:  model.Name,
				Value:  model.Name,
				Detail: model.ID,
			})
		}
		break
	}

	slices.SortFunc(items, func(a, b pickerItem) int {
		return strings.Compare(a.Label, b.Label)
	})
	return items, nil
}

func modelBelongsToProvider(provider, model string) (bool, error) {
	items, err := modelItemsForProvider(provider)
	if err != nil {
		return false, err
	}
	for _, item := range items {
		if strings.EqualFold(item.Value, model) || strings.EqualFold(item.Label, model) {
			return true, nil
		}
	}
	return false, nil
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

func isACPProvider(provider string) bool {
	switch strings.ToLower(strings.TrimSpace(provider)) {
	case "claude-pro", "gemini-advanced", "gh-copilot", "chatgpt", "codex":
		return true
	default:
		return false
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
