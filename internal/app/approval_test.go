package app

import (
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
	"github.com/nijaru/canto/workspace"
	"github.com/nijaru/ion/internal/session"
)

func TestApprovalFailureSurfacesLocalError(t *testing.T) {
	sess := &stubSession{
		events:     make(chan session.Event),
		approveErr: errors.New("approval bridge failed"),
	}
	model := readyModel(t)
	model.Model.Session = sess
	model.Approval.Pending = &session.ApprovalRequest{
		RequestID:   "req-1",
		Description: "run tool",
		ToolName:    "bash",
	}
	model.Progress.Mode = stateApproval

	updated, cmd := model.Update(tea.KeyPressMsg{Code: 'y', Text: "y"})
	model = updated.(Model)

	if model.Approval.Pending != nil {
		t.Fatal("approval pending should be cleared after approval attempt")
	}
	if cmd == nil {
		t.Fatal("expected error command for failed approval")
	}
	err := localErrorFromMsg(t, cmd())
	if !strings.Contains(err.Error(), "send approval") {
		t.Fatalf("approval error = %v, want send approval context", err)
	}
}

func TestApprovalPromptRendersEscalationChannels(t *testing.T) {
	model := readyModel(t).WithEscalation(&workspace.EscalationConfig{
		Channels: []workspace.EscalationChannel{
			{Type: "email", Address: "ops@example.com"},
			{Type: "slack", Channel: "#ai-alerts"},
		},
		Approval: workspace.EscalationApproval{Timeout: 30 * time.Minute},
	})
	model.Approval.Pending = &session.ApprovalRequest{
		RequestID:   "req-1",
		ToolName:    "bash",
		Args:        `{"command":"deploy"}`,
		Description: "Tool: bash",
	}

	planeB := ansi.Strip(model.renderPlaneB())
	for _, want := range []string{
		"Escalate: email ops@example.com",
		"slack #ai-alerts",
		"approval timeout 30m",
	} {
		if !strings.Contains(planeB, want) {
			t.Fatalf("renderPlaneB missing %q:\n%s", want, planeB)
		}
	}
}

func TestApprovalRequestRedactsSensitiveDisplayFields(t *testing.T) {
	model := readyModel(t)
	req := session.ApprovalRequest{
		RequestID:   "req-1",
		ToolName:    "bash",
		Args:        `{"command":"curl -H 'Authorization: Bearer abc.def-123' https://example.test"}`,
		Description: "Email jane.doe@example.com with api_key=sk-test1234567890",
	}

	updated, _ := model.Update(req)
	model = updated.(Model)

	if model.Approval.Pending == nil {
		t.Fatal("expected pending approval")
	}
	for _, leaked := range []string{"abc.def-123", "jane.doe@example.com", "sk-test1234567890"} {
		if strings.Contains(model.Approval.Pending.Description, leaked) ||
			strings.Contains(model.Approval.Pending.Args, leaked) {
			t.Fatalf("approval leaked %q: %#v", leaked, model.Approval.Pending)
		}
	}
	for _, want := range []string{"[redacted-secret]", "[redacted-email]"} {
		if !strings.Contains(model.Approval.Pending.Description+model.Approval.Pending.Args, want) {
			t.Fatalf("approval missing %q: %#v", want, model.Approval.Pending)
		}
	}
}

func TestApprovalPromptRendersExecutorEnvironment(t *testing.T) {
	model := readyModel(t)
	model.Approval.Pending = &session.ApprovalRequest{
		RequestID:   "req-1",
		Description: "run command",
		ToolName:    "bash",
		Args:        `{"command":"go test ./..."}`,
		Environment: "inherit",
	}
	model.Progress.Mode = stateApproval

	view := ansi.Strip(model.View().Content)
	if !strings.Contains(view, "Bash env: inherited") {
		t.Fatalf("view missing environment posture:\n%s", view)
	}
}

func TestApprovalNotificationSendsSlackWebhookAndAudits(t *testing.T) {
	var payload string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			t.Fatalf("method = %s, want POST", r.Method)
		}
		data, err := io.ReadAll(r.Body)
		if err != nil {
			t.Fatalf("read body: %v", err)
		}
		payload = string(data)
		w.WriteHeader(http.StatusOK)
	}))
	defer server.Close()

	t.Setenv("ION_TEST_SLACK_WEBHOOK", server.URL)
	req := session.ApprovalRequest{
		RequestID:   "req-1",
		ToolName:    "bash",
		Args:        `{"command":"deploy"}`,
		Description: "Tool: bash",
	}
	results := deliverApprovalNotifications(t.Context(), &workspace.EscalationConfig{
		Channels: []workspace.EscalationChannel{
			{
				Type:    "slack",
				Channel: "#ai-alerts",
				Metadata: map[string]string{
					"webhook_env": "ION_TEST_SLACK_WEBHOOK",
				},
			},
		},
	}, req, "/repo")

	if len(results) != 1 {
		t.Fatalf("results = %d, want 1", len(results))
	}
	result := results[0]
	if result.record.Status != "sent" || result.record.Channel != "slack" {
		t.Fatalf("record = %#v, want sent slack", result.record)
	}
	if !strings.Contains(result.notice, "Escalation notification sent: slack #ai-alerts") {
		t.Fatalf("notice = %q, want sent notice", result.notice)
	}
	for _, want := range []string{"Ion approval requested", "Workspace: /repo", "Tool: bash", `{\"command\":\"deploy\"}`} {
		if !strings.Contains(payload, want) {
			t.Fatalf("payload missing %q: %s", want, payload)
		}
	}
}

func TestApprovalNotificationIncludesEnvironment(t *testing.T) {
	req := session.ApprovalRequest{
		RequestID:   "req-1",
		ToolName:    "bash",
		Description: "Tool: bash",
		Environment: "inherit",
	}
	got := approvalNotificationText(req, "/repo", "slack #ai-alerts")
	if !strings.Contains(got, "Bash env: inherited") {
		t.Fatalf("notification missing environment posture:\n%s", got)
	}
}

func TestApprovalNotificationRedactsSensitiveContent(t *testing.T) {
	req := session.ApprovalRequest{
		RequestID:   "req-1",
		ToolName:    "bash",
		Args:        `{"command":"curl -H 'Authorization: Bearer abc.def-123' https://example.test"}`,
		Description: "Email jane.doe@example.com with token=sk-test1234567890",
	}

	got := approvalNotificationText(req, "/repo", "slack #ai-alerts")
	for _, leaked := range []string{"abc.def-123", "jane.doe@example.com", "sk-test1234567890"} {
		if strings.Contains(got, leaked) {
			t.Fatalf("notification leaked %q: %s", leaked, got)
		}
	}
	for _, want := range []string{"[redacted-secret]", "[redacted-email]"} {
		if !strings.Contains(got, want) {
			t.Fatalf("notification missing %q: %s", want, got)
		}
	}
}

func TestApprovalNotificationAuditsMissingCredentials(t *testing.T) {
	t.Setenv("ION_SLACK_WEBHOOK_URL", "")
	req := session.ApprovalRequest{
		RequestID:   "req-1",
		ToolName:    "bash",
		Description: "Tool: bash",
	}
	results := deliverApprovalNotifications(t.Context(), &workspace.EscalationConfig{
		Channels: []workspace.EscalationChannel{{Type: "slack", Channel: "#ai-alerts"}},
	}, req, "/repo")

	if len(results) != 1 {
		t.Fatalf("results = %d, want 1", len(results))
	}
	result := results[0]
	if result.record.Status != "skipped" {
		t.Fatalf("status = %q, want skipped", result.record.Status)
	}
	if !strings.Contains(result.record.Detail, "ION_SLACK_WEBHOOK_URL") {
		t.Fatalf("detail = %q, want missing env var", result.record.Detail)
	}
	if result.notice != "" {
		t.Fatalf("notice = %q, want quiet skipped notification", result.notice)
	}
}
