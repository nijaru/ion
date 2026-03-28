package app

import (
	"context"
	"fmt"
	"slices"
	"strings"
	"unicode/utf8"

	"github.com/nijaru/ion/internal/backend/registry"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/providers"
)

var listModels = registry.ListModels
var listModelsForConfig = registry.ListModelsForConfig

func providerItems(cfg *config.Config) []pickerItem {
	items := make([]pickerItem, 0, len(providers.Native()))
	for _, def := range providers.Native() {
		if !providers.ShowInPicker(cfg, def) {
			continue
		}
		items = append(items, buildProviderItem(cfg, def))
	}
	slices.SortFunc(items, func(a, b pickerItem) int {
		if rankA, rankB := providerSortRank(cfg, a.Value), providerSortRank(cfg, b.Value); rankA != rankB {
			return rankA - rankB
		}
		if cmp := strings.Compare(a.Group, b.Group); cmp != 0 {
			return cmp
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
	return providers.DisplayName(value)
}

func modelItemsForProvider(cfg *config.Config) ([]pickerItem, error) {
	models, err := listModelsForConfig(context.Background(), cfg)
	if err != nil {
		return nil, err
	}
	slices.SortFunc(models, func(a, b registry.ModelMetadata) int {
		if orgA, orgB := modelOrg(a.ID), modelOrg(b.ID); orgA != orgB {
			return strings.Compare(orgA, orgB)
		}
		if a.Created != b.Created {
			if a.Created > b.Created {
				return -1
			}
			return 1
		}
		return strings.Compare(strings.ToLower(a.ID), strings.ToLower(b.ID))
	})

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
	return items, nil
}

func modelOrg(id string) string {
	left, _, ok := strings.Cut(strings.ToLower(strings.TrimSpace(id)), "/")
	if !ok {
		return ""
	}
	return left
}

func providerItem(label, value string) pickerItem {
	def, _ := providers.Lookup(value)
	return buildProviderItem(nil, def)
}

func buildProviderItem(cfg *config.Config, def providers.Definition) pickerItem {
	detail, tone := providerDetail(cfg, def)
	return pickerItem{
		Label:  def.DisplayName,
		Value:  def.ID,
		Detail: detail,
		Group:  providers.GroupName(def),
		Tone:   tone,
		Search: pickerSearchIndex(def.DisplayName, def.ID, detail, providers.GroupName(def), nil),
	}
}

func providerDetail(cfg *config.Config, def providers.Definition) (string, pickerTone) {
	detail, ready := providers.CredentialStateContext(context.Background(), cfgForProvider(cfg, def.ID), def)
	if ready || !strings.HasPrefix(detail, "Set ") {
		return detail, pickerToneDefault
	}
	return detail, pickerToneWarn
}

func providerSortRank(cfg *config.Config, provider string) int {
	def, ok := providers.Lookup(provider)
	if !ok {
		return 99
	}
	return providers.SortRank(cfgForProvider(cfg, def.ID), def)
}

func providerCredentialSet(provider string) bool {
	def, ok := providers.Lookup(provider)
	if !ok {
		return false
	}
	_, ready := providers.CredentialStateContext(context.Background(), cfgForProvider(nil, def.ID), def)
	return ready
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

func cfgForProvider(cfg *config.Config, provider string) *config.Config {
	if cfg == nil {
		return &config.Config{Provider: provider}
	}
	copy := *cfg
	copy.Provider = provider
	return &copy
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
