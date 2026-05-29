package workspace

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestLoadEscalate(t *testing.T) {
	dir := t.TempDir()
	content := `# ESCALATE
> Human approval protocol.
---
## TRIGGERS
always_escalate:
- deploy_to_production
- cost_exceeds_usd: 100.00
## CHANNELS
channels:
- type: email
  address: [email protected]
  timeout_minutes: 15
- type: slack
  channel: "#ai-alerts"
  timeout_minutes: 10
## APPROVAL
approval_timeout_minutes: 30
on_timeout: escalate_to_killswitch
on_denial: halt_and_log
on_approval: proceed_and_log
`
	if err := os.WriteFile(filepath.Join(dir, "ESCALATE.md"), []byte(content), 0o600); err != nil {
		t.Fatalf("WriteFile: %v", err)
	}

	root, err := Open(dir)
	if err != nil {
		t.Fatalf("Open: %v", err)
	}
	t.Cleanup(func() {
		_ = root.Close()
	})

	cfg, err := LoadEscalate(root)
	if err != nil {
		t.Fatalf("LoadEscalate: %v", err)
	}
	if len(cfg.Triggers) != 2 {
		t.Fatalf("Triggers len = %d, want 2", len(cfg.Triggers))
	}
	if cfg.Triggers[0].Name != "deploy_to_production" {
		t.Fatalf("first trigger = %+v", cfg.Triggers[0])
	}
	if cfg.Triggers[1].Name != "cost_exceeds_usd" || cfg.Triggers[1].Value != "100.00" {
		t.Fatalf("second trigger = %+v", cfg.Triggers[1])
	}
	if len(cfg.Channels) != 2 {
		t.Fatalf("Channels len = %d, want 2", len(cfg.Channels))
	}
	if got := cfg.Channels[0]; got.Type != "email" || got.Address != "[email protected]" ||
		got.Timeout != 15*time.Minute {
		t.Fatalf("first channel = %+v", got)
	}
	if got := cfg.Channels[1]; got.Type != "slack" || got.Channel != "#ai-alerts" ||
		got.Timeout != 10*time.Minute {
		t.Fatalf("second channel = %+v", got)
	}
	if cfg.Approval.Timeout != 30*time.Minute {
		t.Fatalf("Approval.Timeout = %v, want %v", cfg.Approval.Timeout, 30*time.Minute)
	}
	if cfg.Approval.OnTimeout != "escalate_to_killswitch" ||
		cfg.Approval.OnDenial != "halt_and_log" ||
		cfg.Approval.OnApproval != "proceed_and_log" {
		t.Fatalf("Approval = %+v", cfg.Approval)
	}
}

func TestParseEscalateRejectsInvalidTimeout(t *testing.T) {
	_, err := ParseEscalate([]byte(`## CHANNELS
channels:
- type: slack
  timeout_minutes: nope
`))
	if err == nil {
		t.Fatal("expected parse error")
	}
}
