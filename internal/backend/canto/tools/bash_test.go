package tools

import (
	"context"
	"os"
	"strings"
	"testing"
	"time"
)

func TestBash_Spec(t *testing.T) {
	b := NewBash(".")
	spec := b.Spec()
	if spec.Name != "bash" {
		t.Errorf("expected name bash, got %s", spec.Name)
	}
	if spec.Parameters == nil {
		t.Error("expected parameters, got nil")
	}
}

func TestBashCancellationKillsProcessGroup(t *testing.T) {
	tmpDir := t.TempDir()
	b := NewBash(tmpDir)

	ctx, cancel := context.WithTimeout(context.Background(), 100*time.Millisecond)
	defer cancel()
	start := time.Now()
	_, err := b.Execute(ctx, `{"command":"sleep 10 & wait"}`)
	if err == nil {
		t.Fatal("expected canceled command to fail")
	}
	if elapsed := time.Since(start); elapsed > 2*time.Second {
		t.Fatalf("canceled command took %s, want prompt process-group cleanup", elapsed)
	}
}

func TestBash_Execute(t *testing.T) {
	tmpDir := t.TempDir()
	b := NewBash(tmpDir)

	t.Run("echo command", func(t *testing.T) {
		args := `{"command": "echo 'hello world'"}`
		res, err := b.Execute(context.Background(), args)
		if err != nil {
			t.Fatalf("execute failed: %v", err)
		}
		if strings.TrimSpace(res) != "hello world" {
			t.Errorf("expected hello world, got %q", res)
		}
	})

	t.Run("streaming output", func(t *testing.T) {
		args := `{"command": "echo 'line1'; echo 'line2'"}`
		var chunks []string
		res, err := b.ExecuteStreaming(context.Background(), args, func(chunk string) error {
			chunks = append(chunks, chunk)
			return nil
		})
		if err != nil {
			t.Fatalf("execute streaming failed: %v", err)
		}
		if len(chunks) == 0 {
			t.Error("expected at least one chunk, got zero")
		}
		if !strings.Contains(res, "line1") || !strings.Contains(res, "line2") {
			t.Errorf("unexpected output: %q", res)
		}
	})

	t.Run("error command", func(t *testing.T) {
		args := `{"command": "nonexistentcommand"}`
		res, err := b.Execute(context.Background(), args)
		t.Logf("res: %q, err: %v", res, err)
		if err == nil {
			t.Fatal("expected non-zero command to return an error")
		}
		if !strings.Contains(res, "nonexistentcommand") {
			t.Errorf("expected command stderr in result, got: %q", res)
		}
	})

	t.Run("empty command", func(t *testing.T) {
		if _, err := b.Execute(context.Background(), `{"command":" "}`); err == nil {
			t.Fatal("expected empty command to fail")
		}
	})
}

func TestBash_WorkingDirectory(t *testing.T) {
	tmpDir := t.TempDir()
	subdir := "testdir"
	os.Mkdir(tmpDir+"/"+subdir, 0755)

	b := NewBash(tmpDir)
	args := `{"command": "ls -d testdir"}`
	res, err := b.Execute(context.Background(), args)
	if err != nil {
		t.Fatalf("execute failed: %v", err)
	}
	if strings.TrimSpace(res) != subdir {
		t.Errorf("expected %s, got %q", subdir, res)
	}
}
