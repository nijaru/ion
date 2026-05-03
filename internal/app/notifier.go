package app

import (
	"bytes"
	"context"
	"encoding/json"
	"net"
	"net/http"
	"net/smtp"
	"os"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/nijaru/canto/workspace"
	"github.com/nijaru/ion/internal/privacy"
	"github.com/nijaru/ion/internal/session"
	"github.com/nijaru/ion/internal/storage"
)

const (
	defaultSlackWebhookEnv = "ION_SLACK_WEBHOOK_URL"
	defaultSMTPAddrEnv     = "ION_SMTP_ADDR"
	defaultSMTPFromEnv     = "ION_SMTP_FROM"
	defaultSMTPUserEnv     = "ION_SMTP_USERNAME"
	defaultSMTPPassEnv     = "ION_SMTP_PASSWORD"
)

type approvalNotificationMsg struct {
	entries []session.Entry
}

type approvalNotificationResult struct {
	record storage.EscalationNotification
	notice string
}

func (m Model) approvalNotificationCmd(req session.ApprovalRequest) tea.Cmd {
	if m.Model.Escalation == nil || len(m.Model.Escalation.Channels) == 0 {
		return nil
	}
	cfg := m.Model.Escalation
	store := m.Model.Storage
	workdir := m.App.Workdir
	return func() tea.Msg {
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()

		results := deliverApprovalNotifications(ctx, cfg, req, workdir)
		entries := make([]session.Entry, 0, len(results))
		for _, result := range results {
			if store != nil {
				_ = store.Append(context.Background(), result.record)
			}
			if strings.TrimSpace(result.notice) != "" {
				entries = append(entries, session.Entry{
					Role:    session.System,
					Content: result.notice,
					IsError: result.record.Status == "failed",
				})
			}
		}
		if len(entries) == 0 {
			return nil
		}
		return approvalNotificationMsg{entries: entries}
	}
}

func deliverApprovalNotifications(
	ctx context.Context,
	cfg *workspace.EscalationConfig,
	req session.ApprovalRequest,
	workdir string,
) []approvalNotificationResult {
	if cfg == nil {
		return nil
	}
	results := make([]approvalNotificationResult, 0, len(cfg.Channels))
	for _, channel := range cfg.Channels {
		switch strings.ToLower(strings.TrimSpace(channel.Type)) {
		case "slack":
			results = append(results, deliverSlackApprovalNotification(ctx, channel, req, workdir))
		case "email":
			results = append(results, deliverEmailApprovalNotification(channel, req, workdir))
		default:
			results = append(results, skippedNotification(req, channel, "unsupported channel type"))
		}
	}
	return results
}

func deliverSlackApprovalNotification(
	ctx context.Context,
	channel workspace.EscalationChannel,
	req session.ApprovalRequest,
	workdir string,
) approvalNotificationResult {
	target := escalationChannelLabel(channel)
	webhookEnv := channel.Metadata["webhook_env"]
	if strings.TrimSpace(webhookEnv) == "" {
		webhookEnv = defaultSlackWebhookEnv
	}
	webhookURL := strings.TrimSpace(os.Getenv(webhookEnv))
	if webhookURL == "" {
		return notificationResult(req, channel, "skipped", "missing "+webhookEnv, "")
	}

	payload := map[string]string{
		"text": approvalNotificationText(req, workdir, target),
	}
	body, err := json.Marshal(payload)
	if err != nil {
		return notificationResult(req, channel, "failed", err.Error(), "Escalation notification failed: "+target+": "+err.Error())
	}
	httpReq, err := http.NewRequestWithContext(ctx, http.MethodPost, webhookURL, bytes.NewReader(body))
	if err != nil {
		return notificationResult(req, channel, "failed", err.Error(), "Escalation notification failed: "+target+": "+err.Error())
	}
	httpReq.Header.Set("Content-Type", "application/json")
	resp, err := http.DefaultClient.Do(httpReq)
	if err != nil {
		return notificationResult(req, channel, "failed", err.Error(), "Escalation notification failed: "+target+": "+err.Error())
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		detail := "slack webhook returned " + resp.Status
		return notificationResult(req, channel, "failed", detail, "Escalation notification failed: "+target+": "+detail)
	}
	return notificationResult(req, channel, "sent", "", "Escalation notification sent: "+target)
}

func deliverEmailApprovalNotification(
	channel workspace.EscalationChannel,
	req session.ApprovalRequest,
	workdir string,
) approvalNotificationResult {
	target := escalationChannelLabel(channel)
	addrEnv := metadataDefault(channel.Metadata, "smtp_addr_env", defaultSMTPAddrEnv)
	fromEnv := metadataDefault(channel.Metadata, "from_env", defaultSMTPFromEnv)
	userEnv := metadataDefault(channel.Metadata, "smtp_user_env", defaultSMTPUserEnv)
	passEnv := metadataDefault(channel.Metadata, "smtp_pass_env", defaultSMTPPassEnv)
	addr := strings.TrimSpace(os.Getenv(addrEnv))
	from := strings.TrimSpace(os.Getenv(fromEnv))
	to := strings.TrimSpace(channel.Address)
	if addr == "" || from == "" || to == "" {
		return notificationResult(req, channel, "skipped", "missing SMTP configuration", "")
	}

	host, _, err := net.SplitHostPort(addr)
	if err != nil {
		return notificationResult(req, channel, "failed", err.Error(), "Escalation notification failed: "+target+": "+err.Error())
	}
	var auth smtp.Auth
	user := strings.TrimSpace(os.Getenv(userEnv))
	pass := strings.TrimSpace(os.Getenv(passEnv))
	if user != "" || pass != "" {
		auth = smtp.PlainAuth("", user, pass, host)
	}
	msg := "To: " + to + "\r\n" +
		"From: " + from + "\r\n" +
		"Subject: Ion approval requested\r\n" +
		"\r\n" +
		approvalNotificationText(req, workdir, target) + "\r\n"
	if err := sendSMTPMail(addr, auth, from, []string{to}, []byte(msg)); err != nil {
		return notificationResult(req, channel, "failed", err.Error(), "Escalation notification failed: "+target+": "+err.Error())
	}
	return notificationResult(req, channel, "sent", "", "Escalation notification sent: "+target)
}

func sendSMTPMail(addr string, auth smtp.Auth, from string, to []string, msg []byte) error {
	conn, err := net.DialTimeout("tcp", addr, 10*time.Second)
	if err != nil {
		return err
	}
	defer conn.Close()
	if err := conn.SetDeadline(time.Now().Add(10 * time.Second)); err != nil {
		return err
	}
	host, _, err := net.SplitHostPort(addr)
	if err != nil {
		return err
	}
	client, err := smtp.NewClient(conn, host)
	if err != nil {
		return err
	}
	defer client.Close()
	if auth != nil {
		if ok, _ := client.Extension("AUTH"); ok {
			if err := client.Auth(auth); err != nil {
				return err
			}
		}
	}
	if err := client.Mail(from); err != nil {
		return err
	}
	for _, addr := range to {
		if err := client.Rcpt(addr); err != nil {
			return err
		}
	}
	w, err := client.Data()
	if err != nil {
		return err
	}
	if _, err := w.Write(msg); err != nil {
		_ = w.Close()
		return err
	}
	if err := w.Close(); err != nil {
		return err
	}
	return client.Quit()
}

func approvalNotificationText(req session.ApprovalRequest, workdir, target string) string {
	parts := []string{
		"Ion approval requested",
		"Target: " + target,
		"Workspace: " + workdir,
		"Tool: " + req.ToolName,
		"Request: " + privacy.Redact(req.Description),
	}
	if strings.TrimSpace(req.Args) != "" {
		parts = append(parts, "Args: "+privacy.Redact(req.Args))
	}
	if environment := strings.TrimSpace(req.Environment); environment != "" {
		parts = append(parts, "Environment: "+environment)
	}
	return strings.Join(parts, "\n")
}

func metadataDefault(metadata map[string]string, key, fallback string) string {
	if metadata == nil {
		return fallback
	}
	if value := strings.TrimSpace(metadata[key]); value != "" {
		return value
	}
	return fallback
}

func skippedNotification(
	req session.ApprovalRequest,
	channel workspace.EscalationChannel,
	detail string,
) approvalNotificationResult {
	return notificationResult(req, channel, "skipped", detail, "")
}

func notificationResult(
	req session.ApprovalRequest,
	channel workspace.EscalationChannel,
	status string,
	detail string,
	notice string,
) approvalNotificationResult {
	return approvalNotificationResult{
		record: storage.EscalationNotification{
			Type:      "escalation_notification",
			RequestID: req.RequestID,
			Channel:   strings.ToLower(strings.TrimSpace(channel.Type)),
			Target:    escalationChannelLabel(channel),
			Status:    status,
			Detail:    detail,
			TS:        now(),
		},
		notice: notice,
	}
}

func (m Model) handleApprovalNotification(msg approvalNotificationMsg) (Model, tea.Cmd) {
	if len(msg.entries) == 0 {
		return m, nil
	}
	return m, m.printEntries(msg.entries...)
}
