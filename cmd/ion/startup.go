package main

import "strings"

func startupBannerLines(version, provider, model string, resumed bool) []string {
	version = strings.TrimSpace(version)

	if version == "" {
		version = "v0.0.0"
	}
	return []string{"ion " + version}
}
