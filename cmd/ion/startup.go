package main

import "strings"

func startupBannerLines(version, provider, model string, resumed bool) []string {
	version = strings.TrimSpace(version)
	provider = strings.TrimSpace(provider)

	if version == "" {
		version = "v0.0.0"
	}

	runtimeLabel := "native"
	switch {
	case isACPProvider(provider):
		runtimeLabel = "acp"
	}

	line := strings.Join([]string{"ion " + version, runtimeLabel}, " • ")
	if resumed {
		return []string{"--- resumed ---", line}
	}
	return []string{line}
}
