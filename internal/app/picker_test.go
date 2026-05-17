package app

import (
	"context"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/nijaru/ion/internal/backend/registry"
	"github.com/nijaru/ion/internal/config"
	"github.com/nijaru/ion/internal/credentials"
	"github.com/nijaru/ion/internal/providers"
	"github.com/nijaru/ion/internal/storage"
)

func TestProviderItemsSortSetAPIsThenLocalThenUnset(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	for _, name := range []string{
		"ANTHROPIC_API_KEY",
		"OPENAI_API_KEY",
		"OPENROUTER_API_KEY",
		"GEMINI_API_KEY",
		"GOOGLE_API_KEY",
		"HF_TOKEN",
		"TOGETHER_API_KEY",
		"DEEPSEEK_API_KEY",
		"GROQ_API_KEY",
		"FIREWORKS_API_KEY",
		"MISTRAL_API_KEY",
		"MOONSHOT_API_KEY",
		"CEREBRAS_API_KEY",
		"ZAI_API_KEY",
		"XAI_API_KEY",
		"OPENAI_COMPATIBLE_API_KEY",
	} {
		t.Setenv(name, "")
	}
	t.Setenv("OPENROUTER_API_KEY", "test")
	t.Setenv("GOOGLE_API_KEY", "test")
	items := providerItems(&config.Config{})
	got := make([]string, 0, len(items))
	for _, item := range items {
		got = append(got, item.Label)
	}
	want := []string{
		"Gemini",
		"OpenRouter",
		"Ollama",
		"OpenAI-compatible",
		"Anthropic",
		"Cerebras",
		"DeepSeek",
		"Fireworks AI",
		"Groq",
		"Mistral",
		"Moonshot AI",
		"OpenAI",
		"Z.ai",
		"xAI",
		"Hugging Face",
		"Together AI",
	}
	if !slices.Equal(got, want) {
		t.Fatalf("provider order = %#v, want %#v", got, want)
	}
}

func TestTabCompletesSlashCommands(t *testing.T) {
	model := readyModel(t)
	model.Input.Composer.SetValue("/think")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyTab})
	model = updated.(Model)
	if cmd != nil {
		t.Fatalf("unexpected autocomplete cmd %T", cmd)
	}
	if got := model.Input.Composer.Value(); got != "/thinking " {
		t.Fatalf("composer = %q, want /thinking autocomplete", got)
	}
}

func TestTabCompletesKnownSlashArguments(t *testing.T) {
	tests := []struct {
		name  string
		input string
		want  string
	}{
		{name: "thinking level", input: "/thinking hi", want: "/thinking high "},
		{name: "settings key", input: "/settings ret", want: "/settings retry "},
		{name: "settings retry value", input: "/settings retry of", want: "/settings retry off "},
		{
			name:  "settings tool value",
			input: "/settings tool co",
			want:  "/settings tool collapsed ",
		},
		{
			name:  "settings thinking value",
			input: "/settings thinking h",
			want:  "/settings thinking hidden ",
		},
		{
			name:  "settings busy value",
			input: "/settings busy st",
			want:  "/settings busy steer ",
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			model := readyModel(t)
			model.Input.Composer.SetValue(tc.input)

			updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyTab})
			model = updated.(Model)
			if cmd != nil {
				t.Fatalf("unexpected autocomplete cmd %T", cmd)
			}
			if got := model.Input.Composer.Value(); got != tc.want {
				t.Fatalf("composer = %q, want %q", got, tc.want)
			}
		})
	}
}

func TestTabListsAmbiguousSlashCommands(t *testing.T) {
	model := readyModel(t)
	model.Input.Composer.SetValue("/t")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyTab})
	model = updated.(Model)
	if cmd != nil {
		t.Fatalf("unexpected ambiguous autocomplete command %T", cmd)
	}
	if got := model.Input.Composer.Value(); got != "/t" {
		t.Fatalf("composer = %q, want unchanged ambiguous prefix", got)
	}
	if model.Picker.Overlay == nil {
		t.Fatal("expected slash command picker")
	}
	if model.Picker.Overlay.purpose != pickerPurposeCommand {
		t.Fatalf("picker purpose = %v, want command picker", model.Picker.Overlay.purpose)
	}
	if got := model.Picker.Overlay.query; got != "t" {
		t.Fatalf("picker query = %q, want t", got)
	}
	if len(pickerDisplayItems(model.Picker.Overlay)) < 2 {
		t.Fatalf(
			"ambiguous command picker items = %#v, want multiple matches",
			pickerDisplayItems(model.Picker.Overlay),
		)
	}
}

func TestTabIgnoresHiddenSlashCommandAliases(t *testing.T) {
	model := readyModel(t)
	model.Input.Composer.SetValue("/rea")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyTab})
	model = updated.(Model)
	if cmd != nil {
		t.Fatalf("unexpected autocomplete cmd %T", cmd)
	}
	if got := model.Input.Composer.Value(); got != "/rea" {
		t.Fatalf("composer = %q, want unchanged hidden alias prefix", got)
	}
	if model.Picker.Overlay != nil {
		t.Fatal("hidden alias should not open command picker")
	}
}

func TestCommandPickerInsertsSelectedCommand(t *testing.T) {
	model := readyModel(t)
	model = model.openCommandPicker("stat")

	updated, cmd := model.handlePickerKey(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = updated
	if cmd != nil {
		t.Fatalf("unexpected command picker cmd %T", cmd)
	}
	if got := model.Input.Composer.Value(); got != "/status " {
		t.Fatalf("composer = %q, want /status insertion", got)
	}
	if model.Picker.Overlay != nil {
		t.Fatal("expected command picker to close")
	}
}

func TestTabCompletesFileReference(t *testing.T) {
	workdir := t.TempDir()
	if err := os.WriteFile(filepath.Join(workdir, "README.md"), []byte("readme"), 0o644); err != nil {
		t.Fatalf("write README: %v", err)
	}
	model := readyModel(t)
	model.App.Workdir = workdir
	model.Input.Composer.SetValue("read @REA")

	updated, cmd := model.Update(tea.KeyPressMsg{Code: tea.KeyTab})
	model = updated.(Model)
	if cmd != nil {
		t.Fatalf("unexpected file completion cmd %T", cmd)
	}
	if got := model.Input.Composer.Value(); got != "read @README.md " {
		t.Fatalf("composer = %q, want completed file reference", got)
	}
}

func TestTabCompletesDirectoryReferenceWithoutTrailingSpace(t *testing.T) {
	workdir := t.TempDir()
	if err := os.Mkdir(filepath.Join(workdir, "internal"), 0o755); err != nil {
		t.Fatalf("mkdir internal: %v", err)
	}
	model := readyModel(t)
	model.App.Workdir = workdir
	model.Input.Composer.SetValue("@int")

	updated, _ := model.Update(tea.KeyPressMsg{Code: tea.KeyTab})
	model = updated.(Model)
	if got := model.Input.Composer.Value(); got != "@internal/" {
		t.Fatalf("composer = %q, want completed directory reference", got)
	}
}

func TestTabFileReferenceKeepsCommonPrefixForAmbiguousMatches(t *testing.T) {
	workdir := t.TempDir()
	for _, name := range []string{"README.md", "RELEASE.md"} {
		if err := os.WriteFile(filepath.Join(workdir, name), []byte(name), 0o644); err != nil {
			t.Fatalf("write %s: %v", name, err)
		}
	}
	model := readyModel(t)
	model.App.Workdir = workdir
	model.Input.Composer.SetValue("@RE")

	updated, _ := model.Update(tea.KeyPressMsg{Code: tea.KeyTab})
	model = updated.(Model)
	if got := model.Input.Composer.Value(); got != "@RE" {
		t.Fatalf("composer = %q, want unchanged ambiguous reference", got)
	}
}

func TestMatchingWorkspaceFileReferencesRejectsEscapes(t *testing.T) {
	workdir := t.TempDir()
	if matches := matchingWorkspaceFileReferences(workdir, "../"); len(matches) != 0 {
		t.Fatalf("matches = %#v, want none for workspace escape", matches)
	}
}

func TestSessionPickerLineShowsUsefulMetadata(t *testing.T) {
	info := storage.SessionInfo{
		ID:          "sess-1",
		Model:       "local-api/qwen3.6:27b",
		Branch:      "main",
		UpdatedAt:   time.Now().Add(-2 * time.Hour),
		Title:       "Fix resume",
		LastPreview: "resume follow-up worked",
	}

	label, detail := sessionPickerLine("/tmp/ion", info)
	if label != "Fix resume" {
		t.Fatalf("label = %q, want title", label)
	}
	for _, want := range []string{"resume follow-up worked", "local-api/qwen3.6:27b", "main", "2h ago"} {
		if !strings.Contains(detail, want) {
			t.Fatalf("detail = %q, want %q", detail, want)
		}
	}
}

func TestSessionPickerLineOmitsMissingAge(t *testing.T) {
	info := storage.SessionInfo{
		ID:          "sess-1",
		LastPreview: "hello",
	}

	label, detail := sessionPickerLine("/tmp/ion", info)
	if label != "hello" {
		t.Fatalf("label = %q, want preview", label)
	}
	if strings.Contains(detail, "ago") || strings.Contains(detail, "h0m0s") {
		t.Fatalf("detail = %q, want no age for zero timestamp", detail)
	}
}

func TestRankedSessionPickerItemsSearchesCaseInsensitively(t *testing.T) {
	items := []sessionPickerItem{
		{info: storage.SessionInfo{
			ID:          "sess-1",
			Title:       "Fix Resume Flow",
			Summary:     "Workspace history",
			LastPreview: "Recovered stalled session",
		}},
		{info: storage.SessionInfo{
			ID:          "sess-2",
			Title:       "Tool output cleanup",
			Summary:     "Background jobs",
			LastPreview: "Bounded output",
		}},
	}

	filtered := rankedSessionPickerItems(items, "resume", "/tmp/ion")
	if len(filtered) != 1 || filtered[0].info.ID != "sess-1" {
		t.Fatalf("filtered = %#v, want only sess-1", filtered)
	}

	filtered = rankedSessionPickerItems(items, "RECOVERED", "/tmp/ion")
	if len(filtered) != 1 || filtered[0].info.ID != "sess-1" {
		t.Fatalf("filtered by preview = %#v, want only sess-1", filtered)
	}
}

func TestSessionPickerFilteringSelectsTopRankedMatch(t *testing.T) {
	model := readyModel(t)
	model.Picker.Session = &sessionPickerState{
		items: []sessionPickerItem{
			{info: storage.SessionInfo{ID: "sess-1", Title: "zz resume"}},
			{info: storage.SessionInfo{ID: "sess-2", Title: "resume"}},
		},
		index: 1,
	}
	model.Picker.Session.filtered = model.Picker.Session.items

	for _, r := range []rune("resume") {
		model, _ = model.handleSessionPickerKey(tea.KeyPressMsg{Text: string(r), Code: r})
	}

	items := model.Picker.Session.filtered
	if len(items) != 2 {
		t.Fatalf("filtered items = %#v, want two matches", items)
	}
	if got := model.Picker.Session.index; got != 0 {
		t.Fatalf("selected index = %d, want top ranked match", got)
	}
	if got := items[model.Picker.Session.index].info.ID; got != "sess-2" {
		t.Fatalf("selected session = %q, want sess-2", got)
	}
}

func TestSessionPickerPasteFiltersWithoutChangingComposer(t *testing.T) {
	model := readyModel(t)
	model.Input.Composer.SetValue("draft")
	model.Picker.Session = &sessionPickerState{
		items: []sessionPickerItem{
			{info: storage.SessionInfo{ID: "sess-1", Title: "zz resume"}},
			{info: storage.SessionInfo{ID: "sess-2", Title: "resume"}},
		},
		index: 1,
	}
	model.Picker.Session.filtered = model.Picker.Session.items

	updated, _ := model.Update(tea.PasteMsg{Content: "resume\n"})
	model = updated.(Model)

	if got := model.Input.Composer.Value(); got != "draft" {
		t.Fatalf("composer = %q, want unchanged draft", got)
	}
	if got := model.Picker.Session.query; got != "resume" {
		t.Fatalf("session query = %q, want pasted query", got)
	}
	if got := model.Picker.Session.filtered[model.Picker.Session.index].info.ID; got != "sess-2" {
		t.Fatalf("selected session = %q, want top pasted-query match", got)
	}
}

func TestSessionPickerPageKeysJumpByPage(t *testing.T) {
	model := readyModel(t)
	items := make([]sessionPickerItem, 12)
	for i := range items {
		items[i] = sessionPickerItem{
			info: storage.SessionInfo{
				ID:    "sess-" + string(rune('a'+i)),
				Title: "session " + string(rune('a'+i)),
			},
		}
	}
	model.Picker.Session = &sessionPickerState{
		items:    items,
		filtered: slices.Clone(items),
		index:    0,
	}

	updated, _ := model.handleSessionPickerKey(tea.KeyPressMsg{Code: tea.KeyPgDown})
	model = updated
	if got := model.Picker.Session.index; got != pickerPageSize {
		t.Fatalf("index after pgdown = %d, want %d", got, pickerPageSize)
	}

	updated, _ = model.handleSessionPickerKey(tea.KeyPressMsg{Code: tea.KeyPgUp})
	model = updated
	if got := model.Picker.Session.index; got != 0 {
		t.Fatalf("index after pgup = %d, want 0", got)
	}
}

func TestSessionPickerRowsFitTerminalWidth(t *testing.T) {
	model := readyModel(t)
	model.App.Width = 80
	model.Picker.Session = &sessionPickerState{
		items: []sessionPickerItem{{
			info: storage.SessionInfo{
				ID:          "sess-1",
				Model:       "local-api/qwen3.6:27b-uncensored",
				Branch:      "feature/very-long-session-picker-branch-name",
				UpdatedAt:   time.Now().Add(-48 * time.Hour),
				Title:       "hi, read /Users/nick/github/nijaru/ion/AGENTS.md and summarize the important parts",
				LastPreview: "use the read tool on /Users/nick/github/nijaru/ion/AGENTS.md, then tell me the current priorities",
			},
		}},
		index: 0,
	}
	model.Picker.Session.filtered = model.Picker.Session.items

	out := ansi.Strip(model.renderSessionPicker())
	for _, line := range strings.Split(out, "\n") {
		if ansi.StringWidth(line) > model.shellWidth() {
			t.Fatalf(
				"session picker line width = %d, want <= %d: %q",
				ansi.StringWidth(line),
				model.shellWidth(),
				line,
			)
		}
	}
	if !strings.Contains(out, "feature/very-long-session-picker-branch-name • 2d ago") {
		t.Fatalf("session picker should preserve stable metadata and age: %q", out)
	}
}

func TestSessionAgeLabelUsesDaysForOlderSessions(t *testing.T) {
	got := humanizeSessionAge(8*24*time.Hour + 3*time.Hour)
	if got != "8d ago" {
		t.Fatalf("age label = %q, want 8d ago", got)
	}
	if strings.Contains(got, "h0m0s") {
		t.Fatalf("age label leaked raw duration: %q", got)
	}
}

func TestProviderItemsShowConfiguredStatus(t *testing.T) {
	t.Setenv("OPENROUTER_API_KEY", "test-key")
	items := providerItems(&config.Config{})

	for label, wantDetail := range map[string]string{
		"Anthropic":  "Set ANTHROPIC_API_KEY",
		"OpenRouter": "Ready",
		"Ollama":     "Ready",
	} {
		found := false
		for _, item := range items {
			if item.Label != label {
				continue
			}
			found = true
			if item.Detail != wantDetail {
				t.Fatalf("provider %q detail = %q, want %q", item.Label, item.Detail, wantDetail)
			}
		}
		if !found {
			t.Fatalf("provider %q not found", label)
		}
	}
}

func TestModelItemsUseInjectedModelLister(t *testing.T) {
	oldListModelsForConfig := listModelsForConfig
	listModelsForConfig = func(ctx context.Context, cfg *config.Config) ([]registry.ModelMetadata, error) {
		if cfg.Provider != "openrouter" {
			t.Fatalf("provider = %q, want openrouter", cfg.Provider)
		}
		return []registry.ModelMetadata{
			{
				ID:               "z-ai/glm-4.5",
				Created:          time.Date(2025, 2, 1, 0, 0, 0, 0, time.UTC).Unix(),
				ContextLimit:     64000,
				InputPrice:       1.23,
				OutputPrice:      4.56,
				InputPriceKnown:  true,
				OutputPriceKnown: true,
			},
			{
				ID:               "openai/gpt-4.1",
				Created:          time.Date(2025, 4, 1, 0, 0, 0, 0, time.UTC).Unix(),
				ContextLimit:     128000,
				InputPrice:       0.1,
				OutputPrice:      0.2,
				InputPriceKnown:  true,
				OutputPriceKnown: true,
			},
			{
				ID:               "z-ai/glm-5",
				Created:          time.Date(2025, 5, 1, 0, 0, 0, 0, time.UTC).Unix(),
				ContextLimit:     128000,
				InputPrice:       0.2,
				OutputPrice:      0.4,
				InputPriceKnown:  true,
				OutputPriceKnown: true,
			},
		}, nil
	}
	defer func() { listModelsForConfig = oldListModelsForConfig }()

	items, err := modelItemsForProvider(t.Context(), &config.Config{Provider: "openrouter"})
	if err != nil {
		t.Fatalf("modelItemsForProvider: %v", err)
	}
	if len(items) != 3 {
		t.Fatalf("items len = %d, want 3", len(items))
	}
	wantOrder := []string{"openai/gpt-4.1", "z-ai/glm-5", "z-ai/glm-4.5"}
	gotOrder := []string{items[0].Label, items[1].Label, items[2].Label}
	if !slices.Equal(gotOrder, wantOrder) {
		t.Fatalf("items not sorted by org/newest: got %#v want %#v", gotOrder, wantOrder)
	}
	if items[0].Metrics == nil {
		t.Fatal("expected model metrics")
	}
	if items[0].Metrics.Context != "128k" || items[0].Metrics.Input != "$0.10" ||
		items[0].Metrics.Output != "$0.20" {
		t.Fatalf("unexpected model metrics: %#v", items[0].Metrics)
	}
}

func TestModelItemsTreatZeroPricesAsFreeSearchTerm(t *testing.T) {
	oldListModelsForConfig := listModelsForConfig
	listModelsForConfig = func(ctx context.Context, cfg *config.Config) ([]registry.ModelMetadata, error) {
		return []registry.ModelMetadata{
			{
				ID:               "vendor/model-free",
				ContextLimit:     128000,
				InputPrice:       0,
				OutputPrice:      0,
				InputPriceKnown:  true,
				OutputPriceKnown: true,
			},
			{
				ID:               "vendor/model-paid",
				ContextLimit:     128000,
				InputPrice:       0.1,
				OutputPrice:      0.2,
				InputPriceKnown:  true,
				OutputPriceKnown: true,
			},
			{ID: "vendor/model-unknown", ContextLimit: 128000},
		}, nil
	}
	defer func() { listModelsForConfig = oldListModelsForConfig }()

	items, err := modelItemsForProvider(t.Context(), &config.Config{Provider: "openrouter"})
	if err != nil {
		t.Fatalf("modelItemsForProvider: %v", err)
	}

	filtered := rankedPickerItems(items, "free")
	got := make([]string, 0, len(filtered))
	for _, item := range filtered {
		got = append(got, item.Label)
	}
	if !slices.Contains(got, "vendor/model-free") {
		t.Fatalf("expected zero-priced model to match free query, got %v", got)
	}
	if slices.Contains(got, "vendor/model-paid") {
		t.Fatalf("did not expect paid model to match free query, got %v", got)
	}
	if slices.Contains(got, "vendor/model-unknown") {
		t.Fatalf("did not expect unknown-priced model to match free query, got %v", got)
	}
}

func TestModelMetricsRenderFreeAndUnknownDistinctly(t *testing.T) {
	free := modelMetrics(registry.ModelMetadata{
		ContextLimit:     128000,
		InputPrice:       0,
		OutputPrice:      0,
		InputPriceKnown:  true,
		OutputPriceKnown: true,
	})
	if free == nil || free.Input != "Free" || free.Output != "Free" {
		t.Fatalf("expected free metrics, got %#v", free)
	}

	unknown := modelMetrics(registry.ModelMetadata{
		ContextLimit: 128000,
	})
	if unknown == nil {
		t.Fatal("expected context-only metrics")
	}
	if unknown.Input != "" || unknown.Output != "" {
		t.Fatalf("expected unknown pricing to stay blank, got %#v", unknown)
	}
}

func TestPickerFilteringMatchesTypedQuery(t *testing.T) {
	model := readyModel(t)
	model.Picker.Overlay = &pickerOverlayState{
		title: "Pick a provider",
		items: []pickerItem{
			{Label: "Anthropic", Value: "anthropic", Detail: "Set ANTHROPIC_API_KEY"},
			{Label: "OpenRouter", Value: "openrouter", Detail: "Ready"},
		},
		filtered: []pickerItem{
			{Label: "Anthropic", Value: "anthropic", Detail: "Set ANTHROPIC_API_KEY"},
			{Label: "OpenRouter", Value: "openrouter", Detail: "Ready"},
		},
		purpose: pickerPurposeProvider,
	}

	for _, r := range []rune("router") {
		model, _ = model.handlePickerKey(tea.KeyPressMsg{Text: string(r), Code: r})
	}

	if got := len(pickerDisplayItems(model.Picker.Overlay)); got != 1 {
		t.Fatalf("filtered items = %d, want 1", got)
	}
	if got := pickerDisplayItems(model.Picker.Overlay)[0].Label; got != "OpenRouter" {
		t.Fatalf("filtered label = %q, want OpenRouter", got)
	}
}

func TestPickerPasteFiltersWithoutChangingComposer(t *testing.T) {
	model := readyModel(t)
	model.Input.Composer.SetValue("draft")
	model.Picker.Overlay = &pickerOverlayState{
		title: "Pick a provider",
		items: []pickerItem{
			{Label: "Anthropic", Value: "anthropic", Detail: "Set ANTHROPIC_API_KEY"},
			{Label: "OpenRouter", Value: "openrouter", Detail: "Ready"},
		},
		filtered: []pickerItem{
			{Label: "Anthropic", Value: "anthropic", Detail: "Set ANTHROPIC_API_KEY"},
			{Label: "OpenRouter", Value: "openrouter", Detail: "Ready"},
		},
		purpose: pickerPurposeProvider,
	}

	updated, _ := model.Update(tea.PasteMsg{Content: "router\n"})
	model = updated.(Model)

	if got := model.Input.Composer.Value(); got != "draft" {
		t.Fatalf("composer = %q, want unchanged draft", got)
	}
	if got := model.Picker.Overlay.query; got != "router" {
		t.Fatalf("picker query = %q, want pasted query", got)
	}
	if got := len(pickerDisplayItems(model.Picker.Overlay)); got != 1 {
		t.Fatalf("filtered items = %d, want 1", got)
	}
	if got := pickerDisplayItems(model.Picker.Overlay)[0].Label; got != "OpenRouter" {
		t.Fatalf("filtered label = %q, want OpenRouter", got)
	}
}

func TestPickerFilteringRanksClosestMatchesFirst(t *testing.T) {
	model := readyModel(t)
	model.Picker.Overlay = &pickerOverlayState{
		title: "Pick a model for openrouter",
		items: []pickerItem{
			{Label: "z-ai/glm-5-turbo", Value: "z-ai/glm-5-turbo"},
			{Label: "z-ai/glm-5", Value: "z-ai/glm-5"},
			{Label: "z-ai/glm-4.5", Value: "z-ai/glm-4.5"},
		},
		filtered: []pickerItem{
			{Label: "z-ai/glm-5-turbo", Value: "z-ai/glm-5-turbo"},
			{Label: "z-ai/glm-5", Value: "z-ai/glm-5"},
			{Label: "z-ai/glm-4.5", Value: "z-ai/glm-4.5"},
		},
		purpose: pickerPurposeModel,
	}

	for _, r := range []rune("glm-5") {
		model, _ = model.handlePickerKey(tea.KeyPressMsg{Text: string(r), Code: r})
	}

	items := pickerDisplayItems(model.Picker.Overlay)
	if len(items) != 2 {
		t.Fatalf("filtered items = %d, want 2", len(items))
	}
	if items[0].Label != "z-ai/glm-5" {
		t.Fatalf("top match = %q, want z-ai/glm-5", items[0].Label)
	}
	if items[1].Label != "z-ai/glm-5-turbo" {
		t.Fatalf("second match = %q, want z-ai/glm-5-turbo", items[1].Label)
	}
	for _, item := range items {
		if item.Label == "z-ai/glm-4.5" {
			t.Fatalf("unexpected loose match for glm-5 query: %+v", items)
		}
	}
}

func TestPickerFilteringSelectsTopRankedMatch(t *testing.T) {
	model := readyModel(t)
	model.Picker.Overlay = &pickerOverlayState{
		title: "Pick a model",
		items: []pickerItem{
			{Label: "vendor/slow-model", Value: "vendor/slow-model"},
			{Label: "vendor/fast", Value: "vendor/fast"},
			{Label: "vendor/fast-turbo", Value: "vendor/fast-turbo"},
		},
		filtered: []pickerItem{
			{Label: "vendor/slow-model", Value: "vendor/slow-model"},
			{Label: "vendor/fast", Value: "vendor/fast"},
			{Label: "vendor/fast-turbo", Value: "vendor/fast-turbo"},
		},
		index:   2,
		purpose: pickerPurposeModel,
	}

	for _, r := range []rune("fast") {
		model, _ = model.handlePickerKey(tea.KeyPressMsg{Text: string(r), Code: r})
	}

	items := pickerDisplayItems(model.Picker.Overlay)
	if len(items) != 2 {
		t.Fatalf("filtered items = %d, want 2", len(items))
	}
	if got := model.Picker.Overlay.index; got != 0 {
		t.Fatalf("selected index = %d, want top ranked match", got)
	}
	if got := items[model.Picker.Overlay.index].Value; got != "vendor/fast" {
		t.Fatalf("selected model = %q, want vendor/fast", got)
	}
}

func TestModelPickerRendersSeparatePriceColumns(t *testing.T) {
	model := readyModel(t)
	model.Picker.Overlay = &pickerOverlayState{
		title: "Pick a model for openrouter",
		items: []pickerItem{
			{
				Label: "z-ai/glm-5",
				Value: "z-ai/glm-5",
				Metrics: &pickerMetrics{
					Context: "80k",
					Input:   "$0.72",
					Output:  "$2.30",
				},
			},
			{
				Label: "z-ai/glm-5-turbo",
				Value: "z-ai/glm-5-turbo",
				Metrics: &pickerMetrics{
					Context: "202k",
					Input:   "$1.20",
					Output:  "$4.00",
				},
			},
		},
		filtered: []pickerItem{
			{
				Label: "z-ai/glm-5",
				Value: "z-ai/glm-5",
				Metrics: &pickerMetrics{
					Context: "80k",
					Input:   "$0.72",
					Output:  "$2.30",
				},
			},
			{
				Label: "z-ai/glm-5-turbo",
				Value: "z-ai/glm-5-turbo",
				Metrics: &pickerMetrics{
					Context: "202k",
					Input:   "$1.20",
					Output:  "$4.00",
				},
			},
		},
		purpose: pickerPurposeModel,
	}

	rendered := ansi.Strip(model.renderPicker())
	if !strings.Contains(rendered, "Model") || !strings.Contains(rendered, "Context") ||
		!strings.Contains(rendered, "Input") ||
		!strings.Contains(rendered, "Output") {
		t.Fatalf("rendered picker missing header row: %q", rendered)
	}
	var header, rowA, rowB string
	for _, line := range strings.Split(rendered, "\n") {
		switch {
		case strings.Contains(line, "Model") && strings.Contains(line, "Context") && strings.Contains(line, "Input") && strings.Contains(line, "Output"):
			header = line
		case strings.Contains(line, "z-ai/glm-5-turbo"):
			rowA = line
		case strings.Contains(line, "z-ai/glm-5") && !strings.Contains(line, "turbo"):
			rowB = line
		}
	}
	if header == "" || rowA == "" || rowB == "" {
		t.Fatalf("did not find model rows in rendered picker: %q", rendered)
	}
	if !strings.Contains(rowA, "202k") || !strings.Contains(rowB, "80k") ||
		!strings.Contains(rowA, "$1.20") || !strings.Contains(rowB, "$0.72") ||
		!strings.Contains(rowA, "$4.00") || !strings.Contains(rowB, "$2.30") {
		t.Fatalf("missing detail columns in rendered picker: %q", rendered)
	}
	headerContext := lipgloss.Width(header[:strings.Index(header, "Context")])
	rowAContext := lipgloss.Width(rowA[:strings.Index(rowA, "202k")])
	rowBContext := lipgloss.Width(rowB[:strings.Index(rowB, "80k")])
	if headerContext != rowAContext || headerContext != rowBContext {
		t.Fatalf("context column not aligned:\nheader=%q\nrowA=%q\nrowB=%q", header, rowA, rowB)
	}
	headerInput := lipgloss.Width(header[:strings.Index(header, "Input")])
	rowAInput := lipgloss.Width(rowA[:strings.Index(rowA, "$1.20")])
	rowBInput := lipgloss.Width(rowB[:strings.Index(rowB, "$0.72")])
	if headerInput != rowAInput || headerInput != rowBInput {
		t.Fatalf("input column not aligned:\nheader=%q\nrowA=%q\nrowB=%q", header, rowA, rowB)
	}
	headerOutput := lipgloss.Width(header[:strings.Index(header, "Output")])
	rowAOutput := lipgloss.Width(rowA[:strings.Index(rowA, "$4.00")])
	rowBOutput := lipgloss.Width(rowB[:strings.Index(rowB, "$2.30")])
	if headerOutput != rowAOutput || headerOutput != rowBOutput {
		t.Fatalf("output column not aligned:\nheader=%q\nrowA=%q\nrowB=%q", header, rowA, rowB)
	}
}

func TestModelPickerCtrlMChangesEditTargetWithoutChangingActiveRuntime(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	cfg := &config.Config{
		Provider:  "openai",
		Model:     "gpt-4.1",
		FastModel: "gpt-4.1-mini",
	}
	model := readyModel(t)
	model.Model.Backend = stubBackend{
		provider: "openai",
		model:    "gpt-4.1",
	}

	updated, _ := model.openModelPickerWithConfig(cfg)
	model = updated
	updated, _ = model.handlePickerKey(tea.KeyPressMsg{Code: 'm', Mod: tea.ModCtrl})
	model = updated

	if model.App.ActivePreset != presetPrimary {
		t.Fatalf("active preset = %q, want unchanged primary", model.App.ActivePreset)
	}
	state, err := config.LoadState()
	if err != nil {
		t.Fatalf("load state: %v", err)
	}
	if state.ActivePreset != nil {
		t.Fatalf("persisted active preset = %#v, want nil", state.ActivePreset)
	}
	if got := model.Model.Backend.Model(); got != "gpt-4.1" {
		t.Fatalf("backend model = %q, want unchanged primary model", got)
	}
	if model.Picker.Overlay == nil || model.Picker.Overlay.modelPreset() != presetFast {
		t.Fatalf("picker preset = %#v, want fast edit target", model.Picker.Overlay)
	}
	if !strings.Contains(model.Picker.Overlay.title, "Pick fast model") {
		t.Fatalf("picker title = %q, want fast target", model.Picker.Overlay.title)
	}
	if got := pickerDisplayItems(model.Picker.Overlay)[model.Picker.Overlay.index].Value; got != "gpt-4.1-mini" {
		t.Fatalf("selected model = %q, want fast model", got)
	}
	if line := ansi.Strip(model.statusLine()); strings.Contains(line, "(fast)") {
		t.Fatalf("status line changed active runtime after picker toggle: %q", line)
	}
}

func TestModelPickerSelectingExistingFastModelActivatesFastPreset(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	cfg := &config.Config{
		Provider:            "openai",
		Model:               "gpt-4.1",
		FastModel:           "gpt-4.1-mini",
		FastReasoningEffort: "low",
	}
	model := readyModel(t)
	model.Model.Backend = stubBackend{
		provider: "openai",
		model:    "gpt-4.1",
	}
	model.Picker.Overlay = &pickerOverlayState{
		title: "Pick fast model: OpenAI",
		items: []pickerItem{
			{Label: "gpt-4.1-mini", Value: "gpt-4.1-mini"},
		},
		filtered: []pickerItem{
			{Label: "gpt-4.1-mini", Value: "gpt-4.1-mini"},
		},
		index:   0,
		purpose: pickerPurposeModel,
		preset:  presetFast,
		cfg:     cfg,
	}

	updated, cmd := model.commitPickerSelection()
	model = updated

	if cmd == nil {
		t.Fatal("expected model selection notice")
	}
	if model.App.ActivePreset != presetFast {
		t.Fatalf("active preset = %q, want fast", model.App.ActivePreset)
	}
	state, err := config.LoadState()
	if err != nil {
		t.Fatalf("load state: %v", err)
	}
	if state.ActivePreset == nil || *state.ActivePreset != "fast" {
		t.Fatalf("persisted active preset = %#v, want fast", state.ActivePreset)
	}
}

func TestThinkingPickerCommitUpdatesAppConfig(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	cfg := &config.Config{
		Provider:            "openai",
		Model:               "gpt-4.1",
		ReasoningEffort:     "auto",
		FastModel:           "gpt-4.1-mini",
		FastReasoningEffort: "low",
	}
	capture := &configCaptureBackend{
		stubBackend: stubBackend{
			provider: "openai",
			model:    "gpt-4.1",
		},
	}
	model := readyModel(t)
	model.Model.Backend = capture
	model.Model.Config = cfg

	updated, cmd := model.openThinkingPicker()
	model = updated
	if cmd != nil {
		t.Fatalf("thinking picker returned unexpected command %T", cmd)
	}
	model.Picker.Overlay.index = pickerIndex(model.Picker.Overlay.items, "high")

	updated, cmd = model.commitPickerSelection()
	model = updated
	if cmd == nil {
		t.Fatal("expected thinking selection notice")
	}
	if capture.cfg == nil || capture.cfg.ReasoningEffort != "high" {
		t.Fatalf("backend config = %#v, want high reasoning", capture.cfg)
	}
	if model.Model.Config == nil ||
		model.Model.Config.ReasoningEffort != "high" ||
		model.Model.Config.FastReasoningEffort != "low" {
		t.Fatalf("app config = %#v, want updated full config", model.Model.Config)
	}
	if model.Progress.ReasoningEffort != "high" {
		t.Fatalf("progress reasoning = %q, want high", model.Progress.ReasoningEffort)
	}
}

func TestModelPickerMetricHeaderFitsShellWidth(t *testing.T) {
	model := readyModel(t)
	updated, _ := model.Update(tea.WindowSizeMsg{Width: 40, Height: 24})
	model = updated.(Model)
	longModel := "provider/really-long-model-name-that-must-not-wrap"
	model.Picker.Overlay = &pickerOverlayState{
		title: "Pick a model",
		items: []pickerItem{{
			Label: longModel,
			Value: longModel,
			Metrics: &pickerMetrics{
				Context: "262k",
				Input:   "$0.11",
				Output:  "$0.44",
			},
		}},
		filtered: []pickerItem{{
			Label: longModel,
			Value: longModel,
			Metrics: &pickerMetrics{
				Context: "262k",
				Input:   "$0.11",
				Output:  "$0.44",
			},
		}},
		purpose: pickerPurposeModel,
	}

	out := ansi.Strip(model.renderPicker())
	for i, line := range strings.Split(out, "\n") {
		if got := ansi.StringWidth(line); got > model.shellWidth() {
			t.Fatalf(
				"model picker line %d width = %d, want <= %d: %q\n%s",
				i,
				got,
				model.shellWidth(),
				line,
				out,
			)
		}
	}
}

func TestPickerFilteringAcceptsSpaceInput(t *testing.T) {
	model := readyModel(t)
	model.Picker.Overlay = &pickerOverlayState{
		title: "Pick a provider",
		items: []pickerItem{
			{Label: "alpha", Value: "alpha", Detail: "Set ALPHA_API_KEY"},
			{Label: "beta", Value: "beta", Detail: "Ready"},
		},
		filtered: []pickerItem{
			{Label: "alpha", Value: "alpha", Detail: "Set ALPHA_API_KEY"},
			{Label: "beta", Value: "beta", Detail: "Ready"},
		},
		purpose: pickerPurposeProvider,
	}

	for _, key := range []tea.KeyPressMsg{
		{Text: "s", Code: 's'},
		{Text: "e", Code: 'e'},
		{Text: "t", Code: 't'},
		{Text: " ", Code: tea.KeySpace},
		{Text: "A", Code: 'A'},
		{Text: "L", Code: 'L'},
		{Text: "P", Code: 'P'},
		{Text: "H", Code: 'H'},
		{Text: "A", Code: 'A'},
	} {
		model, _ = model.handlePickerKey(key)
	}

	if got := model.Picker.Overlay.query; got != "set ALPHA" {
		t.Fatalf("picker query = %q, want %q", got, "set ALPHA")
	}
	if got := len(pickerDisplayItems(model.Picker.Overlay)); got != 1 {
		t.Fatalf("filtered items = %d, want 1", got)
	}
}

func TestOpenModelPickerDoesNotFetchBeforeReturning(t *testing.T) {
	called := false
	stubModelCatalog(
		t,
		func(ctx context.Context, cfg *config.Config) ([]registry.ModelMetadata, error) {
			called = true
			return []registry.ModelMetadata{{ID: "vendor/model-a"}}, nil
		},
	)

	model := readyModel(t)
	updated, cmd := model.openModelPickerWithConfig(&config.Config{
		Provider: "openrouter",
		Model:    "vendor/current",
	})
	model = updated
	if called {
		t.Fatal("model catalog was fetched before the picker returned")
	}
	if cmd == nil {
		t.Fatal("expected background model catalog load command")
	}
	if model.Picker.Overlay == nil || !model.Picker.Overlay.loading {
		t.Fatalf("picker overlay = %#v, want immediate loading model picker", model.Picker.Overlay)
	}
	items := pickerDisplayItems(model.Picker.Overlay)
	if len(items) != 1 || items[0].Value != "vendor/current" || items[0].Detail != "primary" {
		t.Fatalf("initial picker items = %#v, want selected primary model", items)
	}

	model = resolveModelPickerLoad(t, model, cmd)
	if !called {
		t.Fatal("background model catalog load did not run")
	}
	if model.Picker.Overlay.loading {
		t.Fatal("picker still loading after catalog result")
	}
}

func TestOpenModelPickerUsesFreshCacheWithoutRefresh(t *testing.T) {
	oldListModelsForConfig := listModelsForConfig
	oldCachedModelsForConfig := cachedModelsForConfig
	listModelsForConfig = func(ctx context.Context, cfg *config.Config) ([]registry.ModelMetadata, error) {
		t.Fatal("fresh cache should not trigger model catalog refresh")
		return nil, nil
	}
	cachedModelsForConfig = func(cfg *config.Config) ([]registry.ModelMetadata, bool, bool) {
		return []registry.ModelMetadata{{ID: "vendor/cached"}}, true, true
	}
	t.Cleanup(func() {
		listModelsForConfig = oldListModelsForConfig
		cachedModelsForConfig = oldCachedModelsForConfig
	})

	model := readyModel(t)
	updated, cmd := model.openModelPickerWithConfig(&config.Config{
		Provider: "openrouter",
		Model:    "vendor/cached",
	})
	model = updated
	if cmd != nil {
		t.Fatalf("fresh cache returned refresh command %T", cmd)
	}
	if model.Picker.Overlay == nil || model.Picker.Overlay.loading {
		t.Fatalf("picker overlay = %#v, want loaded picker from cache", model.Picker.Overlay)
	}
	items := pickerDisplayItems(model.Picker.Overlay)
	if len(items) != 1 || items[0].Value != "vendor/cached" {
		t.Fatalf("cached picker items = %#v", items)
	}
}

func TestOpenModelPickerShowsStaleCacheWhileRefreshing(t *testing.T) {
	oldListModelsForConfig := listModelsForConfig
	oldCachedModelsForConfig := cachedModelsForConfig
	listModelsForConfig = func(ctx context.Context, cfg *config.Config) ([]registry.ModelMetadata, error) {
		return []registry.ModelMetadata{{ID: "vendor/fresh"}}, nil
	}
	cachedModelsForConfig = func(cfg *config.Config) ([]registry.ModelMetadata, bool, bool) {
		return []registry.ModelMetadata{{ID: "vendor/stale"}}, false, true
	}
	t.Cleanup(func() {
		listModelsForConfig = oldListModelsForConfig
		cachedModelsForConfig = oldCachedModelsForConfig
	})

	model := readyModel(t)
	updated, cmd := model.openModelPickerWithConfig(&config.Config{
		Provider: "openrouter",
		Model:    "vendor/stale",
	})
	model = updated
	if cmd == nil {
		t.Fatal("expected stale cache to refresh in the background")
	}
	items := pickerDisplayItems(model.Picker.Overlay)
	if len(items) != 1 || items[0].Value != "vendor/stale" || !model.Picker.Overlay.loading {
		t.Fatalf("initial stale-cache picker = %#v loading=%v", items, model.Picker.Overlay.loading)
	}

	model = resolveModelPickerLoad(t, model, cmd)
	items = pickerDisplayItems(model.Picker.Overlay)
	if len(items) != 2 || items[1].Value != "vendor/fresh" || model.Picker.Overlay.loading {
		t.Fatalf("refreshed picker = %#v loading=%v", items, model.Picker.Overlay.loading)
	}
}

func TestModelPickerListsSelectedModelsAtTop(t *testing.T) {
	stubModelCatalog(
		t,
		func(ctx context.Context, cfg *config.Config) ([]registry.ModelMetadata, error) {
			if cfg.Provider != "openrouter" {
				t.Fatalf("provider = %q, want openrouter", cfg.Provider)
			}
			return []registry.ModelMetadata{
				{ID: "vendor/model-a"},
				{ID: "vendor/model-b"},
				{ID: "vendor/model-c"},
			}, nil
		},
	)

	model := readyModel(t)
	updated, cmd := model.openModelPickerWithConfig(&config.Config{
		Provider:  "openrouter",
		Model:     "vendor/model-b",
		FastModel: "vendor/model-a",
	})
	model = updated
	model = resolveModelPickerLoad(t, model, cmd)
	if model.Picker.Overlay == nil {
		t.Fatal("expected model picker overlay")
	}
	items := pickerDisplayItems(model.Picker.Overlay)
	if len(items) != 3 {
		t.Fatalf("item count = %d, want 3", len(items))
	}
	if items[0].Group != "Current" || items[1].Group != "Current" {
		t.Fatalf(
			"current groups = [%q %q], want [Current Current]",
			items[0].Group,
			items[1].Group,
		)
	}
	if items[0].Value != "vendor/model-b" || items[1].Value != "vendor/model-a" {
		t.Fatalf(
			"configured values = [%q %q], want [vendor/model-b vendor/model-a]",
			items[0].Value,
			items[1].Value,
		)
	}
	if items[2].Group != "All models" {
		t.Fatalf("catalog group = %q, want All models", items[2].Group)
	}

	rendered := ansi.Strip(model.renderPicker())
	if !strings.Contains(rendered, "Current") ||
		!strings.Contains(rendered, "All models") {
		t.Fatalf("rendered picker missing model groups: %q", rendered)
	}
}

func TestModelPickerDoesNotPromoteResolvedFastDefault(t *testing.T) {
	stubModelCatalog(
		t,
		func(ctx context.Context, cfg *config.Config) ([]registry.ModelMetadata, error) {
			return []registry.ModelMetadata{
				{ID: "google/gemini-2.0-flash-lite-001"},
				{ID: "vendor/model-c"},
			}, nil
		},
	)

	model := readyModel(t)
	updated, cmd := model.openModelPickerWithConfig(&config.Config{
		Provider: "openrouter",
		Model:    "vendor/model-b",
	})
	model = updated
	model = resolveModelPickerLoad(t, model, cmd)
	items := pickerDisplayItems(model.Picker.Overlay)
	if len(items) != 3 {
		t.Fatalf("item count = %d, want 3", len(items))
	}
	if items[0].Value != "vendor/model-b" || items[0].Group != "Current" {
		t.Fatalf("selected primary row = %#v, want saved primary model first", items[0])
	}
	if items[0].Metrics != nil || items[0].Detail != "primary" {
		t.Fatalf(
			"missing metadata row = %#v, want primary detail without fake metric columns",
			items[0],
		)
	}
	for _, item := range items {
		if item.Value == "google/gemini-2.0-flash-lite-001" && item.Group == "Current" {
			t.Fatalf("resolved fast default should not appear as configured preset: %#v", item)
		}
	}
}

func TestModelPickerUsesRuntimeConfigOverPersistedState(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	cfgDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(cfgDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(
		filepath.Join(cfgDir, "state.toml"),
		[]byte("provider = \"local-api\"\nmodel = \"qwen3.6:27b\"\n"),
		0o644,
	); err != nil {
		t.Fatalf("write state: %v", err)
	}

	stubModelCatalog(
		t,
		func(ctx context.Context, cfg *config.Config) ([]registry.ModelMetadata, error) {
			if cfg.Provider != "openrouter" {
				t.Fatalf("provider = %q, want openrouter", cfg.Provider)
			}
			if cfg.Model != "tencent/hy3-preview:free" {
				t.Fatalf("model = %q, want runtime CLI override", cfg.Model)
			}
			return []registry.ModelMetadata{
				{ID: "anthropic/claude-sonnet-4.5"},
				{ID: "tencent/hy3-preview:free"},
			}, nil
		},
	)

	model := readyModel(t).WithConfig(&config.Config{
		Provider: "openrouter",
		Model:    "tencent/hy3-preview:free",
	})
	updated, cmd := model.openModelPicker()
	model = updated
	model = resolveModelPickerLoad(t, model, cmd)
	if model.Picker.Overlay == nil {
		t.Fatal("expected model picker overlay")
	}
	if !strings.Contains(model.Picker.Overlay.title, "OpenRouter") {
		t.Fatalf("picker title = %q, want active runtime provider", model.Picker.Overlay.title)
	}
	if got := model.Picker.Overlay.cfg.Provider; got != "openrouter" {
		t.Fatalf("picker config provider = %q, want openrouter", got)
	}
	if got := model.Picker.Overlay.cfg.Model; got != "tencent/hy3-preview:free" {
		t.Fatalf("picker config model = %q, want runtime model", got)
	}
	if got := pickerDisplayItems(model.Picker.Overlay)[model.Picker.Overlay.index].Value; got != "tencent/hy3-preview:free" {
		t.Fatalf("selected model = %q, want runtime model", got)
	}
}

func TestModelPickerTabReturnsToProviderPicker(t *testing.T) {
	model := readyModel(t)
	model.Picker.Overlay = &pickerOverlayState{
		title: "Pick a model for openrouter",
		items: []pickerItem{
			{Label: "vendor/model-b", Value: "vendor/model-b", Group: "Current"},
			{Label: "vendor/model-a", Value: "vendor/model-a", Group: "Current"},
		},
		filtered: []pickerItem{
			{Label: "vendor/model-b", Value: "vendor/model-b", Group: "Current"},
			{Label: "vendor/model-a", Value: "vendor/model-a", Group: "Current"},
		},
		purpose: pickerPurposeModel,
		cfg:     &config.Config{Provider: "openrouter"},
	}

	updated, _ := model.handlePickerKey(tea.KeyPressMsg{Code: tea.KeyTab})
	model = updated

	if model.Picker.Overlay == nil {
		t.Fatal("expected provider picker to open")
	}
	if model.Picker.Overlay.purpose != pickerPurposeProvider {
		t.Fatalf("picker purpose = %v, want provider picker", model.Picker.Overlay.purpose)
	}
}

func TestProviderPickerLocalAPISelectionRefreshesConfiguredEndpoint(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	ready := false
	requests := 0
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requests++
		if r.URL.Path != "/v1/models" {
			http.NotFound(w, r)
			return
		}
		if !ready {
			http.Error(w, "not ready", http.StatusServiceUnavailable)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"data":[{"id":"qwen3.6:27b"}]}`))
	}))
	defer srv.Close()

	endpoint := srv.URL + "/v1"
	cfgDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(cfgDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(
		filepath.Join(cfgDir, "config.toml"),
		[]byte("provider = \"local-api\"\nmodel = \"qwen3.6:27b\"\nendpoint = \""+endpoint+"\"\n"),
		0o644,
	); err != nil {
		t.Fatalf("write config: %v", err)
	}
	if err := config.SaveState(&config.Config{
		Provider: "openrouter",
		Model:    "deepseek/deepseek-v4-flash:free",
	}); err != nil {
		t.Fatalf("save state: %v", err)
	}
	if _, ok := providers.ProbeLocalAPI(context.Background(), &config.Config{
		Provider: "local-api",
		Endpoint: endpoint,
	}); ok {
		t.Fatal("expected initial local api probe to fail")
	}
	ready = true
	stubModelCatalog(
		t,
		func(ctx context.Context, cfg *config.Config) ([]registry.ModelMetadata, error) {
			if cfg.Provider != "openai-compatible" {
				t.Fatalf("provider = %q, want openai-compatible", cfg.Provider)
			}
			if cfg.Endpoint != endpoint {
				t.Fatalf("endpoint = %q, want configured endpoint %q", cfg.Endpoint, endpoint)
			}
			return []registry.ModelMetadata{{ID: "qwen3.6:27b"}}, nil
		},
	)

	model := readyModel(t)
	updated, cmd := model.openProviderPicker()
	model = updated
	if cmd != nil {
		t.Fatalf("provider picker returned unexpected command %T", cmd)
	}
	if model.Picker.Overlay == nil || model.Picker.Overlay.purpose != pickerPurposeProvider {
		t.Fatalf("picker = %#v, want provider picker", model.Picker.Overlay)
	}
	model.Picker.Overlay.index = pickerIndex(
		pickerDisplayItems(model.Picker.Overlay),
		"openai-compatible",
	)

	updated, cmd = model.handlePickerKey(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = resolveModelPickerLoad(t, updated, cmd)
	if requests < 2 {
		t.Fatalf("local api requests = %d, want fresh reprobe after cached failure", requests)
	}
	if model.Picker.Overlay == nil || model.Picker.Overlay.purpose != pickerPurposeModel {
		t.Fatalf("picker = %#v, want model picker", model.Picker.Overlay)
	}
	if model.Picker.Overlay.err != "" {
		t.Fatalf("model picker error = %q", model.Picker.Overlay.err)
	}
	if got := model.Picker.Overlay.cfg.Endpoint; got != endpoint {
		t.Fatalf("model picker endpoint = %q, want %q", got, endpoint)
	}
}

func TestModelProviderPickerTabPreservesFastEditTarget(t *testing.T) {
	model := readyModel(t)
	cfg := &config.Config{
		Provider:  "openrouter",
		Model:     "vendor/primary",
		FastModel: "vendor/fast",
	}
	model.Picker.Overlay = &pickerOverlayState{
		title: "Pick fast model: OpenRouter",
		items: []pickerItem{
			{Label: "vendor/primary", Value: "vendor/primary", Group: "Current"},
			{Label: "vendor/fast", Value: "vendor/fast", Group: "Current"},
		},
		filtered: []pickerItem{
			{Label: "vendor/primary", Value: "vendor/primary", Group: "Current"},
			{Label: "vendor/fast", Value: "vendor/fast", Group: "Current"},
		},
		index:   1,
		purpose: pickerPurposeModel,
		preset:  presetFast,
		cfg:     cfg,
	}

	updated, _ := model.handlePickerKey(tea.KeyPressMsg{Code: tea.KeyTab})
	model = updated
	if model.Picker.Overlay == nil || model.Picker.Overlay.purpose != pickerPurposeProvider {
		t.Fatalf("picker = %#v, want provider picker", model.Picker.Overlay)
	}
	if model.Picker.Overlay.modelPreset() != presetFast {
		t.Fatalf("provider picker preset = %q, want fast", model.Picker.Overlay.modelPreset())
	}

	updated, _ = model.handlePickerKey(tea.KeyPressMsg{Code: tea.KeyTab})
	model = updated
	if model.Picker.Overlay == nil || model.Picker.Overlay.purpose != pickerPurposeModel {
		t.Fatalf("picker = %#v, want model picker", model.Picker.Overlay)
	}
	if model.Picker.Overlay.modelPreset() != presetFast {
		t.Fatalf("model picker preset = %q, want fast", model.Picker.Overlay.modelPreset())
	}
	if !strings.Contains(model.Picker.Overlay.title, "Pick fast model") {
		t.Fatalf("picker title = %q, want fast target", model.Picker.Overlay.title)
	}
	if got := pickerDisplayItems(model.Picker.Overlay)[model.Picker.Overlay.index].Value; got != "vendor/fast" {
		t.Fatalf("selected model = %q, want fast model", got)
	}
}

func TestProviderPickerNonListingSelectionUsesPickerPreset(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("ZAI_API_KEY", "test-key")

	model := readyModel(t)
	model.App.ActivePreset = presetPrimary
	items := providerItems(&config.Config{})
	model.Picker.Overlay = &pickerOverlayState{
		title:    "Pick a provider",
		items:    items,
		filtered: items,
		index:    pickerIndex(items, "zai"),
		purpose:  pickerPurposeProvider,
		preset:   presetFast,
		cfg: &config.Config{
			Provider:  "openrouter",
			Model:     "vendor/primary",
			FastModel: "vendor/fast",
		},
	}

	updated, cmd := model.handlePickerKey(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = updated

	if cmd == nil {
		t.Fatal("expected non-listing provider selection notice")
	}
	if model.App.ActivePreset != presetFast {
		t.Fatalf("active preset = %q, want fast picker target", model.App.ActivePreset)
	}
	state, err := config.LoadState()
	if err != nil {
		t.Fatalf("load state: %v", err)
	}
	if state.ActivePreset == nil || *state.ActivePreset != "fast" {
		t.Fatalf("persisted active preset = %#v, want fast", state.ActivePreset)
	}
	cfg, err := config.Load()
	if err != nil {
		t.Fatalf("load config: %v", err)
	}
	if cfg.Provider != "zai" {
		t.Fatalf("config provider = %q, want zai", cfg.Provider)
	}
}

func TestModelPickerPageKeysJumpByPage(t *testing.T) {
	model := readyModel(t)
	items := make([]pickerItem, 12)
	for i := range items {
		value := "model-" + string(rune('a'+i))
		items[i] = pickerItem{
			Label:  value,
			Value:  value,
			Group:  "All models",
			Search: pickerSearchIndex(value, value, "", "", nil),
		}
	}
	model.Picker.Overlay = &pickerOverlayState{
		title:    "Pick a model",
		items:    items,
		filtered: slices.Clone(items),
		index:    0,
		purpose:  pickerPurposeModel,
		cfg:      &config.Config{Provider: "openrouter"},
	}

	updated, _ := model.handlePickerKey(tea.KeyPressMsg{Code: tea.KeyPgDown})
	model = updated
	if got := model.Picker.Overlay.index; got != pickerPageSize {
		t.Fatalf("index after pgdown = %d, want %d", got, pickerPageSize)
	}

	updated, _ = model.handlePickerKey(tea.KeyPressMsg{Code: tea.KeyPgUp})
	model = updated
	if got := model.Picker.Overlay.index; got != 0 {
		t.Fatalf("index after pgup = %d, want 0", got)
	}
}

func TestProviderItemsUseCatalogGroups(t *testing.T) {
	items := providerItems(&config.Config{})
	if len(items) < 9 {
		t.Fatalf("provider items = %d, want broad catalog", len(items))
	}
	for _, item := range items {
		if item.Group == "" {
			t.Fatalf("provider %q should have a picker group", item.Label)
		}
	}
}

func TestProviderItemsPreferReadyProvidersBeforeUnsetOnes(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	for _, name := range []string{
		"ANTHROPIC_API_KEY",
		"OPENAI_API_KEY",
		"OPENROUTER_API_KEY",
		"GEMINI_API_KEY",
		"GOOGLE_API_KEY",
		"HF_TOKEN",
		"TOGETHER_API_KEY",
		"DEEPSEEK_API_KEY",
		"GROQ_API_KEY",
		"FIREWORKS_API_KEY",
		"MISTRAL_API_KEY",
		"MOONSHOT_API_KEY",
		"CEREBRAS_API_KEY",
		"ZAI_API_KEY",
		"XAI_API_KEY",
		"OPENAI_COMPATIBLE_API_KEY",
	} {
		t.Setenv(name, "")
	}
	t.Setenv("OPENROUTER_API_KEY", "test")
	t.Setenv("GOOGLE_API_KEY", "test")

	items := providerItems(&config.Config{})
	indexOf := func(value string) int {
		for i, item := range items {
			if item.Value == value {
				return i
			}
		}
		return -1
	}
	groupOf := func(value string) string {
		for _, item := range items {
			if item.Value == value {
				return item.Group
			}
		}
		return ""
	}

	if indexOf("gemini") == -1 || indexOf("openrouter") == -1 ||
		indexOf("openai-compatible") == -1 {
		t.Fatalf("expected ready providers and OpenAI-compatible to appear in picker: %#v", items)
	}
	if indexOf("anthropic") == -1 {
		t.Fatalf("expected anthropic in picker")
	}
	if indexOf("gemini") > indexOf("anthropic") || indexOf("openrouter") > indexOf("anthropic") {
		t.Fatalf("ready remote providers should sort before unset direct providers")
	}
	if indexOf("openai-compatible") > indexOf("anthropic") {
		t.Fatalf("OpenAI-compatible should sort ahead of unset direct providers")
	}
	if groupOf("anthropic") != "Needs setup" {
		t.Fatalf("unset direct provider group = %q, want Needs setup", groupOf("anthropic"))
	}
}

func TestProviderItemsShowSingleOpenAICompatibleEndpoint(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	items := providerItems(&config.Config{})
	foundCustom := false
	for _, item := range items {
		if item.Value == "openai-compatible" {
			foundCustom = true
		}
		if item.Value == "local-api" {
			t.Fatalf("local-api should be accepted as an alias, not shown as a second picker entry")
		}
	}
	if !foundCustom {
		t.Fatalf("OpenAI-compatible endpoint should always be visible")
	}

	items = providerItems(
		&config.Config{Provider: "openai-compatible", Endpoint: "https://example.com/v1"},
	)
	found := false
	for _, item := range items {
		if item.Value == "openai-compatible" {
			found = true
			break
		}
	}
	if !found {
		t.Fatalf("custom endpoint entry should be shown when configured")
	}

	items = providerItems(&config.Config{Provider: "local-api", Endpoint: "http://127.0.0.1:1/v1"})
	found = false
	for _, item := range items {
		if item.Value == "openai-compatible" && item.Label == "OpenAI-compatible" {
			if item.Detail != "Not running" {
				t.Fatalf("OpenAI-compatible detail = %q, want %q", item.Detail, "Not running")
			}
			found = true
			break
		}
	}
	if !found {
		t.Fatalf("OpenAI-compatible should render when active through local-api alias")
	}
}

func TestProviderItemsUseConfiguredLocalAPIEndpointWhenRuntimeProviderDiffers(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/models" {
			http.NotFound(w, r)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"data":[{"id":"qwen3.6:27b"}]}`))
	}))
	defer srv.Close()

	items := providerItems(&config.Config{
		Provider: "openrouter",
		Endpoint: srv.URL + "/v1",
	})
	for _, item := range items {
		if item.Value != "openai-compatible" {
			continue
		}
		if !strings.Contains(item.Detail, "Ready at ") {
			t.Fatalf(
				"OpenAI-compatible detail = %q, want configured endpoint readiness",
				item.Detail,
			)
		}
		return
	}
	t.Fatal("OpenAI-compatible provider not found")
}

func TestProviderSelectionMissingAPIKeyOpensSetupPrompt(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("ANTHROPIC_API_KEY", "")
	stubModelCatalog(
		t,
		func(ctx context.Context, cfg *config.Config) ([]registry.ModelMetadata, error) {
			if cfg.Provider != "anthropic" {
				t.Fatalf("provider = %q, want anthropic", cfg.Provider)
			}
			return []registry.ModelMetadata{{ID: "claude-test"}}, nil
		},
	)

	model := readyModel(t)
	updated, cmd := model.handleCommand("/provider anthropic")
	model = updated
	if cmd != nil {
		t.Fatalf("unexpected provider command %T", cmd)
	}
	if model.Picker.Setup == nil || model.Picker.Setup.kind != setupPromptAPIKey {
		t.Fatalf("setup prompt = %#v, want API key prompt", model.Picker.Setup)
	}
	for _, r := range "sk-ant-test" {
		model, cmd = model.handleSetupPromptKey(tea.KeyPressMsg{Text: string(r)})
		if cmd != nil {
			t.Fatalf("typing returned command %T", cmd)
		}
	}
	model, cmd = model.handleSetupPromptKey(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = resolveModelPickerLoad(t, model, cmd)

	if model.Picker.Setup != nil {
		t.Fatal("setup prompt should close after saving key")
	}
	if got, ok := credentials.LookupAPIKey("anthropic"); !ok || got != "sk-ant-test" {
		t.Fatalf("saved credential = (%q, %v), want sk-ant-test true", got, ok)
	}
	if model.Picker.Overlay == nil || model.Picker.Overlay.purpose != pickerPurposeModel {
		t.Fatalf("picker = %#v, want model picker", model.Picker.Overlay)
	}
}

func TestSetupPromptPasteUpdatesPromptWithoutChangingComposer(t *testing.T) {
	model := readyModel(t)
	model.Input.Composer.SetValue("draft")
	model, cmd := model.openEndpointPrompt(&config.Config{}, presetPrimary)
	if cmd != nil {
		t.Fatalf("unexpected endpoint prompt command %T", cmd)
	}

	updated, _ := model.Update(tea.PasteMsg{Content: "fedora:11434\n"})
	model = updated.(Model)

	if got := model.Input.Composer.Value(); got != "draft" {
		t.Fatalf("composer = %q, want unchanged draft", got)
	}
	if got := model.Picker.Setup.value; got != "fedora:11434" {
		t.Fatalf("setup prompt value = %q, want pasted endpoint", got)
	}
}

func TestOpenAICompatibleEndpointPromptSavesEndpointAndOpensModels(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	stubModelCatalog(
		t,
		func(ctx context.Context, cfg *config.Config) ([]registry.ModelMetadata, error) {
			if cfg.Provider != "openai-compatible" {
				t.Fatalf("provider = %q, want openai-compatible", cfg.Provider)
			}
			if cfg.Endpoint != "http://fedora:11434/v1" {
				t.Fatalf("endpoint = %q, want normalized fedora endpoint", cfg.Endpoint)
			}
			return []registry.ModelMetadata{{ID: "qwen3.6:27b"}}, nil
		},
	)

	model := readyModel(t)
	model, cmd := model.openEndpointPrompt(&config.Config{}, presetPrimary)
	if cmd != nil {
		t.Fatalf("unexpected endpoint prompt command %T", cmd)
	}
	for _, r := range "fedora:11434" {
		model, cmd = model.handleSetupPromptKey(tea.KeyPressMsg{Text: string(r)})
		if cmd != nil {
			t.Fatalf("typing returned command %T", cmd)
		}
	}
	model, cmd = model.handleSetupPromptKey(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = resolveModelPickerLoad(t, model, cmd)

	stable, err := config.LoadStable()
	if err != nil {
		t.Fatalf("load stable config: %v", err)
	}
	if stable.Endpoint != "http://fedora:11434/v1" {
		t.Fatalf("stable endpoint = %q, want normalized fedora endpoint", stable.Endpoint)
	}
	if model.Picker.Overlay == nil || model.Picker.Overlay.purpose != pickerPurposeModel {
		t.Fatalf("picker = %#v, want model picker", model.Picker.Overlay)
	}
}

func TestProviderSelectionFailedOpenAICompatibleEndpointPromptsForEdit(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	configDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(configDir, 0o755); err != nil {
		t.Fatalf("mkdir config dir: %v", err)
	}
	if err := os.WriteFile(
		filepath.Join(configDir, "config.toml"),
		[]byte("provider = \"openai-compatible\"\nendpoint = \"http://127.0.0.1:1/v1\"\n"),
		0o644,
	); err != nil {
		t.Fatalf("write config: %v", err)
	}

	model := readyModel(t)
	model, cmd := model.handleCommand("/provider openai-compatible")
	if cmd != nil {
		t.Fatalf("unexpected provider command %T", cmd)
	}
	if model.Picker.Setup == nil || model.Picker.Setup.kind != setupPromptEndpoint {
		t.Fatalf("setup prompt = %#v, want endpoint prompt", model.Picker.Setup)
	}
	if model.Picker.Setup.value != "http://127.0.0.1:1/v1" {
		t.Fatalf("setup prompt value = %q, want configured endpoint", model.Picker.Setup.value)
	}
}
