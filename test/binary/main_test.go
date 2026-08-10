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

type binaryProviderRequest struct {
	Model    string            `json:"model"`
	Messages []json.RawMessage `json:"messages"`
	Tools    []struct {
		Function struct {
			Name string `json:"name"`
		} `json:"function"`
	} `json:"tools"`
}

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

func TestBuiltBinaryRunsBoundedSubagent(t *testing.T) {
	var (
		mu       sync.Mutex
		requests []binaryProviderRequest
	)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost || r.URL.Path != "/v1/chat/completions" {
			http.Error(w, "unexpected request", http.StatusNotFound)
			return
		}
		if r.Header.Get("Authorization") != "Bearer fake-key" {
			http.Error(w, "unexpected authorization", http.StatusUnauthorized)
			return
		}
		var body binaryProviderRequest
		if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
			http.Error(w, fmt.Sprintf("decode request: %v", err), http.StatusBadRequest)
			return
		}
		mu.Lock()
		requests = append(requests, body)
		call := len(requests)
		mu.Unlock()

		switch call {
		case 1:
			if !requestHasTool(body, "subagent") {
				http.Error(w, "parent request did not advertise subagent", http.StatusBadRequest)
				return
			}
			writeToolCallSSE(w)
		case 2:
			if requestHasTool(body, "subagent") {
				http.Error(w, "child request advertised recursive subagent", http.StatusBadRequest)
				return
			}
			writeTextSSE(w, "child-binary-ok")
		case 3:
			if !requestContainsMessage(body, "child-binary-ok") {
				http.Error(w, "parent follow-up omitted child result", http.StatusBadRequest)
				return
			}
			writeTextSSE(w, "parent-binary-ok")
		default:
			http.Error(w, "unexpected extra provider request", http.StatusBadRequest)
		}
	}))
	defer server.Close()

	binaryPath := buildIonBinary(t)
	home := t.TempDir()
	writeBinaryConfig(t, home, server.URL, "ION_BINARY_SUBAGENT_KEY")

	command := exec.Command(binaryPath,
		"--no-session",
		"--print",
		"--timeout",
		"10s",
		"--prompt",
		"delegate the evidence task",
	)
	command.Dir = filepath.Clean(filepath.Join(mustWorkingDirectory(t), "../.."))
	command.Env = filteredEnvironment(
		"HOME="+home,
		"ION_BINARY_SUBAGENT_KEY=fake-key",
		"ION_PROVIDER=",
		"ION_MODEL=",
		"ION_REASONING_EFFORT=",
	)
	output, err := command.CombinedOutput()
	if err != nil {
		t.Fatalf("built Ion subagent print: %v\n%s", err, output)
	}
	if !strings.Contains(string(output), "parent-binary-ok") {
		t.Fatalf("built Ion subagent output = %q, want parent result", output)
	}

	mu.Lock()
	gotRequests := len(requests)
	mu.Unlock()
	if gotRequests != 3 {
		t.Fatalf("provider requests = %d, want parent, child, and parent follow-up", gotRequests)
	}
}

func requestHasTool(body binaryProviderRequest, name string) bool {
	for _, candidate := range body.Tools {
		if candidate.Function.Name == name {
			return true
		}
	}
	return false
}

func requestContainsMessage(body binaryProviderRequest, needle string) bool {
	for _, message := range body.Messages {
		if strings.Contains(string(message), needle) {
			return true
		}
	}
	return false
}

func writeToolCallSSE(w http.ResponseWriter) {
	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache")
	_, _ = fmt.Fprint(
		w,
		"data: {\"id\":\"subagent-call\",\"object\":\"chat.completion.chunk\",\"model\":\"fake-model\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-subagent\",\"type\":\"function\",\"function\":{\"name\":\"subagent\",\"arguments\":\"{\\\"task\\\":\\\"return child evidence\\\"}\"}}]}}]}\n\n",
		"data: {\"id\":\"subagent-call\",\"object\":\"chat.completion.chunk\",\"model\":\"fake-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
		"data: [DONE]\n\n",
	)
}

func writeTextSSE(w http.ResponseWriter, content string) {
	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache")
	_, _ = fmt.Fprintf(
		w,
		"data: {\"id\":\"text-response\",\"object\":\"chat.completion.chunk\",\"model\":\"fake-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":%q}}]}\n\n",
		content,
	)
	_, _ = fmt.Fprint(
		w,
		"data: {\"id\":\"text-response\",\"object\":\"chat.completion.chunk\",\"model\":\"fake-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
		"data: [DONE]\n\n",
	)
}

func mustWorkingDirectory(t *testing.T) string {
	t.Helper()
	cwd, err := os.Getwd()
	if err != nil {
		t.Fatal(err)
	}
	return cwd
}

func buildIonBinary(t *testing.T) string {
	t.Helper()
	moduleRoot := filepath.Clean(filepath.Join(mustWorkingDirectory(t), "../.."))
	binaryPath := filepath.Join(t.TempDir(), "ion")
	build := exec.Command("go", "build", "-o", binaryPath, "./cmd/ion")
	build.Dir = moduleRoot
	if output, err := build.CombinedOutput(); err != nil {
		t.Fatalf("build Ion: %v\n%s", err, output)
	}
	return binaryPath
}

func writeBinaryConfig(t *testing.T, home, endpoint, authEnv string) {
	t.Helper()
	configDir := filepath.Join(home, ".ion")
	if err := os.MkdirAll(configDir, 0o700); err != nil {
		t.Fatal(err)
	}
	config := fmt.Sprintf(
		"provider = 'openai-compatible'\nmodel = 'fake-model'\nendpoint = '%s/v1'\nauth_env_var = '%s'\n",
		endpoint,
		authEnv,
	)
	if err := os.WriteFile(filepath.Join(configDir, "config.toml"), []byte(config), 0o600); err != nil {
		t.Fatal(err)
	}
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
