package tool

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"slices"
	"strconv"
	"strings"
)

type SandboxMode string

const (
	SandboxOff        SandboxMode = "off"
	SandboxAuto       SandboxMode = "auto"
	SandboxSeatbelt   SandboxMode = "seatbelt"
	SandboxBubblewrap SandboxMode = "bubblewrap"
)

var (
	sandboxGOOS       = runtime.GOOS
	sandboxLookPath   = exec.LookPath
	sandboxPathExists = func(path string) bool {
		_, err := os.Stat(path)
		return err == nil
	}
)

type sandboxPlan struct {
	name    string
	args    []string
	dir     string
	cleanup func() error
}

// SandboxCommandPlan is the host-owned command construction result for a
// sandboxed subprocess. Cleanup must run after the process exits.
type SandboxCommandPlan struct {
	Name    string
	Args    []string
	Dir     string
	Cleanup func() error
}

// SandboxPolicy describes the capabilities that a subprocess is allowed to
// use. It is intentionally narrower than the host's general shell policy:
// callers that need a per-process boundary must opt into this planner instead
// of inheriting a broad workspace mount.
//
// WritePaths and ProtectedPaths are absolute or cwd-relative paths. Empty
// WritePaths means the workspace is read-only. Network access is denied
// unless AllowNetwork is true.
type SandboxPolicy struct {
	ReadPaths      []string
	WritePaths     []string
	ProtectedPaths []string
	AllowNetwork   bool
}

// PlanSandboxedCommandWithPolicy builds a command whose declared capabilities
// are enforced by the selected OS sandbox. Unlike PlanSandboxedCommand, it
// never accepts SandboxOff: an explicit unsandboxed host mode cannot satisfy a
// narrower per-process policy.
func PlanSandboxedCommandWithPolicy(
	cwd, name string,
	args []string,
	mode SandboxMode,
	policy SandboxPolicy,
) (SandboxCommandPlan, error) {
	plan, err := planSandboxedCommandWithPolicy(cwd, name, args, mode, policy)
	if err != nil {
		return SandboxCommandPlan{}, err
	}
	return SandboxCommandPlan{
		Name:    plan.name,
		Args:    append([]string(nil), plan.args...),
		Dir:     plan.dir,
		Cleanup: plan.cleanup,
	}, nil
}

// CurrentSandboxMode resolves the configured technical boundary. Invalid
// values remain invalid so callers fail closed instead of silently selecting a
// weaker backend.
func CurrentSandboxMode() SandboxMode {
	return resolveSandboxMode()
}

// PlanSandboxedCommand builds a sandboxed command without starting it. It is
// used by host-owned subprocesses such as MCP servers as well as Bash.
func PlanSandboxedCommand(cwd, name string, args []string, mode SandboxMode) (SandboxCommandPlan, error) {
	plan, err := planSandboxedCommand(cwd, name, args, mode)
	if err != nil {
		return SandboxCommandPlan{}, err
	}
	return SandboxCommandPlan{
		Name: plan.name, Args: append([]string(nil), plan.args...), Dir: plan.dir, Cleanup: plan.cleanup,
	}, nil
}

func resolveSandboxMode() SandboxMode {
	raw := strings.ToLower(strings.TrimSpace(os.Getenv("ION_SANDBOX")))
	switch raw {
	case "":
		return SandboxAuto
	case string(SandboxAuto):
		return SandboxAuto
	case string(SandboxOff):
		return SandboxOff
	case string(SandboxSeatbelt):
		return SandboxSeatbelt
	case string(SandboxBubblewrap):
		return SandboxBubblewrap
	default:
		return SandboxMode("invalid")
	}
}

func SandboxSummary() string {
	return sandboxSummary(resolveSandboxMode())
}

func sandboxNetworkIntent(mode SandboxMode) string {
	if mode == SandboxOff {
		return "unrestricted"
	}
	return "denied-by-sandbox"
}

// SandboxNetworkIntent returns the network capability represented by a
// sandbox mode. Action planners record this value in the approval fingerprint
// so a change from a restricted process boundary to an unrestricted one cannot
// reuse an earlier authorization.
func SandboxNetworkIntent(mode SandboxMode) string {
	return sandboxNetworkIntent(mode)
}

// SandboxPolicyNetworkIntent returns the identity of a per-process network
// capability. It is distinct from SandboxNetworkIntent, which describes the
// host-wide technical mode used by native shell tools.
func SandboxPolicyNetworkIntent(allow bool) string {
	if allow {
		return "allowed-by-server-policy"
	}
	return "denied-by-server-policy"
}

func sandboxSummary(mode SandboxMode) string {
	switch mode {
	case SandboxOff:
		return string(SandboxOff)
	case SandboxSeatbelt:
		if sandboxGOOS != "darwin" {
			return "seatbelt unavailable on " + sandboxGOOS
		}
		if _, err := sandboxLookPath("sandbox-exec"); err != nil {
			return "seatbelt unavailable"
		}
		return string(SandboxSeatbelt)
	case SandboxBubblewrap:
		if sandboxGOOS != "linux" {
			return "bubblewrap unavailable on " + sandboxGOOS
		}
		if _, err := sandboxLookPath("bwrap"); err != nil {
			return "bubblewrap unavailable"
		}
		return string(SandboxBubblewrap)
	case SandboxAuto:
		if sandboxGOOS == "darwin" {
			if _, err := sandboxLookPath("sandbox-exec"); err == nil {
				return "auto: seatbelt"
			}
		}
		if sandboxGOOS == "linux" {
			if _, err := sandboxLookPath("bwrap"); err == nil {
				return "auto: bubblewrap"
			}
		}
		return "auto: unavailable"
	default:
		return "unsupported: " + string(mode)
	}
}

func planSandboxedBash(cwd, command string, mode SandboxMode) (sandboxPlan, error) {
	name := "/bin/bash"
	if mode == SandboxOff {
		name = "bash"
	}
	return planSandboxedCommand(cwd, name, []string{"-c", command}, mode)
}

func planSandboxedCommand(cwd, name string, args []string, mode SandboxMode) (sandboxPlan, error) {
	switch mode {
	case SandboxOff:
		return sandboxPlan{
			name: name,
			args: append([]string(nil), args...),
			dir:  cwd,
		}, nil
	case SandboxSeatbelt:
		return planSeatbeltCommand(cwd, name, args)
	case SandboxBubblewrap:
		return planBubblewrapCommand(cwd, name, args)
	case SandboxAuto:
		if sandboxGOOS == "darwin" {
			if _, err := sandboxLookPath("sandbox-exec"); err == nil {
				return planSeatbeltCommand(cwd, name, args)
			}
		}
		if sandboxGOOS == "linux" {
			if _, err := sandboxLookPath("bwrap"); err == nil {
				return planBubblewrapCommand(cwd, name, args)
			}
		}
		return sandboxPlan{}, fmt.Errorf("automatic sandbox backend unavailable on %s", sandboxGOOS)
	default:
		return sandboxPlan{}, fmt.Errorf("unsupported sandbox mode %q", mode)
	}
}

func planSandboxedCommandWithPolicy(
	cwd, name string,
	args []string,
	mode SandboxMode,
	policy SandboxPolicy,
) (sandboxPlan, error) {
	policy, err := normalizeSandboxPolicy(cwd, policy)
	if err != nil {
		return sandboxPlan{}, err
	}
	switch mode {
	case SandboxSeatbelt:
		return planSeatbeltCommandWithPolicy(cwd, name, args, policy)
	case SandboxBubblewrap:
		return planBubblewrapCommandWithPolicy(cwd, name, args, policy)
	case SandboxAuto:
		if sandboxGOOS == "darwin" {
			if _, err := sandboxLookPath("sandbox-exec"); err == nil {
				return planSeatbeltCommandWithPolicy(cwd, name, args, policy)
			}
		}
		if sandboxGOOS == "linux" {
			if _, err := sandboxLookPath("bwrap"); err == nil {
				return planBubblewrapCommandWithPolicy(cwd, name, args, policy)
			}
		}
		return sandboxPlan{}, fmt.Errorf("automatic sandbox backend unavailable on %s", sandboxGOOS)
	case SandboxOff:
		return sandboxPlan{}, fmt.Errorf("per-process sandbox policy cannot be enforced with sandbox mode %q", mode)
	default:
		return sandboxPlan{}, fmt.Errorf("unsupported sandbox mode %q", mode)
	}
}

func normalizeSandboxPolicy(cwd string, policy SandboxPolicy) (SandboxPolicy, error) {
	root, err := filepath.Abs(cwd)
	if err != nil {
		return SandboxPolicy{}, fmt.Errorf("resolve sandbox workspace: %w", err)
	}
	root = filepath.Clean(root)
	readPaths, err := normalizeSandboxPaths(root, policy.ReadPaths, "read")
	if err != nil {
		return SandboxPolicy{}, err
	}
	writePaths, err := normalizeSandboxPaths(root, policy.WritePaths, "write")
	if err != nil {
		return SandboxPolicy{}, err
	}
	protectedPaths, err := normalizeSandboxPaths(root, policy.ProtectedPaths, "protected")
	if err != nil {
		return SandboxPolicy{}, err
	}
	for _, protected := range protectedPaths {
		for _, writable := range writePaths {
			if writable == protected || sandboxPathWithin(writable, protected) {
				if !sandboxPathExists(protected) {
					return SandboxPolicy{}, fmt.Errorf(
						"protected sandbox path %q does not exist beneath writable path %q; refusing unenforceable policy",
						protected, writable,
					)
				}
			}
		}
	}
	return SandboxPolicy{
		ReadPaths:      readPaths,
		WritePaths:     writePaths,
		ProtectedPaths: protectedPaths,
		AllowNetwork:   policy.AllowNetwork,
	}, nil
}

func normalizeSandboxPaths(root string, paths []string, kind string) ([]string, error) {
	if len(paths) == 0 {
		return nil, nil
	}
	result := make([]string, 0, len(paths))
	for _, raw := range paths {
		raw = strings.TrimSpace(raw)
		if raw == "" {
			continue
		}
		path := raw
		if !filepath.IsAbs(path) {
			path = filepath.Join(root, path)
		}
		path, err := filepath.Abs(path)
		if err != nil {
			return nil, fmt.Errorf("resolve %s sandbox path %q: %w", kind, raw, err)
		}
		path = filepath.Clean(path)
		if !sandboxPathWithin(root, path) && path != root {
			return nil, fmt.Errorf("%s sandbox path %q escapes workspace %q", kind, raw, root)
		}
		if (kind == "read" || kind == "write") && !sandboxPathExists(path) {
			return nil, fmt.Errorf("%s sandbox path %q does not exist", kind, raw)
		}
		result = append(result, path)
	}
	slices.Sort(result)
	return slices.Compact(result), nil
}

func sandboxPathWithin(root, candidate string) bool {
	rel, err := filepath.Rel(root, candidate)
	if err != nil {
		return false
	}
	return rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator))
}

func planSeatbeltSandbox(cwd, command string) (sandboxPlan, error) {
	return planSeatbeltCommand(cwd, "/bin/bash", []string{"-c", command})
}

func planSeatbeltCommand(cwd, name string, args []string) (sandboxPlan, error) {
	if sandboxGOOS != "darwin" {
		return sandboxPlan{}, fmt.Errorf("seatbelt sandbox unsupported on %s", sandboxGOOS)
	}
	seatbelt, err := sandboxLookPath("sandbox-exec")
	if err != nil {
		return sandboxPlan{}, fmt.Errorf("seatbelt sandbox unavailable: %w", err)
	}
	profile, err := os.CreateTemp("", "ion-seatbelt-*.sb")
	if err != nil {
		return sandboxPlan{}, fmt.Errorf("create seatbelt profile: %w", err)
	}
	profileText := seatbeltProfile(cwd)
	if _, err := profile.WriteString(profileText); err != nil {
		_ = profile.Close()
		_ = os.Remove(profile.Name())
		return sandboxPlan{}, fmt.Errorf("write seatbelt profile: %w", err)
	}
	if err := profile.Close(); err != nil {
		_ = os.Remove(profile.Name())
		return sandboxPlan{}, fmt.Errorf("close seatbelt profile: %w", err)
	}
	return sandboxPlan{
		name: seatbelt,
		args: append([]string{"-f", profile.Name(), name}, args...),
		dir:  cwd,
		cleanup: func() error {
			return os.Remove(profile.Name())
		},
	}, nil
}

func planSeatbeltCommandWithPolicy(cwd, name string, args []string, policy SandboxPolicy) (sandboxPlan, error) {
	if sandboxGOOS != "darwin" {
		return sandboxPlan{}, fmt.Errorf("seatbelt sandbox unsupported on %s", sandboxGOOS)
	}
	seatbelt, err := sandboxLookPath("sandbox-exec")
	if err != nil {
		return sandboxPlan{}, fmt.Errorf("seatbelt sandbox unavailable: %w", err)
	}
	profile, err := os.CreateTemp("", "ion-seatbelt-*.sb")
	if err != nil {
		return sandboxPlan{}, fmt.Errorf("create seatbelt profile: %w", err)
	}
	if _, err := profile.WriteString(seatbeltProfileWithPolicy(cwd, name, policy)); err != nil {
		_ = profile.Close()
		_ = os.Remove(profile.Name())
		return sandboxPlan{}, fmt.Errorf("write seatbelt profile: %w", err)
	}
	if err := profile.Close(); err != nil {
		_ = os.Remove(profile.Name())
		return sandboxPlan{}, fmt.Errorf("close seatbelt profile: %w", err)
	}
	return sandboxPlan{
		name: seatbelt,
		args: append([]string{"-f", profile.Name(), name}, args...),
		dir:  cwd,
		cleanup: func() error {
			return os.Remove(profile.Name())
		},
	}, nil
}

func seatbeltProfile(cwd string) string {
	quoted := strconv.Quote(cwd)
	// macOS system processes may resolve dependencies outside a finite list of
	// stable paths (for example, through dyld caches and private framework paths).
	// A filtered read rule aborts sandbox-exec on those resolutions, so keep reads
	// unrestricted while retaining the write and network restrictions below.
	return fmt.Sprintf(`(version 1)
(deny default)
(allow process*)
(allow signal (target self))
(allow sysctl-read)
(allow file-read*)
(allow file-read* (subpath %s))
(allow file-write*
  (subpath "/tmp")
  (subpath "/private/tmp")
  (subpath %s))
`, quoted, quoted)
}

func seatbeltProfileWithPolicy(cwd, name string, policy SandboxPolicy) string {
	var builder strings.Builder
	fmt.Fprintf(&builder, `(version 1)
(deny default)
(allow process*)
(allow signal (target self))
(allow sysctl-read)
(allow file-read-data (literal "/"))
(allow file-read-metadata (subpath "/var"))
(allow file-write* (subpath "/tmp") (subpath "/private/tmp"))
`)
	for _, commandPath := range sandboxCommandPaths(name) {
		fmt.Fprintf(&builder, "(allow process-exec (literal %s))\n", strconv.Quote(commandPath))
		fmt.Fprintf(&builder, "(allow file-map-executable (subpath %s))\n", strconv.Quote(filepath.Dir(commandPath)))
	}
	for _, path := range sandboxReadPaths(cwd, name, policy.ReadPaths) {
		fmt.Fprintf(&builder, "(allow file-read* (subpath %s))\n", strconv.Quote(path))
	}
	if len(policy.WritePaths) > 0 {
		builder.WriteString("(allow file-write*\n")
		for _, path := range policy.WritePaths {
			fmt.Fprintf(&builder, "  (subpath %s)\n", strconv.Quote(path))
		}
		builder.WriteString(")\n")
	}
	if policy.AllowNetwork {
		builder.WriteString("(allow network*)\n")
	}
	for _, path := range policy.ProtectedPaths {
		fmt.Fprintf(&builder, "(deny file-write* (subpath %s))\n", strconv.Quote(path))
	}
	return builder.String()
}

func sandboxReadPaths(cwd, name string, configured []string) []string {
	paths := append([]string{cwd}, configured...)
	for _, path := range []string{"/bin", "/sbin", "/usr", "/System", "/Library", "/etc", "/private/etc", "/dev", "/private/tmp"} {
		if sandboxPathExists(path) {
			paths = append(paths, path)
		}
	}
	for _, commandPath := range sandboxCommandPaths(name) {
		if dir := filepath.Dir(commandPath); sandboxPathExists(dir) {
			paths = append(paths, dir)
		}
		for _, prefix := range []string{"/opt/homebrew", "/usr/local"} {
			if sandboxPathWithin(prefix, commandPath) && sandboxPathExists(prefix) {
				paths = append(paths, prefix)
			}
		}
	}
	slices.Sort(paths)
	return slices.Compact(paths)
}

func sandboxCommandPaths(name string) []string {
	paths := []string{name}
	if !filepath.IsAbs(name) {
		if resolved, err := exec.LookPath(name); err == nil {
			paths = append(paths, resolved)
		}
	}
	for _, path := range append([]string(nil), paths...) {
		if resolved, err := filepath.EvalSymlinks(path); err == nil {
			paths = append(paths, resolved)
		}
	}
	result := paths[:0]
	for _, path := range paths {
		if filepath.IsAbs(path) {
			result = append(result, filepath.Clean(path))
		}
	}
	slices.Sort(result)
	return slices.Compact(result)
}

func planBubblewrapSandbox(cwd, command string) (sandboxPlan, error) {
	return planBubblewrapCommand(cwd, "/bin/bash", []string{"-c", command})
}

func planBubblewrapCommand(cwd, name string, args []string) (sandboxPlan, error) {
	if sandboxGOOS != "linux" {
		return sandboxPlan{}, fmt.Errorf("bubblewrap sandbox unsupported on %s", sandboxGOOS)
	}
	bwrap, err := sandboxLookPath("bwrap")
	if err != nil {
		return sandboxPlan{}, fmt.Errorf("bubblewrap unavailable: %w", err)
	}
	bwrapArgs := []string{
		"--unshare-net",
		"--bind", cwd, cwd,
		"--chdir", cwd,
		"--ro-bind", "/bin", "/bin",
		"--ro-bind", "/usr", "/usr",
		"--ro-bind", "/etc", "/etc",
		"--bind", "/tmp", "/tmp",
		"--dev", "/dev",
		"--proc", "/proc",
	}
	if sandboxPathExists("/private/tmp") {
		bwrapArgs = append(bwrapArgs, "--bind", "/private/tmp", "/private/tmp")
	}
	bwrapArgs = append(bwrapArgs, name)
	bwrapArgs = append(bwrapArgs, args...)
	return sandboxPlan{
		name: bwrap,
		args: bwrapArgs,
		dir:  cwd,
	}, nil
}

func planBubblewrapCommandWithPolicy(cwd, name string, args []string, policy SandboxPolicy) (sandboxPlan, error) {
	if sandboxGOOS != "linux" {
		return sandboxPlan{}, fmt.Errorf("bubblewrap sandbox unsupported on %s", sandboxGOOS)
	}
	bwrap, err := sandboxLookPath("bwrap")
	if err != nil {
		return sandboxPlan{}, fmt.Errorf("bubblewrap unavailable: %w", err)
	}
	bwrapArgs := []string{}
	if !policy.AllowNetwork {
		bwrapArgs = append(bwrapArgs, "--unshare-net")
	}
	bwrapArgs = append(bwrapArgs,
		"--ro-bind", cwd, cwd,
		"--ro-bind", "/bin", "/bin",
		"--ro-bind", "/usr", "/usr",
		"--ro-bind", "/etc", "/etc",
		"--bind", "/tmp", "/tmp",
		"--dev", "/dev",
		"--proc", "/proc",
	)
	if sandboxPathExists("/private/tmp") {
		bwrapArgs = append(bwrapArgs, "--bind", "/private/tmp", "/private/tmp")
	}
	for _, path := range sandboxReadPaths(cwd, name, policy.ReadPaths) {
		if path == cwd || path == "/bin" || path == "/usr" || path == "/etc" || path == "/private/tmp" {
			continue
		}
		if sandboxPathExists(path) {
			bwrapArgs = append(bwrapArgs, "--ro-bind", path, path)
		}
	}
	for _, path := range policy.WritePaths {
		bwrapArgs = append(bwrapArgs, "--bind", path, path)
	}
	for _, path := range policy.ProtectedPaths {
		if sandboxPathExists(path) {
			bwrapArgs = append(bwrapArgs, "--ro-bind", path, path)
		}
	}
	bwrapArgs = append(bwrapArgs, name)
	bwrapArgs = append(bwrapArgs, args...)
	return sandboxPlan{name: bwrap, args: bwrapArgs, dir: cwd}, nil
}
