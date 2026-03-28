package main

import "strings"

func startupBannerLines(version, provider, model string, resumed bool) []string {
	version = strings.TrimSpace(version)

	if version == "" {
		version = "v0.0.0"
	}
	line := "ion " + version
	if resumed {
		return []string{"--- resumed ---", line}
	}
	return []string{line}
}
