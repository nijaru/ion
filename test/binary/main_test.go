package binary_test

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"testing"
)

func TestBuiltBinaryPrintsThroughOpenAICompatibleProvider(t *testing.T) {
	var (
		mu       sync.Mutex
		requests int
		auth     string
		model    string
	)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost || r.URL.Path != "/v1/chat/completions" {
			http.Error(w, "unexpected request", http.StatusNotFound)
			return
		}

		var request struct {
			Model string `json:"model"`
		}
		if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
			http.Error(w, fmt.Sprintf("decode request: %v", err), http.StatusBadRequest)
			return
		}
		mu.Lock()
		requests++
		auth = r.Header.Get("Authorization")
		model = request.Model
		mu.Unlock()

		w.Header().Set("Content-Type", "text/event-stream")
		w.Header().Set("Cache-Control", "no-cache")
		_, _ = fmt.Fprint(
			w,
			"data: {\"id\":\"built-binary-test\",\"object\":\"chat.completion.chunk\",\"model\":\"fake-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"built-\"}}]}\n\n",
			"data: {\"id\":\"built-binary-test\",\"object\":\"chat.completion.chunk\",\"model\":\"fake-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"binary-ok\"},\"finish_reason\":\"stop\"}]}\n\n",
			"data: [DONE]\n\n",
		)
	}))
	defer server.Close()

	moduleRoot := filepath.Clean(filepath.Join(mustWorkingDirectory(t), "../.."))
	binaryPath := filepath.Join(t.TempDir(), "ion")
	build := exec.Command("go", "build", "-o", binaryPath, "./cmd/ion")
	build.Dir = moduleRoot
	if output, err := build.CombinedOutput(); err != nil {
		t.Fatalf("build Ion: %v\n%s", err, output)
	}

	home := t.TempDir()
	configDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(configDir, 0o700); err != nil {
		t.Fatal(err)
	}
	config := fmt.Sprintf(
		"provider = 'openai-compatible'\nmodel = 'fake-model'\nendpoint = '%s/v1'\nauth_env_var = 'ION_BINARY_TEST_KEY'\n",
		server.URL,
	)
	if err := os.WriteFile(filepath.Join(configDir, "config.toml"), []byte(config), 0o600); err != nil {
		t.Fatal(err)
	}

	command := exec.Command(binaryPath,
		"--no-session",
		"--print",
		"--timeout",
		"10s",
		"--prompt",
		"return the test marker",
	)
	command.Dir = moduleRoot
	command.Env = filteredEnvironment(
		"HOME="+home,
		"ION_BINARY_TEST_KEY=fake-key",
		"ION_PROVIDER=",
		"ION_MODEL=",
		"ION_REASONING_EFFORT=",
	)
	output, err := command.CombinedOutput()
	if err != nil {
		t.Fatalf("built Ion print: %v\n%s", err, output)
	}
	if got := string(output); !strings.Contains(got, "built-binary-ok") {
		t.Fatalf("built Ion output = %q, want streamed marker", got)
	}

	mu.Lock()
	gotRequests, gotAuth, gotModel := requests, auth, model
	mu.Unlock()
	if gotRequests != 1 {
		t.Fatalf("provider requests = %d, want 1", gotRequests)
	}
	if gotAuth != "Bearer fake-key" {
		t.Fatalf("authorization = %q, want bearer test key", gotAuth)
	}
	if gotModel != "fake-model" {
		t.Fatalf("model = %q, want fake-model", gotModel)
	}
}

func mustWorkingDirectory(t *testing.T) string {
	t.Helper()
	cwd, err := os.Getwd()
	if err != nil {
		t.Fatal(err)
	}
	return cwd
}

func filteredEnvironment(overrides ...string) []string {
	filtered := make([]string, 0, len(os.Environ())+len(overrides))
	for _, entry := range os.Environ() {
		if strings.HasPrefix(entry, "ION_PROVIDER=") ||
			strings.HasPrefix(entry, "ION_MODEL=") ||
			strings.HasPrefix(entry, "ION_REASONING_EFFORT=") ||
			strings.HasPrefix(entry, "HOME=") {
			continue
		}
		filtered = append(filtered, entry)
	}
	filtered = append(filtered, overrides...)
	return filtered
}
