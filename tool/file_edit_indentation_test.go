package tool

import (
	"testing"
)

func TestIndentationTolerantFuzzyMatch(t *testing.T) {
	content := `package main

func compute() int {
    a := 10
    b := 20
    return a + b
}
`

	// LLM provides old_string with 2-space indentation or stripped indentation
	oldString := `func compute() int {
  a := 10
  b := 20
  return a + b
}`

	newString := `func compute() int {
    a := 100
    b := 200
    return a + b
}`

	edits := []editReplacement{
		{
			OldString: oldString,
			NewString: newString,
		},
	}

	result, count, err := applyEditReplacements("main.go", content, edits)
	if err != nil {
		t.Fatalf("applyEdits failed: %v", err)
	}

	if count != 1 {
		t.Fatalf("expected 1 replacement, got %d", count)
	}

	expected := `package main

func compute() int {
    a := 100
    b := 200
    return a + b
}
`
	if result != expected {
		t.Fatalf("result = %q, want %q", result, expected)
	}
}
