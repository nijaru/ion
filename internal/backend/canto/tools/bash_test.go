package tools

import (
	"context"
	"errors"
	"os"
	"path/filepath"
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

func TestBashSpecHidesBackgroundJobsByDefault(t *testing.T) {
	params := bashSpecParameters(t, NewBash("."))
	properties := bashSpecProperties(t, params)
	for _, key := range []string{"action", "background", "job_id", "tail_lines"} {
		if _, ok := properties[key]; ok {
			t.Fatalf("default bash spec exposes %q: %#v", key, properties)
		}
	}
	if _, ok := properties["command"]; !ok {
		t.Fatalf("default bash spec missing command: %#v", properties)
	}
	required, ok := params["required"].([]string)
	if !ok {
		t.Fatalf("bash spec required = %T, want []string", params["required"])
	}
	if strings.Join(required, ",") != "command" {
		t.Fatalf("bash spec required = %#v, want command", required)
	}
}

func TestBashRejectsDeferredBackgroundJobArgs(t *testing.T) {
	b := NewBash(t.TempDir())
	for _, args := range []string{
		`{"command":"sleep 10","background":true}`,
		`{"action":"output","job_id":"bash-1"}`,
		`{"action":"kill","job_id":"bash-1"}`,
		`{"command":"echo ok","tail_lines":10}`,
	} {
		_, err := b.Execute(t.Context(), args)
		if err == nil || !strings.Contains(err.Error(), "background jobs are deferred") {
			t.Fatalf("Execute(%s) error = %v, want deferred background jobs", args, err)
		}
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
		var execErr error
		for chunk, err := range b.ExecuteStreaming(context.Background(), args) {
			if err != nil {
				execErr = err
				break
			}
			chunks = append(chunks, chunk)
		}
		if execErr != nil {
			t.Fatalf("execute streaming failed: %v", execErr)
		}
		if len(chunks) == 0 {
			t.Error("expected at least one chunk, got zero")
		}
		res := strings.Join(chunks, "")
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

func TestBashExecuteStreamingStopsCommandWhenConsumerStops(t *testing.T) {
	tmpDir := t.TempDir()
	marker := filepath.Join(tmpDir, "survived")
	b := NewBash(tmpDir)

	for chunk, err := range b.ExecuteStreaming(
		t.Context(),
		`{"command":"printf 'start\n'; sleep 1; touch survived"}`,
	) {
		if err != nil {
			t.Fatalf("execute streaming failed: %v", err)
		}
		if !strings.Contains(chunk, "start") {
			t.Fatalf("first chunk = %q, want start", chunk)
		}
		break
	}

	time.Sleep(1500 * time.Millisecond)
	if _, err := os.Stat(marker); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("marker stat err = %v, want not exist", err)
	}
}

func TestBashStripsProviderCredentialsWhenConfigured(t *testing.T) {
	t.Setenv("OPENAI_API_KEY", "secret")
	t.Setenv("ION_TEST_VISIBLE", "visible")

	b := NewBashWithEnvironment(
		t.TempDir(),
		NewEnvironmentPolicy("inherit_without_provider_keys", []string{"OPENAI_API_KEY"}),
	)
	res, err := b.Execute(
		context.Background(),
		`{"command":"printf '%s:%s' \"$OPENAI_API_KEY\" \"$ION_TEST_VISIBLE\""}`,
	)
	if err != nil {
		t.Fatalf("execute failed: %v", err)
	}
	if strings.TrimSpace(res) != ":visible" {
		t.Fatalf("output = %q, want provider key stripped and normal env preserved", res)
	}
}

func TestFilterEnvironmentPreservesNonCredentials(t *testing.T) {
	got := filterEnvironment(
		[]string{"OPENAI_API_KEY=secret", "PATH=/bin", "BROKEN", "OPENAI_API_KEY_EXTRA=keep"},
		map[string]struct{}{"OPENAI_API_KEY": {}},
	)
	want := []string{"PATH=/bin", "BROKEN", "OPENAI_API_KEY_EXTRA=keep"}
	if strings.Join(got, "\n") != strings.Join(want, "\n") {
		t.Fatalf("env = %#v, want %#v", got, want)
	}
}

func TestBash_WorkingDirectory(t *testing.T) {
	tmpDir := t.TempDir()
	subdir := "testdir"
	os.Mkdir(tmpDir+"/"+subdir, 0o755)

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

func bashSpecParameters(t *testing.T, b *Bash) map[string]any {
	t.Helper()
	params, ok := b.Spec().Parameters.(map[string]any)
	if !ok {
		t.Fatalf("bash spec parameters = %T, want map[string]any", b.Spec().Parameters)
	}
	return params
}

func bashSpecProperties(t *testing.T, params map[string]any) map[string]any {
	t.Helper()
	properties, ok := params["properties"].(map[string]any)
	if !ok {
		t.Fatalf("bash spec properties = %T, want map[string]any", params["properties"])
	}
	return properties
}
