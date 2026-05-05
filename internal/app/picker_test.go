package app

import (
	"context"
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
	"github.com/nijaru/ion/internal/storage"
)

func TestProviderItemsSortSetAPIsThenLocalThenUnset(t *testing.T) {
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
		"Local API",
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
	model = model.openCommandPicker("mode")

	updated, cmd := model.handlePickerKey(tea.KeyPressMsg{Code: tea.KeyEnter})
	model = updated
	if cmd != nil {
		t.Fatalf("unexpected command picker cmd %T", cmd)
	}
	if got := model.Input.Composer.Value(); got != "/mode " {
		t.Fatalf("composer = %q, want /mode insertion", got)
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

	items, err := modelItemsForProvider(&config.Config{Provider: "openrouter"})
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

	items, err := modelItemsForProvider(&config.Config{Provider: "openrouter"})
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

func TestModelPickerListsConfiguredPresetsAtTop(t *testing.T) {
	oldListModelsForConfig := listModelsForConfig
	listModelsForConfig = func(ctx context.Context, cfg *config.Config) ([]registry.ModelMetadata, error) {
		if cfg.Provider != "openrouter" {
			t.Fatalf("provider = %q, want openrouter", cfg.Provider)
		}
		return []registry.ModelMetadata{
			{ID: "vendor/model-a"},
			{ID: "vendor/model-b"},
			{ID: "vendor/model-c"},
		}, nil
	}
	defer func() { listModelsForConfig = oldListModelsForConfig }()

	model := readyModel(t)
	updated, cmd := model.openModelPickerWithConfig(&config.Config{
		Provider:  "openrouter",
		Model:     "vendor/model-b",
		FastModel: "vendor/model-a",
	})
	model = updated
	if cmd != nil {
		t.Fatalf("openModelPickerWithConfig returned unexpected command %T", cmd)
	}
	if model.Picker.Overlay == nil {
		t.Fatal("expected model picker overlay")
	}
	items := pickerDisplayItems(model.Picker.Overlay)
	if len(items) != 3 {
		t.Fatalf("item count = %d, want 3", len(items))
	}
	if items[0].Group != "Configured presets" || items[1].Group != "Configured presets" {
		t.Fatalf(
			"configured groups = [%q %q], want [Configured presets Configured presets]",
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
	if !strings.Contains(rendered, "Configured presets") ||
		!strings.Contains(rendered, "All models") {
		t.Fatalf("rendered picker missing model groups: %q", rendered)
	}
}

func TestModelPickerDoesNotPromoteResolvedFastDefault(t *testing.T) {
	oldListModelsForConfig := listModelsForConfig
	listModelsForConfig = func(ctx context.Context, cfg *config.Config) ([]registry.ModelMetadata, error) {
		return []registry.ModelMetadata{
			{ID: "google/gemini-2.0-flash-lite-001"},
			{ID: "vendor/model-c"},
		}, nil
	}
	defer func() { listModelsForConfig = oldListModelsForConfig }()

	model := readyModel(t)
	updated, cmd := model.openModelPickerWithConfig(&config.Config{
		Provider: "openrouter",
		Model:    "vendor/model-b",
	})
	model = updated
	if cmd != nil {
		t.Fatalf("openModelPickerWithConfig returned unexpected command %T", cmd)
	}
	items := pickerDisplayItems(model.Picker.Overlay)
	if len(items) != 3 {
		t.Fatalf("item count = %d, want 3", len(items))
	}
	if items[0].Value != "vendor/model-b" || items[0].Group != "Configured presets" {
		t.Fatalf("configured primary row = %#v, want stale configured model first", items[0])
	}
	if items[0].Metrics == nil || items[0].Metrics.Context != "—" ||
		items[0].Metrics.Input != "—" || items[0].Metrics.Output != "—" {
		t.Fatalf("missing metadata metrics = %#v, want explicit unknown columns", items[0].Metrics)
	}
	for _, item := range items {
		if item.Value == "google/gemini-2.0-flash-lite-001" && item.Group == "Configured presets" {
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

	oldListModelsForConfig := listModelsForConfig
	listModelsForConfig = func(ctx context.Context, cfg *config.Config) ([]registry.ModelMetadata, error) {
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
	}
	defer func() { listModelsForConfig = oldListModelsForConfig }()

	model := readyModel(t).WithConfig(&config.Config{
		Provider: "openrouter",
		Model:    "tencent/hy3-preview:free",
	})
	updated, cmd := model.openModelPicker()
	model = updated
	if cmd != nil {
		t.Fatalf("openModelPicker returned unexpected command %T", cmd)
	}
	if model.Picker.Overlay == nil {
		t.Fatal("expected model picker overlay")
	}
	if !strings.Contains(model.Picker.Overlay.title, "openrouter") {
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
			{Label: "vendor/model-b", Value: "vendor/model-b", Group: "Configured presets"},
			{Label: "vendor/model-a", Value: "vendor/model-a", Group: "Configured presets"},
		},
		filtered: []pickerItem{
			{Label: "vendor/model-b", Value: "vendor/model-b", Group: "Configured presets"},
			{Label: "vendor/model-a", Value: "vendor/model-a", Group: "Configured presets"},
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

	if indexOf("gemini") == -1 || indexOf("openrouter") == -1 || indexOf("local-api") == -1 {
		t.Fatalf("expected ready providers and Local API to appear in picker: %#v", items)
	}
	if indexOf("anthropic") == -1 {
		t.Fatalf("expected anthropic in picker")
	}
	if indexOf("gemini") > indexOf("anthropic") || indexOf("openrouter") > indexOf("anthropic") {
		t.Fatalf("ready remote providers should sort before unset direct providers")
	}
	if indexOf("local-api") > indexOf("anthropic") {
		t.Fatalf("Local API should sort ahead of unset direct providers")
	}
	if groupOf("anthropic") != "Needs setup" {
		t.Fatalf("unset direct provider group = %q, want Needs setup", groupOf("anthropic"))
	}
}

func TestProviderItemsHideCustomEndpointByDefault(t *testing.T) {
	items := providerItems(&config.Config{})
	for _, item := range items {
		if item.Value == "openai-compatible" {
			t.Fatalf("custom endpoint entry %q should be hidden by default", item.Value)
		}
	}
	foundLocal := false
	for _, item := range items {
		if item.Value == "local-api" && item.Label == "Local API" {
			foundLocal = true
			break
		}
	}
	if !foundLocal {
		t.Fatalf("Local API should always be visible")
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
	for _, item := range items {
		if item.Value == "openai-compatible" {
			t.Fatalf("custom endpoint entry should stay hidden when endpoint belongs to local-api")
		}
	}

	items = providerItems(&config.Config{Provider: "local-api", Endpoint: "http://127.0.0.1:1/v1"})
	found = false
	for _, item := range items {
		if item.Value == "local-api" && item.Label == "Local API" {
			if item.Detail != "Not running" {
				t.Fatalf("local-api detail = %q, want %q", item.Detail, "Not running")
			}
			found = true
			break
		}
	}
	if !found {
		t.Fatalf("local-api should render when active")
	}
}
