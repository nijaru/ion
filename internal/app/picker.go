package app

import (
	"context"
	"fmt"
	"slices"
	"strings"
	"unicode"
	"unicode/utf8"

	"github.com/nijaru/ion/internal/models"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/providers"
)

var (
	listModels            = models.ListModels
	listModelsForConfig   = models.ListModelsForConfig
	cachedModelsForConfig = models.CachedModelsForConfig
)

const pickerPageSize = 8

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

func pickerItemByValue(items []pickerItem, value string) (pickerItem, bool) {
	for _, item := range items {
		if strings.EqualFold(item.Value, value) || strings.EqualFold(item.Label, value) {
			return item, true
		}
	}
	return pickerItem{}, false
}

func providerDisplayName(value string) string {
	return providers.DisplayName(value)
}

func modelItemsForProvider(ctx context.Context, cfg *config.Config) ([]pickerItem, error) {
	models, err := listModelsForConfig(ctx, cfg)
	if err != nil {
		return nil, err
	}
	return modelItemsFromMetadata(models), nil
}

func cachedModelItemsForProvider(cfg *config.Config) ([]pickerItem, bool, bool) {
	models, fresh, ok := cachedModelsForConfig(cfg)
	if !ok {
		return nil, false, false
	}
	return modelItemsFromMetadata(models), fresh, true
}

func modelItemsFromMetadata(metas []models.ModelMetadata) []pickerItem {
	metas = append([]models.ModelMetadata(nil), metas...)
	slices.SortFunc(metas, func(a, b models.ModelMetadata) int {
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
	for _, model := range metas {
		metrics := modelMetrics(model)
		search := pickerSearchIndex(model.ID, model.ID, "", "", metrics)
		if model.InputPriceKnown && model.OutputPriceKnown && model.InputPrice == 0 &&
			model.OutputPrice == 0 {
			search = append(search, pickerSearchField{value: "free", weight: 12})
		}
		items = append(items, pickerItem{
			Label:   model.ID,
			Value:   model.ID,
			Metrics: metrics,
			Search:  search,
		})
	}
	return items
}

func clonePickerItems(items []pickerItem) []pickerItem {
	return append([]pickerItem(nil), items...)
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
	detail, tone, ready := providerDetail(cfg, def)
	label, detail := providerItemLabelAndDetail(cfg, def, detail)
	group := providers.GroupName(def)
	if !ready && strings.HasPrefix(detail, "Set ") {
		group = "Needs setup"
	}
	return pickerItem{
		Label:       label,
		Value:       def.ID,
		Detail:      detail,
		Group:       group,
		Tone:        tone,
		SettingName: label,
		CurrentVal:  detail,
		Desc:        group,
		Search:      pickerSearchIndex(label, def.ID, detail+" "+def.DisplayName, group, nil),
	}
}

func providerItemLabelAndDetail(
	cfg *config.Config,
	def providers.Definition,
	detail string,
) (string, string) {
	if def.ID != providers.OpenAICompatibleID {
		return def.DisplayName, detail
	}

	endpoint := providerItemEndpointDisplay(cfg, detail)
	if endpoint == "" {
		return def.DisplayName, detail
	}

	status := detail
	if strings.HasPrefix(status, "Ready at ") {
		status = "Ready"
	}
	if status == "" {
		return endpoint, def.DisplayName
	}
	return endpoint, def.DisplayName + " • " + status
}

func providerItemEndpointDisplay(cfg *config.Config, detail string) string {
	if cfg != nil {
		if endpoint := providers.EndpointDisplayName(cfg.Endpoint); endpoint != "" {
			return endpoint
		}
	}
	if endpoint, ok := strings.CutPrefix(detail, "Ready at "); ok {
		return strings.TrimSpace(endpoint)
	}
	return ""
}

func providerDetail(cfg *config.Config, def providers.Definition) (string, pickerTone, bool) {
	if def.ID == providers.OpenAICompatibleID {
		return openAICompatibleProviderDetail(cfg, def)
	}
	detail, ready := providers.CredentialStateContext(
		context.Background(),
		cfgForProvider(cfg, def.ID),
		def,
	)
	if ready || !strings.HasPrefix(detail, "Set ") {
		return detail, pickerToneDefault, ready
	}
	return detail, pickerToneWarn, ready
}

func openAICompatibleProviderDetail(
	cfg *config.Config,
	def providers.Definition,
) (string, pickerTone, bool) {
	providerCfg := cfgForProvider(cfg, def.ID)
	if providers.RequiresAuth(providerCfg, def) &&
		providers.ResolvedAuthToken(providerCfg, def) == "" {
		return fmt.Sprintf("Set %s", providers.MissingAuthDetail(providerCfg, def)),
			pickerToneWarn,
			false
	}
	if endpoint, ready, ok := providers.CachedLocalAPIState(providerCfg); ok {
		if ready {
			return "Ready at " + providers.EndpointDisplayName(endpoint), pickerToneDefault, true
		}
		return "Not running", pickerToneDefault, false
	}
	if strings.TrimSpace(providerCfg.Endpoint) != "" {
		return "Configured", pickerToneDefault, false
	}
	return "Set endpoint", pickerToneWarn, false
}

func providerSortRank(cfg *config.Config, provider string) int {
	def, ok := providers.Lookup(provider)
	if !ok {
		return 99
	}
	_, _, ready := providerDetail(cfg, def)
	isLocal := def.Kind == providers.KindLocal || def.ID == providers.OpenAICompatibleID
	rank := 3
	switch {
	case ready && !isLocal:
		rank = 0
	case ready && isLocal:
		rank = 1
	case !ready && isLocal:
		rank = 2
	}
	if rank != 3 {
		return rank
	}
	switch def.Kind {
	case providers.KindDirect:
		return 3
	case providers.KindRouter:
		return 4
	case providers.KindCustom:
		return 5
	default:
		return rank
	}
}

func providerCredentialSet(provider string) bool {
	def, ok := providers.Lookup(provider)
	if !ok {
		return false
	}
	_, ready := providers.CredentialStateContext(
		context.Background(),
		cfgForProvider(nil, def.ID),
		def,
	)
	return ready
}

func modelMetrics(meta models.ModelMetadata) *pickerMetrics {
	metrics := &pickerMetrics{}
	if meta.ContextLimit > 0 {
		if meta.ContextLimit >= 1000 {
			metrics.Context = fmt.Sprintf("%dk", meta.ContextLimit/1000)
		} else {
			metrics.Context = fmt.Sprintf("%d", meta.ContextLimit)
		}
	}
	if meta.InputPriceKnown {
		if meta.InputPrice == 0 {
			metrics.Input = "Free"
		} else {
			metrics.Input = fmt.Sprintf("$%.2f", meta.InputPrice)
		}
	}
	if meta.OutputPriceKnown {
		if meta.OutputPrice == 0 {
			metrics.Output = "Free"
		} else {
			metrics.Output = fmt.Sprintf("$%.2f", meta.OutputPrice)
		}
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
	activeProvider := providers.ResolveID(copy.Provider)
	targetProvider := providers.ResolveID(provider)
	copy.Provider = targetProvider
	if activeProvider != targetProvider && !providers.IsOpenAICompatible(targetProvider) {
		copy.Endpoint = ""
		copy.AuthEnvVar = ""
		copy.ExtraHeaders = nil
	}
	return &copy
}

func refreshPickerFilter(m *Model) {
	m.pickerReducer().refreshOverlayFilter()
}

func pickerDisplayItems(p *pickerOverlayState) []pickerItem {
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
	item     pickerItem
	score    int
	index    int
	labelKey string
	valueKey string
}

func rankedPickerItems(items []pickerItem, query string) []pickerItem {
	search := preparePickerSearchQuery(query)
	ranked := make([]rankedPickerItem, 0, len(items))
	for i, item := range items {
		score, ok := pickerSearchScorePrepared(search, pickerSearchFields(item))
		if !ok {
			continue
		}
		ranked = append(ranked, rankedPickerItem{
			item:     item,
			score:    score,
			index:    i,
			labelKey: strings.ToLower(item.Label),
			valueKey: strings.ToLower(item.Value),
		})
	}
	slices.SortFunc(ranked, func(a, b rankedPickerItem) int {
		if a.score != b.score {
			return a.score - b.score
		}
		if cmp := strings.Compare(a.labelKey, b.labelKey); cmp != 0 {
			return cmp
		}
		if cmp := strings.Compare(a.valueKey, b.valueKey); cmp != 0 {
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
		fields = append(
			fields,
			pickerSearchField{value: normalizeSearchQuery(item.Metrics.Context), weight: 30},
			pickerSearchField{value: normalizeSearchQuery(item.Metrics.Input), weight: 31},
			pickerSearchField{value: normalizeSearchQuery(item.Metrics.Output), weight: 32},
		)
	}
	return fields
}

type pickerSearchQuery struct {
	value  string
	tokens []string
}

func preparePickerSearchQuery(query string) pickerSearchQuery {
	q := normalizeSearchQuery(query)
	if q == "" {
		return pickerSearchQuery{}
	}
	tokens := strings.Fields(q)
	if len(tokens) <= 1 {
		tokens = nil
	}
	return pickerSearchQuery{value: q, tokens: tokens}
}

func pickerSearchScore(query string, fields ...pickerSearchField) (int, bool) {
	return pickerSearchScorePrepared(preparePickerSearchQuery(query), fields)
}

func pickerSearchScorePrepared(query pickerSearchQuery, fields []pickerSearchField) (int, bool) {
	if query.value == "" {
		return 0, true
	}
	if len(query.tokens) > 1 {
		return multiTokenPickerSearchScore(query.tokens, fields)
	}

	best := int(^uint(0) >> 1)
	matched := false
	for _, field := range fields {
		score, ok := searchFieldScore(query.value, field.value)
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

func multiTokenPickerSearchScore(tokens []string, fields []pickerSearchField) (int, bool) {
	total := 0
	for _, token := range tokens {
		best := int(^uint(0) >> 1)
		matched := false
		for _, field := range fields {
			score, ok := searchFieldScore(token, field.value)
			if !ok {
				continue
			}
			score += field.weight
			if score < best {
				best = score
				matched = true
			}
		}
		if !matched {
			return 0, false
		}
		total += best
	}
	return total, true
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
		if score, ok := tokenSearchScore(query, candidate); ok {
			return 260 + score, true
		}
		if utf8.RuneCountInString(query) <= 3 {
			if score, ok := subsequenceScore(query, candidate); ok {
				return 320 + score, true
			}
		}
		return 0, false
	}
}

func pickerSearchIndex(
	label, value, detail, group string,
	metrics *pickerMetrics,
) []pickerSearchField {
	fields := []pickerSearchField{
		{value: normalizeSearchQuery(label), weight: 0},
		{value: normalizeSearchQuery(value), weight: 5},
		{value: normalizeSearchQuery(detail), weight: 10},
		{value: normalizeSearchQuery(group), weight: 20},
	}
	if metrics != nil {
		fields = append(
			fields,
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

func tokenSearchScore(query, candidate string) (int, bool) {
	tokens := splitSearchTokens(candidate)
	if len(tokens) == 0 {
		return 0, false
	}

	best := int(^uint(0) >> 1)
	matched := false
	for idx, token := range tokens {
		if token == "" {
			continue
		}
		switch {
		case token == query:
			if score := idx * 2; score < best {
				best = score
				matched = true
			}
		case strings.HasPrefix(token, query):
			if score := 20 + idx*2 + len(token) - len(query); score < best {
				best = score
				matched = true
			}
		case strings.Contains(token, query):
			pos := strings.Index(token, query)
			if score := 40 + idx*2 + pos + len(token) - len(query); score < best {
				best = score
				matched = true
			}
		}
	}

	compactQuery := compactSearchToken(query)
	if compactQuery == "" || compactQuery == query {
		return best, matched
	}
	for idx, token := range tokens {
		compactToken := compactSearchToken(token)
		if compactToken == "" {
			continue
		}
		switch {
		case compactToken == compactQuery:
			if score := 60 + idx*2; score < best {
				best = score
				matched = true
			}
		case strings.HasPrefix(compactToken, compactQuery):
			if score := 80 + idx*2 + len(compactToken) - len(compactQuery); score < best {
				best = score
				matched = true
			}
		case strings.Contains(compactToken, compactQuery):
			pos := strings.Index(compactToken, compactQuery)
			if score := 100 + idx*2 + pos + len(compactToken) - len(compactQuery); score < best {
				best = score
				matched = true
			}
		}
	}
	return best, matched
}

func splitSearchTokens(s string) []string {
	return strings.FieldsFunc(s, func(r rune) bool {
		return !unicode.IsLetter(r) && !unicode.IsDigit(r)
	})
}

func compactSearchToken(s string) string {
	var b strings.Builder
	b.Grow(len(s))
	for _, r := range s {
		if unicode.IsLetter(r) || unicode.IsDigit(r) {
			b.WriteRune(r)
		}
	}
	return b.String()
}
