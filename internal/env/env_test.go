package env

import (
	"context"
	"path/filepath"
	"testing"
)

func TestDefaultFileSystem(t *testing.T) {
	tmpDir := t.TempDir()
	fs := NewDefaultFileSystem(tmpDir)

	t.Run("WriteAndRead", func(t *testing.T) {
		path := "test.txt"
		data := []byte("hello world")

		if err := fs.WriteFile(path, data, 0o644); err != nil {
			t.Fatalf("WriteFile: %v", err)
		}

		got, err := fs.ReadFile(path)
		if err != nil {
			t.Fatalf("ReadFile: %v", err)
		}

		if string(got) != string(data) {
			t.Errorf("got %q, want %q", got, data)
		}
	})

	t.Run("MkdirAll", func(t *testing.T) {
		dir := "nested/dir"

		if err := fs.MkdirAll(dir, 0o755); err != nil {
			t.Fatalf("MkdirAll: %v", err)
		}

		exists, err := fs.Exists(dir)
		if err != nil {
			t.Fatalf("Exists: %v", err)
		}
		if !exists {
			t.Error("directory should exist")
		}
	})

	t.Run("Remove", func(t *testing.T) {
		path := "to_remove.txt"
		if err := fs.WriteFile(path, []byte("data"), 0o644); err != nil {
			t.Fatalf("WriteFile: %v", err)
		}

		if err := fs.Remove(path); err != nil {
			t.Fatalf("Remove: %v", err)
		}

		exists, err := fs.Exists(path)
		if err != nil {
			t.Fatalf("Exists: %v", err)
		}
		if exists {
			t.Error("file should not exist")
		}
	})

	t.Run("Join", func(t *testing.T) {
		got := fs.Join("a", "b", "c")
		want := filepath.Join("a", "b", "c")
		if got != want {
			t.Errorf("got %q, want %q", got, want)
		}
	})

	t.Run("Abs", func(t *testing.T) {
		rel := "test.txt"
		abs, err := fs.Abs(rel)
		if err != nil {
			t.Fatalf("Abs: %v", err)
		}

		if !filepath.IsAbs(abs) {
			t.Errorf("expected absolute path, got %q", abs)
		}
	})
}

func TestDefaultShellExecutor(t *testing.T) {
	se := NewDefaultShellExecutor("", nil)

	t.Run("EchoCommand", func(t *testing.T) {
		result, err := se.Exec(context.Background(), "echo hello", nil)
		if err != nil {
			t.Fatalf("Exec: %v", err)
		}

		if result.ExitCode != 0 {
			t.Errorf("exit code %d, want 0", result.ExitCode)
		}

		if got := result.Stdout; got != "hello\n" {
			t.Errorf("stdout %q, want %q", got, "hello\n")
		}
	})

	t.Run("WithCwd", func(t *testing.T) {
		tmpDir := t.TempDir()
		result, err := se.Exec(context.Background(), "pwd", &ExecOptions{
			Cwd: tmpDir,
		})
		if err != nil {
			t.Fatalf("Exec: %v", err)
		}

		if result.ExitCode != 0 {
			t.Errorf("exit code %d, want 0", result.ExitCode)
		}
	})

	t.Run("ExitCode", func(t *testing.T) {
		result, err := se.Exec(context.Background(), "exit 1", nil)
		if err != nil {
			t.Fatalf("Exec: %v", err)
		}

		if result.ExitCode != 1 {
			t.Errorf("exit code %d, want 1", result.ExitCode)
		}
	})

	t.Run("WithEnv", func(t *testing.T) {
		result, err := se.Exec(context.Background(), "echo $ION_TEST_VAR", &ExecOptions{
			Env: map[string]string{"ION_TEST_VAR": "test_value"},
		})
		if err != nil {
			t.Fatalf("Exec: %v", err)
		}

		if result.ExitCode != 0 {
			t.Errorf("exit code %d, want 0", result.ExitCode)
		}

		if got := result.Stdout; got != "test_value\n" {
			t.Errorf("stdout %q, want %q", got, "test_value\n")
		}
	})
}

func TestFileErrors(t *testing.T) {
	tmpDir := t.TempDir()
	fs := NewDefaultFileSystem(tmpDir)

	t.Run("ReadNonExistent", func(t *testing.T) {
		_, err := fs.ReadFile("nonexistent.txt")
		if err == nil {
			t.Fatal("expected error")
		}

		fileErr, ok := err.(*FileError)
		if !ok {
			t.Fatalf("expected *FileError, got %T", err)
		}

		if fileErr.Code != "read_error" {
			t.Errorf("code %q, want %q", fileErr.Code, "read_error")
		}
	})
}

func TestExecutionErrors(t *testing.T) {
	se := NewDefaultShellExecutor("", nil)

	t.Run("ContextCancelled", func(t *testing.T) {
		ctx, cancel := context.WithCancel(context.Background())
		cancel()

		_, err := se.Exec(ctx, "sleep 1", nil)
		if err == nil {
			t.Fatal("expected error")
		}
	})
}
