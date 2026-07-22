package mcp

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"slices"
	"strings"
	"sync"
	"syscall"

	sdkmcp "github.com/modelcontextprotocol/go-sdk/mcp"
	"github.com/nijaru/ion/internal/workvfs"
	"github.com/nijaru/ion/tool"
)

// ServerConfig is the runtime-owned form of one configured MCP stdio server.
type ServerConfig struct {
	Name           string
	Command        string
	Args           []string
	Directory      string
	Env            map[string]string
	ProtectedPaths []string
	ReadPaths      []string
	WritablePaths  []string
	AllowNetwork   bool
}

// Runtime owns MCP client sessions and the subprocess lifetimes behind them.
// It is attached to the agent harness and never enters the session tree.
type Runtime struct {
	mu      sync.Mutex
	clients []*ownedClient
	tools   []tool.Tool
	closed  bool
}

// mcpParentWatchScript keeps a stdio server contained by the Ion process
// lifetime. The control pipe remains open in the parent; if Ion crashes, the
// watcher observes EOF and kills the whole process group. The server is kept
// in the same group so descendants are contained as well.
const mcpParentWatchScript = `( watcher() {
	IFS= read -r _ <&3 || kill -KILL -$$
}; watcher ) &
watcher=$!
exec 4<&0
"$@" <&4 >&1 2>&2 &
child=$!
exec 4<&-
wait "$child"
status=$?
kill "$watcher" 2>/dev/null || :
wait "$watcher" 2>/dev/null || :
exit "$status"`

// Open connects and discovers every configured server atomically. A failure
// closes all clients opened so far and returns no partially usable runtime.
func Open(ctx context.Context, workdir string, configs []ServerConfig) (*Runtime, error) {
	if ctx == nil {
		ctx = context.Background()
	}
	runtime := &Runtime{}
	seenServers := make(map[string]struct{}, len(configs))
	seenTools := make(map[string]struct{})
	for _, cfg := range configs {
		cfg.Name = strings.TrimSpace(cfg.Name)
		cfg.Command = strings.TrimSpace(cfg.Command)
		cfg.Directory = strings.TrimSpace(cfg.Directory)
		name, err := normalizeServerName(cfg.Name)
		if err != nil {
			runtime.Close()
			return nil, err
		}
		if _, exists := seenServers[name]; exists {
			runtime.Close()
			return nil, fmt.Errorf("mcp server %q is configured more than once", name)
		}
		seenServers[name] = struct{}{}
		if cfg.Command == "" {
			runtime.Close()
			return nil, fmt.Errorf("mcp server %q has no command", name)
		}
		if err := validateEnvironment(cfg.Env); err != nil {
			runtime.Close()
			return nil, fmt.Errorf("mcp server %q: %w", name, err)
		}

		serverCtx, cancel := context.WithCancel(context.Background())
		directory, err := resolveDirectory(workdir, cfg.Directory)
		if err != nil {
			cancel()
			runtime.Close()
			return nil, fmt.Errorf("mcp server %q: %w", name, err)
		}
		validator, err := workvfs.NewValidator(directory)
		if err != nil {
			cancel()
			runtime.Close()
			return nil, fmt.Errorf("mcp server %q workspace policy: %w", name, err)
		}
		directory = validator.Base()
		protectedPaths, protectedAbsolute, err := normalizeCapabilityPaths(validator, cfg.ProtectedPaths, "protected")
		if err != nil {
			cancel()
			runtime.Close()
			return nil, fmt.Errorf("mcp server %q: %w", name, err)
		}
		_, readAbsolute, err := normalizeCapabilityPaths(validator, cfg.ReadPaths, "read")
		if err != nil {
			cancel()
			runtime.Close()
			return nil, fmt.Errorf("mcp server %q: %w", name, err)
		}
		_, writableAbsolute, err := normalizeCapabilityPaths(validator, cfg.WritablePaths, "writable")
		if err != nil {
			cancel()
			runtime.Close()
			return nil, fmt.Errorf("mcp server %q: %w", name, err)
		}
		plan, err := tool.PlanSandboxedCommandWithPolicy(directory, cfg.Command, cfg.Args, tool.CurrentSandboxMode(), tool.SandboxPolicy{
			ReadPaths:      readAbsolute,
			WritePaths:     writableAbsolute,
			ProtectedPaths: protectedAbsolute,
			AllowNetwork:   cfg.AllowNetwork,
		})
		if err != nil {
			cancel()
			runtime.Close()
			return nil, fmt.Errorf("mcp server %q sandbox: %w", name, err)
		}
		childControl, parentControl, err := os.Pipe()
		if err != nil {
			cancel()
			runtime.Close()
			return nil, fmt.Errorf("mcp server %q parent-lifetime pipe: %w", name, err)
		}
		commandArgs := append([]string{"-c", mcpParentWatchScript, "ion-mcp-supervisor", plan.Name}, plan.Args...)
		command := exec.CommandContext(serverCtx, "/bin/sh", commandArgs...)
		command.Dir = plan.Dir
		command.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
		command.Env = overlayEnvironment(tool.NewEnvironmentPolicy("allowlist", nil).CommandEnvironment(), cfg.Env)
		command.ExtraFiles = []*os.File{childControl}
		client, err := NewClient(ctx, &sdkmcp.CommandTransport{Command: command}, "ion", "0.0.1")
		_ = childControl.Close()
		if err != nil {
			cancel()
			_ = parentControl.Close()
			_ = terminateProcessGroup(command)
			_ = callCleanup(plan.Cleanup)
			runtime.Close()
			return nil, fmt.Errorf("mcp server %q: %w", name, err)
		}
		client.WithIdentity(serverIdentity(name, cfg.Command, cfg.Args, command.Dir, cfg.Env, readAbsolute, writableAbsolute, cfg.AllowNetwork)).
			WithEnvironment(environmentKeys(cfg.Env)).
			WithNetworkIntent(tool.SandboxPolicyNetworkIntent(cfg.AllowNetwork))
		client.WithFilePolicy(&FilePolicy{
			Validator:       validator,
			ProtectedPaths:  protectedPaths,
			RequireApproval: true,
		})
		discovered, err := client.discoverTools(ctx, name)
		if err != nil {
			cancel()
			_ = client.Close()
			_ = parentControl.Close()
			_ = terminateProcessGroup(command)
			_ = callCleanup(plan.Cleanup)
			runtime.Close()
			return nil, fmt.Errorf("mcp server %q: %w", name, err)
		}
		for _, external := range discovered {
			toolName := external.Spec().Name
			if _, exists := seenTools[toolName]; exists {
				cancel()
				_ = client.Close()
				_ = terminateProcessGroup(command)
				_ = callCleanup(plan.Cleanup)
				runtime.Close()
				return nil, fmt.Errorf("mcp tool %q is exposed by more than one server", toolName)
			}
			seenTools[toolName] = struct{}{}
		}
		runtime.clients = append(runtime.clients, &ownedClient{
			client:        client,
			cancel:        cancel,
			command:       command,
			cleanup:       plan.Cleanup,
			parentControl: parentControl,
		})
		runtime.tools = append(runtime.tools, discovered...)
	}
	slices.SortFunc(runtime.tools, func(a, b tool.Tool) int {
		return cmpStrings(a.Spec().Name, b.Spec().Name)
	})
	return runtime, nil
}

func serverIdentity(name, command string, args []string, directory string, environment map[string]string, readPaths, writablePaths []string, allowNetwork bool) string {
	payload, _ := json.Marshal(struct {
		Name          string            `json:"name"`
		Command       string            `json:"command"`
		Directory     string            `json:"directory"`
		Args          []string          `json:"args"`
		Environment   map[string]string `json:"environment"`
		ReadPaths     []string          `json:"read_paths"`
		WritablePaths []string          `json:"writable_paths"`
		AllowNetwork  bool              `json:"allow_network"`
	}{Name: name, Command: command, Directory: directory, Args: args, Environment: environment, ReadPaths: readPaths, WritablePaths: writablePaths, AllowNetwork: allowNetwork})
	digest := sha256.Sum256(payload)
	return "mcp:server:" + name + ":" + hex.EncodeToString(digest[:8])
}

func normalizeCapabilityPaths(validator *workvfs.Validator, paths []string, kind string) (relative, absolute []string, err error) {
	if len(paths) == 0 {
		return nil, nil, nil
	}
	for _, raw := range paths {
		raw = strings.TrimSpace(raw)
		if raw == "" {
			continue
		}
		var rel string
		if raw == "." || raw == "./" {
			rel = "."
		} else {
			rel, err = validator.Validate(raw)
			if err != nil {
				return nil, nil, fmt.Errorf("invalid %s capability path %q: %w", kind, raw, err)
			}
		}
		relative = append(relative, rel)
		absolute = append(absolute, filepath.Join(validator.Base(), rel))
	}
	slices.Sort(relative)
	relative = slices.Compact(relative)
	slices.Sort(absolute)
	absolute = slices.Compact(absolute)
	return relative, absolute, nil
}

// Tools returns a stable snapshot of discovered external tools.
func (r *Runtime) Tools() []tool.Tool {
	if r == nil {
		return nil
	}
	return append([]tool.Tool(nil), r.tools...)
}

// Close terminates all client sessions and their stdio subprocesses.
func (r *Runtime) Close() error {
	if r == nil {
		return nil
	}
	r.mu.Lock()
	if r.closed {
		r.mu.Unlock()
		return nil
	}
	r.closed = true
	clients := append([]*ownedClient(nil), r.clients...)
	r.mu.Unlock()
	var errs []error
	for i := len(clients) - 1; i >= 0; i-- {
		if err := clients[i].Close(); err != nil {
			errs = append(errs, err)
		}
	}
	return joinErrors(errs)
}

// ownedClient keeps the process cancel function adjacent to the MCP client.
// Client.Close handles the protocol; cancel guarantees the command exits even
// when a server does not finish its transport handshake cleanly.
type ownedClient struct {
	client        *Client
	cancel        context.CancelFunc
	command       *exec.Cmd
	cleanup       func() error
	parentControl *os.File
	controlOnce   sync.Once
	controlErr    error
}

func (c *ownedClient) Close() error {
	if c == nil {
		return nil
	}
	if c.cancel != nil {
		c.cancel()
	}
	err := c.client.Close()
	// Canceling the command context intentionally terminates a stdio child;
	// the SDK may surface that expected process exit as an ExitError or a
	// context/connection cancellation.
	var exitErr *exec.ExitError
	if errors.As(err, &exitErr) ||
		errors.Is(err, context.Canceled) ||
		errors.Is(err, sdkmcp.ErrConnectionClosed) {
		err = nil
	}
	return errors.Join(err, c.closeParentControl(), terminateProcessGroup(c.command), callCleanup(c.cleanup))
}

func (c *ownedClient) closeParentControl() error {
	if c == nil || c.parentControl == nil {
		return nil
	}
	c.controlOnce.Do(func() { c.controlErr = c.parentControl.Close() })
	return c.controlErr
}

func terminateProcessGroup(command *exec.Cmd) error {
	if command == nil || command.Process == nil || command.Process.Pid <= 0 {
		return nil
	}
	if err := syscall.Kill(-command.Process.Pid, syscall.SIGKILL); err != nil && !errors.Is(err, syscall.ESRCH) {
		return fmt.Errorf("terminate MCP process group: %w", err)
	}
	return nil
}

func callCleanup(cleanup func() error) error {
	if cleanup == nil {
		return nil
	}
	return cleanup()
}

var serverNamePattern = regexp.MustCompile(`^[A-Za-z0-9_.-]+$`)

func normalizeServerName(value string) (string, error) {
	if value == "" || !serverNamePattern.MatchString(value) {
		return "", fmt.Errorf("invalid MCP server name %q", value)
	}
	return value, nil
}

func namespacedToolName(server, remote string) string {
	return "mcp_" + server + "_" + remote
}

func resolveDirectory(workdir, configured string) (string, error) {
	directory := configured
	if directory == "" {
		directory = workdir
	}
	if !filepath.IsAbs(directory) {
		directory = filepath.Join(workdir, directory)
	}
	info, err := os.Stat(directory)
	if err != nil {
		return "", fmt.Errorf("working directory %q: %w", directory, err)
	}
	if !info.IsDir() {
		return "", fmt.Errorf("working directory %q is not a directory", directory)
	}
	return filepath.Abs(directory)
}

func overlayEnvironment(base []string, overrides map[string]string) []string {
	if len(overrides) == 0 {
		return base
	}
	values := make(map[string]string, len(base)+len(overrides))
	for _, value := range base {
		key, _, ok := strings.Cut(value, "=")
		if ok {
			values[key] = value
		}
	}
	for key, value := range overrides {
		values[key] = key + "=" + value
	}
	result := make([]string, 0, len(values))
	for _, value := range values {
		result = append(result, value)
	}
	slices.Sort(result)
	return result
}

func environmentKeys(values map[string]string) []string {
	keys := make([]string, 0, len(values))
	for key := range values {
		keys = append(keys, key)
	}
	slices.Sort(keys)
	return keys
}

func validateEnvironment(values map[string]string) error {
	for key, value := range values {
		if key == "" || strings.ContainsRune(key, '=') || strings.ContainsRune(key, '\x00') {
			return fmt.Errorf("invalid environment variable name %q", key)
		}
		if strings.ContainsRune(value, '\x00') {
			return fmt.Errorf("environment variable %q contains a NUL byte", key)
		}
	}
	return nil
}

func cmpStrings(a, b string) int {
	switch {
	case a < b:
		return -1
	case a > b:
		return 1
	default:
		return 0
	}
}

func joinErrors(errs []error) error {
	var result error
	for _, err := range errs {
		result = errors.Join(result, err)
	}
	return result
}
