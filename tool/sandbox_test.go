package tool

import (
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"testing"
)

func TestPlanSandboxedBashOffUsesPlainBash(t *testing.T) {
	plan, err := planSandboxedBash("/tmp/workspace", "git status", SandboxOff)
	if err != nil {
		t.Fatalf("planSandboxedBash(off) error = %v", err)
	}
	if plan.name != "bash" {
		t.Fatalf("plan name = %q, want bash", plan.name)
	}
	if got := strings.Join(plan.args, " "); got != "-c git status" {
		t.Fatalf("plan args = %q, want plain shell invocation", got)
	}
	if plan.dir != "/tmp/workspace" {
		t.Fatalf("plan dir = %q, want /tmp/workspace", plan.dir)
	}
}

func TestResolveSandboxModeDefaultsToAuto(t *testing.T) {
	t.Setenv("ION_SANDBOX", "")
	if got := resolveSandboxMode(); got != SandboxAuto {
		t.Fatalf("default sandbox mode = %s, want %s", got, SandboxAuto)
	}
}

func TestResolveSandboxModeRejectsUnknownConfiguration(t *testing.T) {
	t.Setenv("ION_SANDBOX", "not-a-sandbox")
	got := resolveSandboxMode()
	if got == SandboxAuto || got == SandboxOff {
		t.Fatalf("unknown sandbox mode = %s, want fail-closed invalid mode", got)
	}
	if _, err := planSandboxedBash(t.TempDir(), "true", got); err == nil {
		t.Fatal("unknown sandbox mode unexpectedly produced an executable plan")
	}
}

func TestPlanSeatbeltSandboxBuildsProfile(t *testing.T) {
	prevGOOS := sandboxGOOS
	prevLookPath := sandboxLookPath
	sandboxGOOS = "darwin"
	sandboxLookPath = func(name string) (string, error) {
		if name != "sandbox-exec" {
			t.Fatalf("lookPath called with %q, want sandbox-exec", name)
		}
		return "/usr/bin/sandbox-exec", nil
	}
	defer func() {
		sandboxGOOS = prevGOOS
		sandboxLookPath = prevLookPath
	}()

	plan, err := planSeatbeltSandbox("/Users/nick/github/nijaru/ion", "go test ./...")
	if err != nil {
		t.Fatalf("planSeatbeltSandbox error = %v", err)
	}
	if plan.cleanup != nil {
		t.Cleanup(func() {
			_ = plan.cleanup()
		})
	}
	if plan.name != "/usr/bin/sandbox-exec" {
		t.Fatalf("plan name = %q, want /usr/bin/sandbox-exec", plan.name)
	}
	if len(plan.args) < 4 {
		t.Fatalf("plan args too short: %#v", plan.args)
	}
	profilePath := plan.args[1]
	if _, err := os.Stat(profilePath); err != nil {
		t.Fatalf("seatbelt profile missing: %v", err)
	}
	data, err := os.ReadFile(profilePath)
	if err != nil {
		t.Fatalf("read seatbelt profile: %v", err)
	}
	profile := string(data)
	for _, want := range []string{"(deny default)", "(allow process*)", "/Users/nick/github/nijaru/ion"} {
		if !strings.Contains(profile, want) {
			t.Fatalf("seatbelt profile missing %q: %s", want, profile)
		}
	}
	if strings.Contains(profile, "(allow file-read*)\n") {
		t.Fatalf("native shell profile grants unrestricted reads: %s", profile)
	}
}

func TestSeatbeltProfileQuotesWorkspacePath(t *testing.T) {
	cwd := "/tmp/work\"space\\with\nnewline"
	profile := seatbeltProfile(cwd)
	quoted := strconv.Quote(cwd)
	if !strings.Contains(profile, "(subpath "+quoted+")") {
		t.Fatalf("seatbelt profile missing quoted path %s: %s", quoted, profile)
	}
	if strings.Contains(profile, "(subpath \"/tmp/work\"space") {
		t.Fatalf("seatbelt profile contains unescaped workspace path: %s", profile)
	}
}

func TestPlanSandboxedBashAutoPrefersDarwinSeatbeltWhenAvailable(t *testing.T) {
	prevGOOS := sandboxGOOS
	prevLookPath := sandboxLookPath
	sandboxGOOS = "darwin"
	sandboxLookPath = func(name string) (string, error) {
		if name != "sandbox-exec" {
			t.Fatalf("lookPath called with %q, want sandbox-exec", name)
		}
		return "/usr/bin/sandbox-exec", nil
	}
	defer func() {
		sandboxGOOS = prevGOOS
		sandboxLookPath = prevLookPath
	}()

	plan, err := planSandboxedBash(t.TempDir(), "pwd", SandboxAuto)
	if err != nil {
		t.Fatalf("planSandboxedBash(auto darwin) error = %v", err)
	}
	if plan.name != "/usr/bin/sandbox-exec" {
		t.Fatalf("plan name = %q, want /usr/bin/sandbox-exec", plan.name)
	}
}

func TestPlanSandboxedBashAutoPrefersLinuxBubblewrapWhenAvailable(t *testing.T) {
	prevGOOS := sandboxGOOS
	prevLookPath := sandboxLookPath
	prevPathExists := sandboxPathExists
	sandboxGOOS = "linux"
	sandboxLookPath = func(name string) (string, error) {
		if name != "bwrap" {
			t.Fatalf("lookPath called with %q, want bwrap", name)
		}
		return "/usr/bin/bwrap", nil
	}
	sandboxPathExists = func(path string) bool {
		return path != "/private/tmp"
	}
	defer func() {
		sandboxGOOS = prevGOOS
		sandboxLookPath = prevLookPath
		sandboxPathExists = prevPathExists
	}()

	plan, err := planSandboxedBash("/tmp/workspace", "pwd", SandboxAuto)
	if err != nil {
		t.Fatalf("planSandboxedBash(auto linux) error = %v", err)
	}
	if plan.name != "/usr/bin/bwrap" {
		t.Fatalf("plan name = %q, want /usr/bin/bwrap", plan.name)
	}
	if !strings.Contains(strings.Join(plan.args, " "), "--unshare-net") {
		t.Fatalf("expected bubblewrap args, got %#v", plan.args)
	}
	if strings.Contains(strings.Join(plan.args, " "), "/private/tmp") {
		t.Fatalf("did not expect missing /private/tmp bind, got %#v", plan.args)
	}
}

func TestPlanSandboxedCommandPreservesArbitraryCommandArguments(t *testing.T) {
	prevGOOS := sandboxGOOS
	prevLookPath := sandboxLookPath
	prevPathExists := sandboxPathExists
	sandboxGOOS = "linux"
	sandboxLookPath = func(name string) (string, error) {
		if name != "bwrap" {
			t.Fatalf("lookPath called with %q, want bwrap", name)
		}
		return "/usr/bin/bwrap", nil
	}
	root := t.TempDir()
	resolvedRoot, err := filepath.EvalSymlinks(root)
	if err != nil {
		t.Fatal(err)
	}
	sandboxPathExists = func(path string) bool {
		return path == root || path == resolvedRoot
	}
	defer func() {
		sandboxGOOS = prevGOOS
		sandboxLookPath = prevLookPath
		sandboxPathExists = prevPathExists
	}()

	plan, err := PlanSandboxedCommand(root, "/usr/local/bin/mcp-server", []string{"--config", "file with spaces.json"}, SandboxBubblewrap)
	if err != nil {
		t.Fatalf("PlanSandboxedCommand error = %v", err)
	}
	if plan.Name != "/usr/bin/bwrap" || plan.Dir != resolvedRoot {
		t.Fatalf("plan = %#v, want bubblewrap in workspace", plan)
	}
	if len(plan.Args) < 3 || plan.Args[len(plan.Args)-3] != "/usr/local/bin/mcp-server" || plan.Args[len(plan.Args)-2] != "--config" || plan.Args[len(plan.Args)-1] != "file with spaces.json" {
		t.Fatalf("plan args = %#v, want command and arguments preserved", plan.Args)
	}
}

func TestPlanSandboxedCommandWithPolicyIsReadOnlyAndNetworkDenied(t *testing.T) {
	prevGOOS := sandboxGOOS
	prevLookPath := sandboxLookPath
	prevPathExists := sandboxPathExists
	sandboxGOOS = "linux"
	sandboxLookPath = func(name string) (string, error) {
		if name != "bwrap" {
			t.Fatalf("lookPath called with %q, want bwrap", name)
		}
		return "/usr/bin/bwrap", nil
	}
	sandboxPathExists = func(path string) bool { return path == "/private/tmp" }
	defer func() {
		sandboxGOOS = prevGOOS
		sandboxLookPath = prevLookPath
		sandboxPathExists = prevPathExists
	}()

	plan, err := PlanSandboxedCommandWithPolicy(
		"/tmp/workspace", "/usr/local/bin/mcp-server", nil, SandboxBubblewrap,
		SandboxPolicy{},
	)
	if err != nil {
		t.Fatalf("PlanSandboxedCommandWithPolicy: %v", err)
	}
	joined := strings.Join(plan.Args, " ")
	if !strings.Contains(joined, "--unshare-net") || !strings.Contains(joined, "--ro-bind /tmp/workspace /tmp/workspace") {
		t.Fatalf("read-only policy args = %#v, want network isolation and read-only workspace", plan.Args)
	}
	if strings.Contains(joined, "--bind /tmp/workspace /tmp/workspace") {
		t.Fatalf("read-only policy unexpectedly grants workspace writes: %#v", plan.Args)
	}
}

func TestSeatbeltPolicyDoesNotGrantUnrestrictedReads(t *testing.T) {
	prevGOOS := sandboxGOOS
	prevLookPath := sandboxLookPath
	prevPathExists := sandboxPathExists
	sandboxGOOS = "darwin"
	sandboxLookPath = func(name string) (string, error) {
		if name != "sandbox-exec" {
			t.Fatalf("lookPath called with %q, want sandbox-exec", name)
		}
		return "/usr/bin/sandbox-exec", nil
	}
	sandboxPathExists = func(path string) bool {
		return path == "/tmp/workspace" || path == "/bin" || path == "/usr" || path == "/etc"
	}
	defer func() {
		sandboxGOOS = prevGOOS
		sandboxLookPath = prevLookPath
		sandboxPathExists = prevPathExists
	}()

	plan, err := PlanSandboxedCommandWithPolicy("/tmp/workspace", "/bin/server", nil, SandboxSeatbelt, SandboxPolicy{})
	if err != nil {
		t.Fatalf("PlanSandboxedCommandWithPolicy: %v", err)
	}
	t.Cleanup(func() { _ = plan.Cleanup() })
	data, err := os.ReadFile(plan.Args[1])
	if err != nil {
		t.Fatalf("read profile: %v", err)
	}
	profile := string(data)
	if strings.Contains(profile, "(allow file-read*)\n") {
		t.Fatalf("policy profile grants unrestricted reads: %s", profile)
	}
	if !strings.Contains(profile, `(allow file-read* (subpath "/tmp/workspace"))`) {
		t.Fatalf("policy profile omits workspace read rule: %s", profile)
	}
}

func TestPlanSandboxedCommandWithPolicyProtectsWritablePath(t *testing.T) {
	prevGOOS := sandboxGOOS
	prevLookPath := sandboxLookPath
	sandboxGOOS = "linux"
	sandboxLookPath = func(name string) (string, error) {
		if name != "bwrap" {
			t.Fatalf("lookPath called with %q, want bwrap", name)
		}
		return "/usr/bin/bwrap", nil
	}
	defer func() {
		sandboxGOOS = prevGOOS
		sandboxLookPath = prevLookPath
	}()

	root := t.TempDir()
	protected := filepath.Join(root, ".env")
	resolvedRoot, err := filepath.EvalSymlinks(root)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(protected, []byte("secret"), 0o600); err != nil {
		t.Fatal(err)
	}
	plan, err := PlanSandboxedCommandWithPolicy(root, "server", nil, SandboxBubblewrap, SandboxPolicy{
		WritePaths:     []string{"."},
		ProtectedPaths: []string{".env"},
	})
	if err != nil {
		t.Fatalf("PlanSandboxedCommandWithPolicy: %v", err)
	}
	joined := strings.Join(plan.Args, " ")
	if !strings.Contains(joined, "--bind "+resolvedRoot+" "+resolvedRoot) {
		t.Fatalf("writable policy missing workspace bind: %#v", plan.Args)
	}
	resolvedProtected := filepath.Join(resolvedRoot, ".env")
	if !strings.Contains(joined, "--ro-bind "+resolvedProtected+" "+resolvedProtected) {
		t.Fatalf("protected path missing read-only overlay: %#v", plan.Args)
	}
}

func TestPlanSandboxedCommandWithPolicyRejectsUnsandboxedMode(t *testing.T) {
	if _, err := PlanSandboxedCommandWithPolicy(t.TempDir(), "server", nil, SandboxOff, SandboxPolicy{}); err == nil || !strings.Contains(err.Error(), "cannot be enforced") {
		t.Fatalf("unsandboxed policy error = %v, want fail-closed enforcement error", err)
	}
}

func TestSeatbeltPolicyBlocksProtectedWrite(t *testing.T) {
	if runtime.GOOS != "darwin" {
		t.Skip("seatbelt is only available on macOS")
	}
	if _, err := exec.LookPath("sandbox-exec"); err != nil {
		t.Skipf("sandbox-exec unavailable: %v", err)
	}
	root := t.TempDir()
	protected := filepath.Join(root, ".env")
	if err := os.WriteFile(protected, []byte("original"), 0o600); err != nil {
		t.Fatal(err)
	}
	plan, err := PlanSandboxedCommandWithPolicy(root, "/bin/sh", []string{"-c", "printf changed > .env"}, SandboxSeatbelt, SandboxPolicy{
		WritePaths:     []string{"."},
		ProtectedPaths: []string{".env"},
	})
	if err != nil {
		t.Fatalf("PlanSandboxedCommandWithPolicy: %v", err)
	}
	t.Cleanup(func() { _ = plan.Cleanup() })
	if err := exec.Command(plan.Name, plan.Args...).Run(); err == nil {
		t.Fatal("sandboxed protected write unexpectedly succeeded")
	}
	data, err := os.ReadFile(protected)
	if err != nil {
		t.Fatal(err)
	}
	if string(data) != "original" {
		t.Fatalf("protected file = %q, want original", data)
	}
}

func TestExplicitSeatbeltFailsClosedWhenUnavailable(t *testing.T) {
	prevGOOS := sandboxGOOS
	prevLookPath := sandboxLookPath
	sandboxGOOS = "darwin"
	sandboxLookPath = func(name string) (string, error) {
		return "", errors.New("missing")
	}
	defer func() {
		sandboxGOOS = prevGOOS
		sandboxLookPath = prevLookPath
	}()

	if _, err := planSandboxedBash("/tmp/workspace", "pwd", SandboxSeatbelt); err == nil {
		t.Fatal("explicit seatbelt mode fell back instead of failing")
	}
}

func TestExplicitBubblewrapFailsClosedOnUnsupportedPlatform(t *testing.T) {
	prevGOOS := sandboxGOOS
	sandboxGOOS = "darwin"
	defer func() {
		sandboxGOOS = prevGOOS
	}()

	if _, err := planSandboxedBash("/tmp/workspace", "pwd", SandboxBubblewrap); err == nil {
		t.Fatal("explicit bubblewrap mode ran on unsupported platform")
	}
}

func TestSandboxSummaryReportsAutoUnavailable(t *testing.T) {
	prevGOOS := sandboxGOOS
	prevLookPath := sandboxLookPath
	sandboxGOOS = "linux"
	sandboxLookPath = func(name string) (string, error) {
		return "", errors.New("missing")
	}
	defer func() {
		sandboxGOOS = prevGOOS
		sandboxLookPath = prevLookPath
	}()

	if got := sandboxSummary(SandboxAuto); got != "auto: unavailable" {
		t.Fatalf("summary = %q, want unavailable backend", got)
	}
}

func TestPlanSandboxedBashAutoFailsClosedWithoutBackend(t *testing.T) {
	prevGOOS := sandboxGOOS
	prevLookPath := sandboxLookPath
	sandboxGOOS = "linux"
	sandboxLookPath = func(string) (string, error) {
		return "", errors.New("missing")
	}
	defer func() {
		sandboxGOOS = prevGOOS
		sandboxLookPath = prevLookPath
	}()

	if _, err := planSandboxedBash("/tmp/workspace", "pwd", SandboxAuto); err == nil {
		t.Fatal("automatic sandbox unexpectedly fell back to unsandboxed bash")
	}
}
