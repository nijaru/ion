package workspace

import (
	"errors"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

func TestNewValidatorCanonicalizesRoot(t *testing.T) {
	dir := t.TempDir()
	resolved, err := filepath.EvalSymlinks(dir)
	if err != nil {
		t.Fatalf("EvalSymlinks: %v", err)
	}
	validator, err := NewValidator(dir)
	if err != nil {
		t.Fatalf("NewValidator: %v", err)
	}
	if validator.Base() != resolved {
		t.Fatalf("Base = %q, want %q", validator.Base(), resolved)
	}
}

func TestValidatorValidateRejectsMalformedAndEscapingPaths(t *testing.T) {
	dir := t.TempDir()
	validator, err := NewValidator(dir)
	if err != nil {
		t.Fatalf("NewValidator: %v", err)
	}

	cases := []struct {
		name string
		path string
		want error
	}{
		{name: "empty", path: "", want: ErrInvalidPath},
		{name: "nul", path: "bad\x00name", want: ErrInvalidPath},
		{name: "absolute", path: filepath.Join(dir, "file.txt"), want: ErrAbsolutePath},
		{name: "root ref", path: ".", want: ErrInvalidPath},
		{name: "traversal", path: "../secret", want: ErrPathTraversal},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			_, err := validator.Validate(tc.path)
			if !errors.Is(err, tc.want) {
				t.Fatalf("expected %v, got %v", tc.want, err)
			}
		})
	}
}

func TestValidatorValidateRejectsPathsThatAreTooDeep(t *testing.T) {
	dir := t.TempDir()
	validator, err := NewValidator(dir)
	if err != nil {
		t.Fatalf("NewValidator: %v", err)
	}
	validator.maxDepth = 2

	_, err = validator.Validate(filepath.Join("a", "b", "c"))
	if !errors.Is(err, ErrPathTooDeep) {
		t.Fatalf("expected ErrPathTooDeep, got %v", err)
	}
}

func TestValidatorValidateRejectsSymlinkEscapes(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("symlink permissions are environment-dependent on Windows")
	}

	dir := t.TempDir()
	outside := t.TempDir()
	if err := os.Mkdir(filepath.Join(dir, "nested"), 0o755); err != nil {
		t.Fatalf("Mkdir: %v", err)
	}
	if err := os.Symlink(outside, filepath.Join(dir, "nested", "escape")); err != nil {
		t.Fatalf("Symlink: %v", err)
	}

	validator, err := NewValidator(dir)
	if err != nil {
		t.Fatalf("NewValidator: %v", err)
	}
	_, err = validator.Validate(filepath.Join("nested", "escape", "file.txt"))
	if !errors.Is(err, ErrPathTraversal) {
		t.Fatalf("expected ErrPathTraversal, got %v", err)
	}
}

func TestValidatorValidateAllowsContainedRelativePaths(t *testing.T) {
	dir := t.TempDir()
	validator, err := NewValidator(dir)
	if err != nil {
		t.Fatalf("NewValidator: %v", err)
	}

	got, err := validator.Validate(filepath.Join("nested", "..", "file.txt"))
	if err != nil {
		t.Fatalf("Validate: %v", err)
	}
	if got != "file.txt" && !strings.HasSuffix(got, string(filepath.Separator)+"file.txt") {
		t.Fatalf("unexpected validated path %q", got)
	}
}
