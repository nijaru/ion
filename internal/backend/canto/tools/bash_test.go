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

func TestBashBackgroundJobLifecycle(t *testing.T) {
	b := NewBash(t.TempDir())
	t.Cleanup(b.Close)

	start, err := b.Execute(
		t.Context(),
		`{"command":"printf start; sleep 10","background":true}`,
	)
	if err != nil {
		t.Fatalf("start background: %v", err)
	}
	if !strings.Contains(start, "background job bash-1 started") {
		t.Fatalf("start result = %q", start)
	}

	jobs := b.Jobs()
	if len(jobs) != 1 {
		t.Fatalf("jobs = %d, want 1", len(jobs))
	}
	if jobs[0].ID != "bash-1" || jobs[0].Status != "running" {
		t.Fatalf("job = %+v, want running bash-1", jobs[0])
	}

	var output string
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		output, err = b.Execute(t.Context(), `{"action":"output","job_id":"bash-1"}`)
		if err != nil {
			t.Fatalf("output background: %v", err)
		}
		if strings.Contains(output, "start") {
			break
		}
		time.Sleep(10 * time.Millisecond)
	}
	if !strings.Contains(output, "background job bash-1 running") ||
		!strings.Contains(output, "start") {
		t.Fatalf("output = %q, want status and command output", output)
	}

	stop, err := b.Execute(t.Context(), `{"action":"kill","job_id":"bash-1"}`)
	if err != nil {
		t.Fatalf("kill background: %v", err)
	}
	if stop != "background job bash-1 stopped" {
		t.Fatalf("stop = %q", stop)
	}
	if jobs := b.Jobs(); len(jobs) != 1 || jobs[0].Status != "stopped" {
		t.Fatalf("jobs after stop = %+v, want stopped job", jobs)
	}
}

func TestBashBackgroundOutputTail(t *testing.T) {
	b := NewBash(t.TempDir())
	t.Cleanup(b.Close)

	if _, err := b.Execute(
		t.Context(),
		`{"command":"printf 'one\ntwo\nthree\n'","background":true}`,
	); err != nil {
		t.Fatalf("start background: %v", err)
	}
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		jobs := b.Jobs()
		if len(jobs) == 1 && jobs[0].Status == "done" {
			break
		}
		time.Sleep(10 * time.Millisecond)
	}

	output, err := b.Execute(
		t.Context(),
		`{"action":"output","job_id":"bash-1","tail_lines":2}`,
	)
	if err != nil {
		t.Fatalf("output background: %v", err)
	}
	if !strings.HasSuffix(output, "two\nthree\n") {
		t.Fatalf("tail output = %q, want last two lines", output)
	}
}

func TestBashCloseStopsBackgroundJobs(t *testing.T) {
	b := NewBash(t.TempDir())
	if _, err := b.Execute(
		t.Context(),
		`{"command":"sleep 10","background":true}`,
	); err != nil {
		t.Fatalf("start background: %v", err)
	}

	done := make(chan struct{})
	go func() {
		b.Close()
		close(done)
	}()

	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("Close did not stop background job promptly")
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
