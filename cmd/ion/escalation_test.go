package main

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestLoadEscalationConfig(t *testing.T) {
	dir := t.TempDir()
	content := `# ESCALATE
## TRIGGERS
- deploy_to_production
## CHANNELS
- type: email
  address: ops@example.com
  timeout_minutes: 15
- type: slack
  channel: "#ai-alerts"
  timeout_minutes: 10
## APPROVAL
approval_timeout_minutes: 30
on_timeout: halt_and_log
`
	if err := os.WriteFile(filepath.Join(dir, "ESCALATE.md"), []byte(content), 0o600); err != nil {
		t.Fatalf("write ESCALATE.md: %v", err)
	}

	cfg, err := loadEscalationConfig(dir)
	if err != nil {
		t.Fatalf("loadEscalationConfig returned error: %v", err)
	}
	if cfg == nil {
		t.Fatal("loadEscalationConfig returned nil config")
	}
	if len(cfg.Triggers) != 1 {
		t.Fatalf("triggers len = %d, want 1", len(cfg.Triggers))
	}
	if len(cfg.Channels) != 2 {
		t.Fatalf("channels len = %d, want 2", len(cfg.Channels))
	}
	if cfg.Channels[0].Address != "ops@example.com" {
		t.Fatalf("email address = %q", cfg.Channels[0].Address)
	}
	if cfg.Channels[1].Channel != "#ai-alerts" {
		t.Fatalf("slack channel = %q", cfg.Channels[1].Channel)
	}
	if cfg.Approval.Timeout != 30*time.Minute {
		t.Fatalf("approval timeout = %v, want 30m", cfg.Approval.Timeout)
	}
}

func TestLoadEscalationConfigMissingFile(t *testing.T) {
	cfg, err := loadEscalationConfig(t.TempDir())
	if err != nil {
		t.Fatalf("loadEscalationConfig returned error: %v", err)
	}
	if cfg != nil {
		t.Fatalf("loadEscalationConfig = %#v, want nil", cfg)
	}
}
