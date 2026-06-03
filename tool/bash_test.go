package tool_test

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/nijaru/ion/tool"
	"github.com/nijaru/ion/internal/tracing"
)

func TestBash_Spec(t *testing.T) {
	b := tool.NewBash(".")
	spec := b.Spec()
	if spec.Name != "bash" {
		t.Errorf("expected name bash, got %s", spec.Name)
	}
	if spec.Parameters == nil {
		t.Error("expected parameters, got nil")
	}
}

func TestBashSpecHidesBackgroundJobsByDefault(t *testing.T) {
	params := bashSpecParameters(t, tool.NewBash("."))
	properties := bashSpecProperties(t, params)
	for _, key := range []string{"action", "background", "job_id", "tail_lines"} {
		if _, ok := properties[key]; ok {
			t.Fatalf("default bash spec exposes %q: %#v", key, properties)
		}
	}
	if _, ok := properties["command"]; !ok {
		t.Fatalf("default bash spec missing command: %#v", properties)
	}
	if _, ok := properties["timeout"]; !ok {
		t.Fatalf("default bash spec missing timeout: %#v", properties)
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
	b := tool.NewBash(t.TempDir())
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
	b := tool.NewBash(tmpDir)

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

func TestBashExecuteDoesNotWaitForDetachedChildHoldingStdout(t *testing.T) {
	tmpDir := t.TempDir()
	b := tool.NewBash(tmpDir)

	start := time.Now()
	res, err := b.Execute(t.Context(), `{"command":"printf done; (sleep 1; touch survived) &"}`)
	if err != nil {
		t.Fatalf("execute failed: %v", err)
	}
	if elapsed := time.Since(start); elapsed > time.Second {
		t.Fatalf("detached child held command for %s; output=%q", elapsed, res)
	}
	if strings.TrimSpace(res) != "done" {
		t.Fatalf("output = %q, want done", res)
	}
	time.Sleep(1200 * time.Millisecond)
	if _, err := os.Stat(filepath.Join(tmpDir, "survived")); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("detached child marker stat err = %v, want not exist", err)
	}
}

func TestBashTimeoutKillsProcessGroup(t *testing.T) {
	tmpDir := t.TempDir()
	b := tool.NewBash(tmpDir)

	start := time.Now()
	_, err := b.Execute(t.Context(), `{"command":"sleep 10 & wait","timeout":0.1}`)
	if err == nil || !strings.Contains(err.Error(), "timeout after") {
		t.Fatalf("timeout error = %v, want timeout after", err)
	}
	if elapsed := time.Since(start); elapsed > 2*time.Second {
		t.Fatalf("timed-out command took %s, want prompt process-group cleanup", elapsed)
	}
}

func TestBash_Execute(t *testing.T) {
	tmpDir := t.TempDir()
	b := tool.NewBash(tmpDir)

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
	b := tool.NewBash(tmpDir)

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

func TestBashExecuteStreamingEmitsTruncatedSnapshot(t *testing.T) {
	b := tool.NewBash(t.TempDir())
	args := largeLineOutputCommandArgs(2105)

	var chunks []string
	for chunk, err := range b.ExecuteStreaming(t.Context(), args) {
		if err != nil {
			t.Fatalf("execute streaming failed: %v", err)
		}
		chunks = append(chunks, chunk)
	}
	got := strings.Join(chunks, "")
	if !strings.Contains(got, "line-2105") ||
		!strings.Contains(got, "Full output:") {
		t.Fatalf("streaming output missing final snapshot: %q", tailForTest(got, 300))
	}
}

func TestBashExecuteStreamingUpdatesEmitTailSnapshotWhenTruncated(t *testing.T) {
	b := tool.NewBash(t.TempDir())
	assertStreamingUpdateTailSnapshot(t, b)
}

func TestBashExecuteStreamingUpdatesThroughTracingEmitTailSnapshotWhenTruncated(t *testing.T) {
	b := tool.NewBash(t.TempDir())
	wrapped, ok := tracing.WrapTool(b).(tool.StreamingUpdateTool)
	if !ok {
		t.Fatal("wrapped bash does not implement StreamingUpdateTool")
	}
	assertStreamingUpdateTailSnapshot(t, wrapped)
}

func TestBashExecuteReturnsTailAndFullOutputPathWhenTruncated(t *testing.T) {
	b := tool.NewBash(t.TempDir())
	args := largeLineOutputCommandArgs(2105)
	got, err := b.Execute(t.Context(), args)
	if err != nil {
		t.Fatalf("execute failed: %v", err)
	}
	assertBashTailTruncation(t, got)
}

func assertStreamingUpdateTailSnapshot(t *testing.T, b tool.StreamingUpdateTool) {
	t.Helper()
	args := largeLineOutputCommandArgs(2105)

	var finalSnapshot string
	sawSnapshot := false
	for update, err := range b.ExecuteStreamingUpdates(t.Context(), args) {
		if err != nil {
			t.Fatalf("execute streaming updates failed: %v", err)
		}
		if update.Snapshot {
			sawSnapshot = true
			finalSnapshot = update.Text
		}
	}
	if !sawSnapshot {
		t.Fatal("streaming updates did not emit a snapshot after truncation")
	}
	assertBashTailTruncation(t, finalSnapshot)
}

func largeLineOutputCommandArgs(lines int) string {
	return fmt.Sprintf(
		`{"command":"awk 'BEGIN { for (i = 1; i <= %d; i++) print \"line-\" i }'"}`,
		lines,
	)
}

func assertBashTailTruncation(t *testing.T, got string) {
	t.Helper()
	if strings.Contains(got, "line-1\n") || strings.Contains(got, "line-105\n") {
		t.Fatalf("output kept head instead of tail: %q", tailForTest(got, 300))
	}
	if !strings.Contains(got, "line-106\n") || !strings.Contains(got, "line-2105") {
		t.Fatalf("output missing expected tail lines: %q", tailForTest(got, 300))
	}
	if !strings.Contains(got, "[Showing lines 106-2105 of 2105. Full output: ") {
		t.Fatalf("output missing Pi-style tail notice: %q", tailForTest(got, 300))
	}
	fullOutputPath := fullOutputPathFromNotice(t, got)
	defer func() { _ = os.Remove(fullOutputPath) }()
	fullOutput, err := os.ReadFile(fullOutputPath)
	if err != nil {
		t.Fatalf("read full output file %q: %v", fullOutputPath, err)
	}
	if !strings.Contains(string(fullOutput), "line-1\n") ||
		!strings.Contains(string(fullOutput), "line-2105\n") {
		t.Fatalf(
			"full output file missing original content: %q",
			tailForTest(string(fullOutput), 300),
		)
	}
	if len(got) > tool.MaxToolOutputSize+512 {
		t.Fatalf("output length = %d, want bounded output", len(got))
	}
}

func fullOutputPathFromNotice(t *testing.T, output string) string {
	t.Helper()
	const marker = "Full output: "
	start := strings.LastIndex(output, marker)
	if start == -1 {
		t.Fatalf("output missing full output marker: %q", tailForTest(output, 300))
	}
	pathStart := start + len(marker)
	pathEnd := strings.IndexByte(output[pathStart:], ']')
	if pathEnd == -1 {
		t.Fatalf("output missing full output closing bracket: %q", tailForTest(output, 300))
	}
	return output[pathStart : pathStart+pathEnd]
}

func tailForTest(s string, max int) string {
	if len(s) <= max {
		return s
	}
	return s[len(s)-max:]
}

func TestBashStripsProviderCredentialsWhenConfigured(t *testing.T) {
	t.Setenv("OPENAI_API_KEY", "secret")
	t.Setenv("ION_TEST_VISIBLE", "visible")

	b := tool.NewBashWithEnvironment(
		t.TempDir(),
		tool.NewEnvironmentPolicy("inherit_without_provider_keys", []string{"OPENAI_API_KEY"}),
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
	got := tool.FilterEnvironment(
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

	b := tool.NewBash(tmpDir)
	args := `{"command": "ls -d testdir"}`
	res, err := b.Execute(context.Background(), args)
	if err != nil {
		t.Fatalf("execute failed: %v", err)
	}
	if strings.TrimSpace(res) != subdir {
		t.Errorf("expected %s, got %q", subdir, res)
	}
}

func bashSpecParameters(t *testing.T, b *tool.Bash) map[string]any {
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
