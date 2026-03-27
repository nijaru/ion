package main

import "strings"

func startupBannerLines(version, provider, model string, resumed bool) []string {
	version = strings.TrimSpace(version)
	provider = strings.TrimSpace(provider)
	model = strings.TrimSpace(model)

	if version == "" {
		version = "v0.0.0"
	}

	runtimeLabel := "native"
	switch {
	case isACPProvider(provider):
		runtimeLabel = "acp"
	}

	segments := []string{"ion " + version, runtimeLabel}
	if provider != "" {
		segments = append(segments, provider)
	}
	if model != "" {
		segments = append(segments, model)
	}
	line := strings.Join(segments, " • ")
	if resumed {
		return []string{"--- resumed ---", line}
	}
	return []string{line}
}
