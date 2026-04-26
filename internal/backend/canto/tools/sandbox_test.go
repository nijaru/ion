package tools

import (
	"errors"
	"os"
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

	plan, err := planSandboxedBash("/tmp/workspace", "pwd", SandboxAuto)
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

func TestSandboxSummaryReportsAutoFallback(t *testing.T) {
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

	if got := sandboxSummary(SandboxAuto); got != "auto: off (no backend)" {
		t.Fatalf("summary = %q, want auto fallback", got)
	}
}
