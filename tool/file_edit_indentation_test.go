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

func TestIndentationTolerantFuzzyMatchCRLF(t *testing.T) {
	crlfContent := "package main\r\n\r\nfunc run() {\r\n    x := 1\r\n    y := 2\r\n}\r\n"
	oldString := "func run() {\n  x := 1\n  y := 2\n}"
	newString := "func run() {\n    x := 10\n    y := 20\n}"

	edits := []editReplacement{
		{
			OldString: oldString,
			NewString: newString,
		},
	}

	result, count, err := applyEditReplacements("main.go", crlfContent, edits)
	if err != nil {
		t.Fatalf("applyEditReplacements failed on CRLF: %v", err)
	}
	if count != 1 {
		t.Fatalf("expected 1 replacement, got %d", count)
	}
	expected := "package main\r\n\r\nfunc run() {\r\n    x := 10\r\n    y := 20\r\n}\r\n"
	if result != expected {
		t.Fatalf("result = %q, want %q", result, expected)
	}
}

func TestIndentationTolerantFuzzyMatchTabsVsSpaces(t *testing.T) {
	tabContent := "package main\n\nfunc run() {\n\tx := 1\n\ty := 2\n}\n"
	// Old string provided with spaces instead of tabs
	oldString := "func run() {\n    x := 1\n    y := 2\n}"
	newString := "func run() {\n\tx := 10\n\ty := 20\n}"

	edits := []editReplacement{
		{
			OldString: oldString,
			NewString: newString,
		},
	}

	result, count, err := applyEditReplacements("main.go", tabContent, edits)
	if err != nil {
		t.Fatalf("applyEditReplacements failed on tabs: %v", err)
	}
	if count != 1 {
		t.Fatalf("expected 1 replacement, got %d", count)
	}
	expected := "package main\n\nfunc run() {\n\tx := 10\n\ty := 20\n}\n"
	if result != expected {
		t.Fatalf("result = %q, want %q", result, expected)
	}
}

func TestIndentationTolerantFuzzyMatchNonMatchingError(t *testing.T) {
	content := "package main\n\nfunc run() {\n    x := 1\n}\n"
	oldString := "func completelyDifferent() {\n    z := 999\n}"
	newString := "func run() {}"

	edits := []editReplacement{
		{
			OldString: oldString,
			NewString: newString,
		},
	}

	_, _, err := applyEditReplacements("main.go", content, edits)
	if err == nil {
		t.Fatal("expected error when old_string is not found in file")
	}
}
