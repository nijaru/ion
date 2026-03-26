package main

import (
	"fmt"
	"strings"
)

func startupBannerLines(provider, model string, resumed bool) []string {
	provider = strings.TrimSpace(provider)
	model = strings.TrimSpace(model)

	runtimeLabel := "native"
	switch {
	case provider == "":
		runtimeLabel = "no provider set"
	case isACPProvider(provider):
		runtimeLabel = "acp"
	}

	providerLabel := provider
	if providerLabel == "" {
		providerLabel = "no provider set"
	}

	modelLabel := model
	if modelLabel == "" {
		modelLabel = "no model set"
	}

	line := fmt.Sprintf("ion · %s · provider=%s · model=%s", runtimeLabel, providerLabel, modelLabel)
	if resumed {
		return []string{"--- resumed ---", line}
	}
	return []string{line}
}
