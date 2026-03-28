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
		providerItem("Anthropic", "anthropic"),
		providerItem("Gemini", "gemini"),
		providerItem("OpenAI", "openai"),
		providerItem("OpenRouter", "openrouter"),
		providerItem("Ollama", "ollama"),
	}
	slices.SortFunc(items, func(a, b pickerItem) int {
		if rankA, rankB := providerSortRank(a.Value), providerSortRank(b.Value); rankA != rankB {
			return rankA - rankB
		}
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

func providerDisplayName(value string) string {
	for _, item := range providerItems() {
		if strings.EqualFold(item.Value, value) || strings.EqualFold(item.Label, value) {
			return item.Label
		}
	}
	return value
}

func modelItemsForProvider(provider string) ([]pickerItem, error) {
	resolved := catwalkProvider(provider)
	models, err := listModels(context.Background(), resolved)
	if err != nil {
		return nil, err
	}

	var items []pickerItem
	for _, model := range models {
		metrics := modelMetrics(model)
		items = append(items, pickerItem{
			Label:   model.ID,
			Value:   model.ID,
			Metrics: metrics,
			Search:  pickerSearchIndex(model.ID, model.ID, "", "", metrics),
		})
	}

	slices.SortFunc(items, func(a, b pickerItem) int {
		return strings.Compare(a.Label, b.Label)
	})
	return items, nil
}

func providerItem(label, value string) pickerItem {
	detail, tone := providerDetail(value)
	return pickerItem{
		Label:  label,
		Value:  value,
		Detail: detail,
		Tone:   tone,
		Search: pickerSearchIndex(label, value, detail, "", nil),
	}
}

func providerDetail(provider string) (string, pickerTone) {
	switch strings.ToLower(strings.TrimSpace(provider)) {
	case "anthropic":
		return keyDetail("ANTHROPIC_API_KEY")
	case "openai":
		return keyDetail("OPENAI_API_KEY")
	case "openrouter":
		return keyDetail("OPENROUTER_API_KEY")
	case "gemini":
		if os.Getenv("GEMINI_API_KEY") != "" || os.Getenv("GOOGLE_API_KEY") != "" {
			return "Ready", pickerToneDefault
		}
		return "Missing • set GEMINI_API_KEY or GOOGLE_API_KEY", pickerToneWarn
	case "ollama":
		return "Local", pickerToneDefault
	default:
		return "", pickerToneDefault
	}
}

func providerSortRank(provider string) int {
	isLocal := strings.EqualFold(strings.TrimSpace(provider), "ollama")
	isSet := providerCredentialSet(provider)
	switch {
	case isSet && !isLocal:
		return 0
	case isSet && isLocal:
		return 1
	case !isSet && !isLocal:
		return 2
	default:
		return 3
	}
}

func providerCredentialSet(provider string) bool {
	switch strings.ToLower(strings.TrimSpace(provider)) {
	case "anthropic":
		return hasEnv("ANTHROPIC_API_KEY")
	case "openai":
		return hasEnv("OPENAI_API_KEY")
	case "openrouter":
		return hasEnv("OPENROUTER_API_KEY")
	case "gemini":
		return hasEnv("GEMINI_API_KEY") || hasEnv("GOOGLE_API_KEY")
	case "ollama":
		return true
	default:
		return false
	}
}

func keyDetail(env string) (string, pickerTone) {
	if hasEnv(env) {
		return "Ready", pickerToneDefault
	}
	return "Missing • set " + env, pickerToneWarn
}

func hasEnv(name string) bool {
	return strings.TrimSpace(os.Getenv(name)) != ""
}

func modelMetrics(meta registry.ModelMetadata) *pickerMetrics {
	metrics := &pickerMetrics{}
	if meta.ContextLimit > 0 {
		if meta.ContextLimit >= 1000 {
			metrics.Context = fmt.Sprintf("%dk", meta.ContextLimit/1000)
		} else {
			metrics.Context = fmt.Sprintf("%d", meta.ContextLimit)
		}
	}
	if meta.InputPrice > 0 {
		metrics.Input = fmt.Sprintf("$%.2f", meta.InputPrice)
	}
	if meta.OutputPrice > 0 {
		metrics.Output = fmt.Sprintf("$%.2f", meta.OutputPrice)
	}
	if metrics.Context == "" && metrics.Input == "" && metrics.Output == "" {
		return nil
	}
	return metrics
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
			line += " • " + item.Detail
		}
		b.WriteString(line)
		b.WriteString("\n")
	}
	b.WriteString("Esc cancel • Enter select")
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
		filtered := rankedPickerItems(m.picker.items, query)
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

type pickerSearchField struct {
	value  string
	weight int
}

type rankedPickerItem struct {
	item  pickerItem
	score int
	index int
}

func rankedPickerItems(items []pickerItem, query string) []pickerItem {
	ranked := make([]rankedPickerItem, 0, len(items))
	for i, item := range items {
		score, ok := pickerSearchScore(query, pickerSearchFields(item)...)
		if !ok {
			continue
		}
		ranked = append(ranked, rankedPickerItem{
			item:  item,
			score: score,
			index: i,
		})
	}
	slices.SortFunc(ranked, func(a, b rankedPickerItem) int {
		if a.score != b.score {
			return a.score - b.score
		}
		if cmp := strings.Compare(strings.ToLower(a.item.Label), strings.ToLower(b.item.Label)); cmp != 0 {
			return cmp
		}
		if cmp := strings.Compare(strings.ToLower(a.item.Value), strings.ToLower(b.item.Value)); cmp != 0 {
			return cmp
		}
		return a.index - b.index
	})
	filtered := make([]pickerItem, 0, len(ranked))
	for _, item := range ranked {
		filtered = append(filtered, item.item)
	}
	return filtered
}

func pickerSearchFields(item pickerItem) []pickerSearchField {
	if len(item.Search) > 0 {
		return item.Search
	}
	fields := []pickerSearchField{
		{value: normalizeSearchQuery(item.Label), weight: 0},
		{value: normalizeSearchQuery(item.Value), weight: 5},
		{value: normalizeSearchQuery(item.Detail), weight: 10},
		{value: normalizeSearchQuery(item.Group), weight: 20},
	}
	if item.Metrics != nil {
		fields = append(fields,
			pickerSearchField{value: normalizeSearchQuery(item.Metrics.Context), weight: 30},
			pickerSearchField{value: normalizeSearchQuery(item.Metrics.Input), weight: 31},
			pickerSearchField{value: normalizeSearchQuery(item.Metrics.Output), weight: 32},
		)
	}
	return fields
}

func pickerSearchScore(query string, fields ...pickerSearchField) (int, bool) {
	q := normalizeSearchQuery(query)
	if q == "" {
		return 0, true
	}

	best := int(^uint(0) >> 1)
	matched := false
	for _, field := range fields {
		score, ok := searchFieldScore(q, field.value)
		if !ok {
			continue
		}
		score += field.weight
		if score < best {
			best = score
			matched = true
		}
	}
	return best, matched
}

func searchFieldScore(query, candidate string) (int, bool) {
	if query == "" {
		return 0, true
	}
	if candidate == "" {
		return 0, false
	}
	switch {
	case candidate == query:
		return 0, true
	case strings.HasPrefix(candidate, query):
		return 100 + len(candidate) - len(query), true
	case strings.Contains(candidate, query):
		idx := strings.Index(candidate, query)
		return 200 + idx*2 + len(candidate) - len(query), true
	default:
		if score, ok := subsequenceScore(query, candidate); ok {
			return 300 + score, true
		}
		return 0, false
	}
}

func pickerSearchIndex(label, value, detail, group string, metrics *pickerMetrics) []pickerSearchField {
	fields := []pickerSearchField{
		{value: normalizeSearchQuery(label), weight: 0},
		{value: normalizeSearchQuery(value), weight: 5},
		{value: normalizeSearchQuery(detail), weight: 10},
		{value: normalizeSearchQuery(group), weight: 20},
	}
	if metrics != nil {
		fields = append(fields,
			pickerSearchField{value: normalizeSearchQuery(metrics.Context), weight: 30},
			pickerSearchField{value: normalizeSearchQuery(metrics.Input), weight: 31},
			pickerSearchField{value: normalizeSearchQuery(metrics.Output), weight: 32},
		)
	}
	return fields
}

func subsequenceScore(query, candidate string) (int, bool) {
	idx := 0
	gaps := 0
	for _, r := range query {
		next := strings.IndexRune(candidate[idx:], r)
		if next < 0 {
			return 0, false
		}
		gaps += next
		idx += next + utf8.RuneLen(r)
	}
	return gaps*4 + len(candidate) - idx, true
}

func normalizeSearchQuery(s string) string {
	return strings.ToLower(strings.TrimSpace(s))
}
