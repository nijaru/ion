package app

import (
	"github.com/nijaru/ion/config"
	"context"
	"time"
	"github.com/nijaru/ion/internal/runtime"
	"github.com/nijaru/ion/session"
	"fmt"
	"slices"
	"strings"
	"unicode"
	"unicode/utf8"

	"github.com/nijaru/ion/llm"
)

var (
	listModels            = llm.ListModels
	listModelsForConfig   = llm.ListModelsForConfig
	cachedModelsForConfig = llm.CachedModelsForConfig
)

const pickerPageSize = 8

func providerItems(cfg *config.Config) []pickerItem {
	items := make([]pickerItem, 0, len(llm.Native()))
	for _, def := range llm.Native() {
		if !llm.ShowInPicker(cfg, def) {
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
	if llm.IsOpenAICompatible(value) {
		return ""
	}
	return llm.DisplayName(value)
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

func modelItemsFromMetadata(metas []llm.ModelMetadata) []pickerItem {
	metas = append([]llm.ModelMetadata(nil), metas...)
	slices.SortFunc(metas, func(a, b llm.ModelMetadata) int {
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
	def, _ := llm.Lookup(value)
	return buildProviderItem(nil, def)
}

func buildProviderItem(cfg *config.Config, def llm.Definition) pickerItem {
	detail, tone, ready := providerDetail(cfg, def)
	label, detail := providerItemLabelAndDetail(cfg, def, detail)
	group := llm.GroupName(def)
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
	def llm.Definition,
	detail string,
) (string, string) {
	if def.ID != llm.OpenAICompatibleID {
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
		if endpoint := llm.EndpointDisplayName(cfg.Endpoint); endpoint != "" {
			return endpoint
		}
	}
	if endpoint, ok := strings.CutPrefix(detail, "Ready at "); ok {
		return strings.TrimSpace(endpoint)
	}
	return ""
}

func providerDetail(cfg *config.Config, def llm.Definition) (string, pickerTone, bool) {
	if def.ID == llm.OpenAICompatibleID {
		return openAICompatibleProviderDetail(cfg, def)
	}
	detail, ready := llm.CredentialStateContext(
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
	def llm.Definition,
) (string, pickerTone, bool) {
	providerCfg := cfgForProvider(cfg, def.ID)
	if llm.RequiresAuth(providerCfg, def) &&
		llm.ResolvedAuthToken(providerCfg, def) == "" {
		return fmt.Sprintf("Set %s", llm.MissingAuthDetail(providerCfg, def)),
			pickerToneWarn,
			false
	}
	if endpoint, ready, ok := llm.CachedLocalAPIState(providerCfg); ok {
		if ready {
			return "Ready at " + llm.EndpointDisplayName(endpoint), pickerToneDefault, true
		}
		return "Not running", pickerToneDefault, false
	}
	if strings.TrimSpace(providerCfg.Endpoint) != "" {
		return "Configured", pickerToneDefault, false
	}
	return "Set endpoint", pickerToneWarn, false
}

func providerSortRank(cfg *config.Config, provider string) int {
	def, ok := llm.Lookup(provider)
	if !ok {
		return 99
	}
	_, _, ready := providerDetail(cfg, def)
	isLocal := def.Kind == llm.KindLocal || def.ID == llm.OpenAICompatibleID
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
	case llm.KindDirect:
		return 3
	case llm.KindRouter:
		return 4
	case llm.KindCustom:
		return 5
	default:
		return rank
	}
}

func providerCredentialSet(provider string) bool {
	def, ok := llm.Lookup(provider)
	if !ok {
		return false
	}
	_, ready := llm.CredentialStateContext(
		context.Background(),
		cfgForProvider(nil, def.ID),
		def,
	)
	return ready
}

func modelMetrics(meta llm.ModelMetadata) *pickerMetrics {
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
	activeProvider := llm.ResolveID(copy.Provider)
	targetProvider := llm.ResolveID(provider)
	copy.Provider = targetProvider
	if activeProvider != targetProvider && !llm.IsOpenAICompatible(targetProvider) {
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

type pickerReducer struct {
	picker *PickerState
}

func (m *Model) pickerReducer() pickerReducer {
	return pickerReducer{picker: &m.Picker}
}

func (r pickerReducer) showSessionUnavailable() {
	r.picker.Overlay = nil
	r.picker.Session = &sessionPickerState{err: "session store not available"}
}

func (r pickerReducer) openOverlay(state pickerOverlayState) {
	r.picker.Overlay = &state
}

func (r pickerReducer) openOverlayInvalidatingModelLoads(state pickerOverlayState) {
	r.picker.ModelLoadRequest++
	r.openOverlay(state)
}

func (r pickerReducer) beginModelOverlayLoad(state pickerOverlayState) uint64 {
	r.picker.ModelLoadRequest++
	requestID := r.picker.ModelLoadRequest
	state.request = requestID
	r.openOverlay(state)
	return requestID
}

func (r pickerReducer) modelSetupRequestMatches(requestID uint64) bool {
	overlay := r.picker.Overlay
	if overlay == nil ||
		overlay.purpose != pickerPurposeModel ||
		!overlay.setup ||
		overlay.request != requestID ||
		requestID != r.picker.ModelLoadRequest {
		return false
	}
	return true
}

func (r pickerReducer) failModelSetup(requestID uint64, message string) bool {
	if !r.modelSetupRequestMatches(requestID) {
		return false
	}
	r.picker.Overlay.loading = false
	r.picker.Overlay.setup = false
	r.picker.Overlay.err = message
	return true
}

func (r pickerReducer) modelLoadRequestMatches(requestID uint64) bool {
	overlay := r.picker.Overlay
	if overlay == nil ||
		overlay.purpose != pickerPurposeModel ||
		overlay.request != requestID ||
		requestID != r.picker.ModelLoadRequest {
		return false
	}
	return true
}

func (r pickerReducer) failModelLoad(requestID uint64, message string) bool {
	if !r.modelLoadRequestMatches(requestID) {
		return false
	}
	r.picker.Overlay.loading = false
	r.picker.Overlay.err = message
	if len(r.picker.Overlay.items) == 0 {
		r.picker.Overlay.filtered = nil
	}
	return true
}

func (r pickerReducer) completeModelLoad(
	requestID uint64,
	items []pickerItem,
	selectedValue string,
) bool {
	if !r.modelLoadRequestMatches(requestID) {
		return false
	}
	r.picker.Overlay.loading = false
	r.picker.Overlay.err = ""
	r.picker.Overlay.items = items
	r.picker.Overlay.filtered = clonePickerItems(items)
	r.picker.Overlay.index = pickerIndex(items, selectedValue)
	r.refreshOverlayFilter()
	return true
}

func (r pickerReducer) closeOverlay() {
	r.picker.Overlay = nil
	r.picker.OverlayClosedAt = time.Now()
}

func (r pickerReducer) closeAll() {
	r.picker.Overlay = nil
	r.picker.Session = nil
	r.picker.Setup = nil
	r.picker.OverlayClosedAt = time.Now()
}

func (r pickerReducer) openSetup(state setupPromptState) {
	r.picker.Overlay = nil
	r.picker.Setup = &state
}

func (r pickerReducer) closeSetup() {
	r.picker.Setup = nil
}

func (r pickerReducer) appendSetupValue(text string) {
	if r.picker.Setup == nil || text == "" {
		return
	}
	r.picker.Setup.value += text
	r.picker.Setup.err = ""
}

func (r pickerReducer) backspaceSetupValue() {
	if r.picker.Setup == nil || r.picker.Setup.value == "" {
		return
	}
	_, size := utf8.DecodeLastRuneInString(r.picker.Setup.value)
	r.picker.Setup.value = r.picker.Setup.value[:len(r.picker.Setup.value)-size]
}

func (r pickerReducer) setSetupError(message string) {
	if r.picker.Setup != nil {
		r.picker.Setup.err = message
	}
}

func (r pickerReducer) beginSetupSave() (uint64, bool) {
	if r.picker.Setup == nil {
		return 0, false
	}
	r.picker.SetupSaveRequest++
	requestID := r.picker.SetupSaveRequest
	r.picker.Setup.saving = true
	r.picker.Setup.request = requestID
	r.picker.Setup.err = ""
	return requestID, true
}

func (r pickerReducer) failSetupSave(requestID uint64, message string) bool {
	if !r.setupSaveMatches(requestID) {
		return false
	}
	r.picker.SetupSaveRequest = 0
	r.picker.Setup.saving = false
	r.picker.Setup.request = 0
	r.picker.Setup.err = message
	return true
}

func (r pickerReducer) completeSetupSave(requestID uint64) bool {
	if !r.setupSaveMatches(requestID) {
		return false
	}
	r.picker.SetupSaveRequest = 0
	r.picker.Setup = nil
	return true
}

func (r pickerReducer) setupSaveMatches(requestID uint64) bool {
	return requestID != 0 &&
		requestID == r.picker.SetupSaveRequest &&
		r.picker.Setup != nil &&
		r.picker.Setup.request == requestID
}

func (r pickerReducer) appendOverlayQuery(text string) {
	if r.picker.Overlay == nil || text == "" {
		return
	}
	r.picker.Overlay.query += text
	r.refreshOverlayFilter()
}

func (r pickerReducer) setOverlayQuery(query string) {
	if r.picker.Overlay == nil {
		return
	}
	r.picker.Overlay.query = query
	r.refreshOverlayFilter()
}

func (r pickerReducer) backspaceOverlayQuery() {
	if r.picker.Overlay == nil || r.picker.Overlay.query == "" {
		return
	}
	_, size := utf8.DecodeLastRuneInString(r.picker.Overlay.query)
	r.picker.Overlay.query = r.picker.Overlay.query[:len(r.picker.Overlay.query)-size]
	r.refreshOverlayFilter()
}

func (r pickerReducer) moveOverlaySelection(delta int) {
	if r.picker.Overlay == nil {
		return
	}
	items := pickerDisplayItems(r.picker.Overlay)
	if len(items) == 0 {
		return
	}
	next := r.picker.Overlay.index + delta
	if next < 0 {
		next = 0
	}
	if max := len(items) - 1; next > max {
		next = max
	}
	r.picker.Overlay.index = next
}

func (r pickerReducer) pageOverlaySelection(delta int) {
	r.moveOverlaySelection(delta * pickerPageSize)
}

func (r pickerReducer) refreshOverlayFilter() {
	if r.picker.Overlay == nil {
		return
	}
	query := strings.TrimSpace(r.picker.Overlay.query)
	if query == "" {
		r.picker.Overlay.filtered = append([]pickerItem(nil), r.picker.Overlay.items...)
		if len(r.picker.Overlay.filtered) == 0 {
			r.picker.Overlay.index = 0
			return
		}
		if r.picker.Overlay.index >= len(r.picker.Overlay.filtered) {
			r.picker.Overlay.index = len(r.picker.Overlay.filtered) - 1
		}
		return
	}
	r.picker.Overlay.filtered = rankedPickerItems(r.picker.Overlay.items, query)
	if len(r.picker.Overlay.filtered) == 0 {
		r.picker.Overlay.index = 0
		return
	}
	r.picker.Overlay.index = 0
}

func (r pickerReducer) beginSessionLoad() uint64 {
	r.picker.Overlay = nil
	r.picker.SessionLoadRequest++
	requestID := r.picker.SessionLoadRequest
	r.picker.Session = &sessionPickerState{
		loading: true,
		request: requestID,
	}
	return requestID
}

func (r pickerReducer) applySessionLoad(
	requestID uint64,
	sessions []session.SessionInfoEntry,
	err error,
) bool {
	if !r.sessionLoadMatches(requestID) {
		return false
	}
	if err != nil {
		r.picker.Session = &sessionPickerState{
			err: fmt.Sprintf("failed to list sessions: %v", err),
		}
		return true
	}
	items := make([]sessionPickerItem, 0, len(sessions))
	for _, info := range sessions {
		if !runtime.IsConversationSessionInfo(&info) {
			continue
		}
		items = append(items, sessionPickerItem{info: info})
	}

	state := &sessionPickerState{
		items:    items,
		filtered: append([]sessionPickerItem(nil), items...),
		index:    0,
	}
	if len(items) == 0 {
		state.err = "no recent sessions in this workspace"
	}
	r.picker.Session = state
	return true
}

func (r pickerReducer) sessionLoadMatches(requestID uint64) bool {
	return r.picker.Session != nil &&
		r.picker.Session.request != 0 &&
		r.picker.Session.request == requestID &&
		requestID == r.picker.SessionLoadRequest
}

func (r pickerReducer) closeSession() {
	r.picker.Session = nil
}

func (r pickerReducer) selectedSession() (session.SessionInfoEntry, bool) {
	if r.picker.Session == nil || len(r.picker.Session.filtered) == 0 {
		return session.SessionInfoEntry{}, false
	}
	index := r.picker.Session.index
	if index < 0 || index >= len(r.picker.Session.filtered) {
		return session.SessionInfoEntry{}, false
	}
	return r.picker.Session.filtered[index].info, true
}

func (r pickerReducer) appendSessionQuery(text, workdir string) {
	if r.picker.Session == nil || text == "" {
		return
	}
	r.picker.Session.query += text
	r.refreshSessionFilter(workdir)
}

func (r pickerReducer) backspaceSessionQuery(workdir string) {
	if r.picker.Session == nil || r.picker.Session.query == "" {
		return
	}
	_, size := utf8.DecodeLastRuneInString(r.picker.Session.query)
	r.picker.Session.query = r.picker.Session.query[:len(r.picker.Session.query)-size]
	r.refreshSessionFilter(workdir)
}

func (r pickerReducer) moveSessionSelection(delta int) {
	if r.picker.Session == nil || len(r.picker.Session.filtered) == 0 {
		return
	}
	next := r.picker.Session.index + delta
	if next < 0 {
		next = 0
	}
	if max := len(r.picker.Session.filtered) - 1; next > max {
		next = max
	}
	r.picker.Session.index = next
}

func (r pickerReducer) pageSessionSelection(delta int) {
	r.moveSessionSelection(delta * pickerPageSize)
}

func (r pickerReducer) refreshSessionFilter(workdir string) {
	if r.picker.Session == nil {
		return
	}
	query := strings.TrimSpace(r.picker.Session.query)

	// Start with all items or named-only items
	var baseItems []sessionPickerItem
	if r.picker.Session.namedOnly {
		baseItems = make([]sessionPickerItem, 0)
		for _, item := range r.picker.Session.items {
			if item.info.Title() != "" {
				baseItems = append(baseItems, item)
			}
		}
	} else {
		baseItems = r.picker.Session.items
	}

	if query == "" {
		r.picker.Session.filtered = append(
			[]sessionPickerItem(nil),
			baseItems...,
		)
		if len(r.picker.Session.filtered) == 0 {
			r.picker.Session.index = 0
			return
		}
		if r.picker.Session.index >= len(r.picker.Session.filtered) {
			r.picker.Session.index = len(r.picker.Session.filtered) - 1
		}
		return
	}
	r.picker.Session.filtered = rankedSessionPickerItems(
		baseItems,
		query,
		workdir,
	)
	if len(r.picker.Session.filtered) == 0 {
		r.picker.Session.index = 0
		return
	}
	r.picker.Session.index = 0
}
