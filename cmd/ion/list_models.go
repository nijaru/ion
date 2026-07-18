package main

import (
	"context"
	"fmt"
	"io"
	"slices"
	"strconv"
	"strings"
	"unicode"

	"github.com/nijaru/ion/config"
	"github.com/nijaru/ion/llm"
)

func runListModels(
	ctx context.Context,
	w io.Writer,
	errW io.Writer,
	cfg *config.Config,
	search string,
	catalog *llm.ModelCatalog,
) error {
	if catalog == nil {
		return fmt.Errorf("model catalog is not configured")
	}
	result, err := catalog.QueryAvailableModels(ctx, llm.ModelCatalogQuery{Config: cfg})
	if err != nil && len(result.Models) == 0 {
		return err
	}

	for _, status := range result.Status {
		if status.Err != nil {
			fmt.Fprintf(errW, "Warning: %s model catalog unavailable: %v\n", status.Provider, status.Err)
		}
		if status.Stale {
			fmt.Fprintf(errW, "Warning: using stale cached models for %s\n", status.Provider)
		}
	}

	models := filterListModels(result.Models, search)
	if len(models) == 0 {
		if strings.TrimSpace(search) != "" {
			fmt.Fprintf(w, "No models matching %q\n", search)
		} else {
			fmt.Fprintln(w, "No models available. Configure provider credentials or a local model endpoint.")
		}
		return nil
	}

	writeModelTable(w, models)
	return nil
}

func filterListModels(models []llm.ModelMetadata, search string) []llm.ModelMetadata {
	query := strings.TrimSpace(search)
	if query == "" {
		return append([]llm.ModelMetadata(nil), models...)
	}

	filtered := make([]llm.ModelMetadata, 0, len(models))
	for _, model := range models {
		if fuzzyModelMatch(query, model.Provider+" "+model.ID) {
			filtered = append(filtered, model)
		}
	}
	return filtered
}

func fuzzyModelMatch(query, text string) bool {
	normalizedText := strings.ToLower(text)
	for _, token := range strings.FieldsFunc(strings.ToLower(query), func(r rune) bool {
		return unicode.IsSpace(r) || r == '/'
	}) {
		if !fuzzySubsequence(token, normalizedText) {
			return false
		}
	}
	return true
}

func fuzzySubsequence(query, text string) bool {
	if query == "" {
		return true
	}
	queryRunes := []rune(query)
	textRunes := []rune(text)
	if len(queryRunes) > len(textRunes) {
		return false
	}

	queryIndex := 0
	for _, candidate := range textRunes {
		if candidate == queryRunes[queryIndex] {
			queryIndex++
			if queryIndex == len(queryRunes) {
				return true
			}
		}
	}
	return false
}

func writeModelTable(w io.Writer, models []llm.ModelMetadata) {
	slices.SortFunc(models, func(a, b llm.ModelMetadata) int {
		if provider := strings.Compare(strings.ToLower(a.Provider), strings.ToLower(b.Provider)); provider != 0 {
			return provider
		}
		return strings.Compare(strings.ToLower(a.ID), strings.ToLower(b.ID))
	})

	rows := make([][]string, 0, len(models))
	for _, model := range models {
		rows = append(rows, []string{
			model.Provider,
			model.ID,
			formatModelTokenCount(model.ContextLimit),
			formatModelTokenCount(model.MaxTokens),
			yesNo(model.Reasoning),
			yesNo(containsImageInput(model.Input)),
		})
	}

	headers := []string{"provider", "model", "context", "max-out", "thinking", "images"}
	widths := make([]int, len(headers))
	for i, header := range headers {
		widths[i] = len(header)
	}
	for _, row := range rows {
		for i, value := range row {
			if len(value) > widths[i] {
				widths[i] = len(value)
			}
		}
	}

	writeModelTableRow(w, headers, widths)
	for _, row := range rows {
		writeModelTableRow(w, row, widths)
	}
}

func writeModelTableRow(w io.Writer, row []string, widths []int) {
	columns := make([]string, len(row))
	for i, value := range row {
		columns[i] = value
		if i != len(row)-1 {
			columns[i] = value + strings.Repeat(" ", widths[i]-len(value))
		}
	}
	fmt.Fprintln(w, strings.Join(columns, "  "))
}

func formatModelTokenCount(count int) string {
	switch {
	case count >= 1_000_000:
		return formatScaledModelCount(float64(count)/1_000_000, "M")
	case count >= 1_000:
		return formatScaledModelCount(float64(count)/1_000, "K")
	default:
		return strconv.Itoa(count)
	}
}

func formatScaledModelCount(value float64, suffix string) string {
	if value == float64(int(value)) {
		return fmt.Sprintf("%d%s", int(value), suffix)
	}
	return fmt.Sprintf("%.1f%s", value, suffix)
}

func containsImageInput(input []string) bool {
	for _, kind := range input {
		if strings.EqualFold(strings.TrimSpace(kind), "image") {
			return true
		}
	}
	return false
}

func yesNo(value bool) string {
	if value {
		return "yes"
	}
	return "no"
}
