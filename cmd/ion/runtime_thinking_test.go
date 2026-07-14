package main

import (
	"testing"

	"github.com/nijaru/ion/session"
)

func TestThinkingLevelForRuntime(t *testing.T) {
	tests := []struct {
		name  string
		value string
		want  session.ThinkingLevel
	}{
		{name: "auto uses provider default", value: "auto", want: ""},
		{name: "empty uses provider default", value: "", want: ""},
		{name: "minimal", value: "minimal", want: session.ThinkingMinimal},
		{name: "xhigh", value: "xhigh", want: session.ThinkingXHigh},
		{name: "provider-specific max", value: "max", want: "max"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := thinkingLevelForRuntime(tt.value); got != tt.want {
				t.Fatalf("thinkingLevelForRuntime(%q) = %q, want %q", tt.value, got, tt.want)
			}
		})
	}
}
