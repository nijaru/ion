package main

import "strings"

func startupBannerLines(provider, model string, resumed bool) []string {
	provider = strings.TrimSpace(provider)
	model = strings.TrimSpace(model)

	runtimeLabel := "native"
	switch {
	case isACPProvider(provider):
		runtimeLabel = "acp"
	}

	segments := []string{"ion", runtimeLabel}
	if provider != "" {
		segments = append(segments, "provider="+provider)
	}
	if model != "" {
		segments = append(segments, "model="+model)
	}
	line := strings.Join(segments, " · ")
	if resumed {
		return []string{"--- resumed ---", line}
	}
	return []string{line}
}
