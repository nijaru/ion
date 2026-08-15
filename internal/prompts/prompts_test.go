package prompts

import (
	"context"
	"os"
	"path/filepath"
	"testing"
)

func TestParseCommandArgs(t *testing.T) {
	tests := []struct {
		input string
		want  []string
	}{
		{"", nil},
		{"hello world", []string{"hello", "world"}},
		{`"hello world" 'foo bar' baz`, []string{"hello world", "foo bar", "baz"}},
		{`--file "my test.txt" -v`, []string{"--file", "my test.txt", "-v"}},
	}

	for _, tt := range tests {
		got := ParseCommandArgs(tt.input)
		if len(got) != len(tt.want) {
			t.Fatalf("ParseCommandArgs(%q) len = %d, want %d (%v vs %v)", tt.input, len(got), len(tt.want), got, tt.want)
		}
		for i := range got {
			if got[i] != tt.want[i] {
				t.Errorf("ParseCommandArgs(%q)[%d] = %q, want %q", tt.input, i, got[i], tt.want[i])
			}
		}
	}
}

func TestSubstituteArgs(t *testing.T) {
	tests := []struct {
		content string
		args    []string
		want    string
	}{
		{
			content: "Review $1 and $2",
			args:    []string{"file.go", "tests.go"},
			want:    "Review file.go and tests.go",
		},
		{
			content: "Run: $@",
			args:    []string{"npm", "test", "--watch"},
			want:    "Run: npm test --watch",
		},
		{
			content: "Arguments: $ARGUMENTS",
			args:    []string{"foo", "bar"},
			want:    "Arguments: foo bar",
		},
		{
			content: "Target: ${1:-all}",
			args:    nil,
			want:    "Target: all",
		},
		{
			content: "Target: ${1:-all}",
			args:    []string{"cmd/ion"},
			want:    "Target: cmd/ion",
		},
		{
			content: "Flags: ${@:-none}",
			args:    nil,
			want:    "Flags: none",
		},
		{
			content: "Rest: ${@:2}",
			args:    []string{"subcmd", "arg1", "arg2"},
			want:    "Rest: arg1 arg2",
		},
		{
			content: "Slice: ${@:2:1}",
			args:    []string{"subcmd", "arg1", "arg2"},
			want:    "Slice: arg1",
		},
	}

	for _, tt := range tests {
		got := SubstituteArgs(tt.content, tt.args)
		if got != tt.want {
			t.Errorf("SubstituteArgs(%q, %v) = %q, want %q", tt.content, tt.args, got, tt.want)
		}
	}
}

func TestDiscoverPromptsAndExpand(t *testing.T) {
	tempDir := t.TempDir()
	promptFile := filepath.Join(tempDir, "review.md")
	content := `---
description: Review changes in a file
argument-hint: [file]
---
Please review the changes in $1 carefully with focus on correctness.`

	if err := os.WriteFile(promptFile, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	templates, err := DiscoverPrompts(context.Background(), tempDir)
	if err != nil {
		t.Fatal(err)
	}
	if len(templates) != 1 {
		t.Fatalf("DiscoverPrompts len = %d, want 1", len(templates))
	}
	if templates[0].Name != "review" {
		t.Errorf("template Name = %q, want review", templates[0].Name)
	}
	if templates[0].Description != "Review changes in a file" {
		t.Errorf("template Description = %q", templates[0].Description)
	}
	if templates[0].ArgumentHint != "[file]" {
		t.Errorf("template ArgumentHint = %q", templates[0].ArgumentHint)
	}

	expanded, ok := ExpandPromptTemplate("/review main.go", templates)
	if !ok {
		t.Fatal("expected ExpandPromptTemplate to return true")
	}
	want := "Please review the changes in main.go carefully with focus on correctness."
	if expanded != want {
		t.Errorf("expanded = %q, want %q", expanded, want)
	}
}
