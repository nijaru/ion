package tool

import (
	"testing"
)

func TestSanitizeBinaryOutput(t *testing.T) {
	tests := []struct {
		name  string
		input string
		want  string
	}{
		{
			name:  "clean text unchanged",
			input: "hello world\nfoo\tbar",
			want:  "hello world\nfoo\tbar",
		},
		{
			name:  "strips null byte",
			input: "hello\x00world",
			want:  "helloworld",
		},
		{
			name:  "strips escape sequences",
			input: "\x1b[31mred\x1b[0m",
			want:  "[31mred[0m",
		},
		{
			name:  "strips bell and backspace",
			input: "beep\x07back\x08",
			want:  "beepback",
		},
		{
			name:  "keeps tab newline cr",
			input: "a\tb\nc\rd",
			want:  "a\tb\nc\rd",
		},
		{
			name:  "strips interlinear annotation anchors",
			input: "text\xef\xbf\xb9anno\xef\xbf\xbbmore",
			want:  "textannomore",
		},
		{
			name:  "empty string",
			input: "",
			want:  "",
		},
		{
			name:  "all control chars stripped",
			input: "\x00\x01\x02\x03\x04\x05\x06\x07\x08\x0b\x0c\x0e\x0f\x10\x11\x12\x13\x14\x15\x16\x17\x18\x19\x1a\x1b\x1c\x1d\x1e\x1f",
			want:  "",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := sanitizeBinaryOutput(tt.input)
			if got != tt.want {
				t.Errorf("sanitizeBinaryOutput(%q) = %q, want %q", tt.input, got, tt.want)
			}
		})
	}
}
