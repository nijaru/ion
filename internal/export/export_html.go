package export

import (
	"context"
	"fmt"
	"html"
	"strings"
	"time"

	"github.com/nijaru/ion/session"
)

// SessionBundle is the transport format for explicit session export/import.
// It is owned by the export boundary rather than the TUI runtime.
type SessionBundle struct {
	RootSessionID string
	Sessions      []SessionBundleRecord
	ExportedAt    time.Time
}

type SessionBundleRecord struct {
	Info   session.Session
	Events []session.Entry
}

type SessionBundleExporter interface {
	ExportSessionBundle(ctx context.Context, leafID string) (SessionBundle, error)
}

type SessionBundleImporter interface {
	ImportSessionBundle(ctx context.Context, bundle SessionBundle) (string, error)
}

// SessionData holds the data needed to export a session to HTML.
type SessionData struct {
	SessionID string
	Entries   []session.Entry
	Exported  time.Time
}

// EntryToHTML converts a single session entry to HTML.
func EntryToHTML(entry session.Entry) string {
	me, ok := entry.(*session.MessageEntry)
	if !ok {
		return "" // skip non-message entries
	}

	msg := me.Message
	switch m := msg.(type) {
	case *session.UserMessage:
		content := extractText(m.Content)
		return fmt.Sprintf(`<div class="entry user">
  <div class="role">You</div>
  <div class="content">%s</div>
</div>`, html.EscapeString(content))

	case *session.AssistantMessage:
		var b strings.Builder
		b.WriteString(`<div class="entry assistant">`)
		b.WriteString("\n  <div class=\"role\">Assistant</div>")
		reasoning := extractThinking(m.Content)
		if reasoning != "" {
			b.WriteString("\n  <details class=\"thinking\"><summary>Thinking...</summary>")
			b.WriteString("\n  <pre>")
			b.WriteString(html.EscapeString(reasoning))
			b.WriteString("</pre>\n  </details>")
		}
		content := extractText(m.Content)
		b.WriteString("\n  <div class=\"content\">")
		b.WriteString(html.EscapeString(content))
		b.WriteString("</div>\n</div>")
		return b.String()

	case *session.ToolResultMessage:
		content := extractText(m.Content)
		label := m.ToolName
		if label == "" {
			label = "tool"
		}
		return fmt.Sprintf(`<div class="entry tool">
  <div class="role">%s</div>
  <div class="content"><pre>%s</pre></div>
</div>`, html.EscapeString(label), html.EscapeString(content))

	default:
		return ""
	}
}

// GenerateHTML produces a complete self-contained HTML document from session data.
func GenerateHTML(data SessionData) string {
	var b strings.Builder
	b.WriteString(`<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Ion Session `)
	b.WriteString(html.EscapeString(data.SessionID[:min(8, len(data.SessionID))]))
	b.WriteString(`</title>
<style>
  :root {
    --bg: #1a1a2e;
    --surface: #16213e;
    --text: #e0e0e0;
    --muted: #888;
    --accent: #0f3460;
    --user-bg: #1e3a5f;
    --assistant-bg: #1a1a2e;
    --tool-bg: #2a2a3e;
    --border: #333;
    --code-bg: #0d1117;
  }
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body {
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
    background: var(--bg);
    color: var(--text);
    max-width: 900px;
    margin: 0 auto;
    padding: 2rem 1rem;
    line-height: 1.6;
  }
  .header {
    text-align: center;
    padding-bottom: 2rem;
    border-bottom: 1px solid var(--border);
    margin-bottom: 2rem;
  }
  .header h1 { font-size: 1.5rem; font-weight: 600; }
  .header .meta { color: var(--muted); font-size: 0.875rem; margin-top: 0.5rem; }
  .entry {
    padding: 1rem 1.25rem;
    margin-bottom: 1rem;
    border-radius: 8px;
    border: 1px solid var(--border);
  }
  .user { background: var(--user-bg); }
  .assistant { background: var(--assistant-bg); }
  .tool { background: var(--tool-bg); }
  .system { background: var(--surface); opacity: 0.7; }
  .role {
    font-size: 0.75rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--muted);
    margin-bottom: 0.5rem;
  }
  .content {
    white-space: pre-wrap;
    word-wrap: break-word;
    font-size: 0.9375rem;
  }
  .content pre {
    background: var(--code-bg);
    padding: 0.75rem;
    border-radius: 4px;
    overflow-x: auto;
    font-size: 0.8125rem;
    font-family: "SF Mono", "Fira Code", monospace;
  }
  .thinking {
    margin-bottom: 0.75rem;
    font-size: 0.8125rem;
    color: var(--muted);
  }
  .thinking summary {
    cursor: pointer;
    user-select: none;
  }
  .thinking pre {
    margin-top: 0.5rem;
    background: var(--code-bg);
    padding: 0.75rem;
    border-radius: 4px;
    color: var(--muted);
  }
  code {
    background: var(--code-bg);
    padding: 0.15em 0.4em;
    border-radius: 3px;
    font-size: 0.875em;
    font-family: "SF Mono", "Fira Code", monospace;
  }
  pre code { background: none; padding: 0; }
  .footer {
    text-align: center;
    padding-top: 2rem;
    border-top: 1px solid var(--border);
    margin-top: 2rem;
    color: var(--muted);
    font-size: 0.75rem;
  }
</style>
</head>
<body>
<div class="header">
  <h1>Ion Session</h1>
  <div class="meta">`)
	b.WriteString(html.EscapeString(data.SessionID[:min(8, len(data.SessionID))]))
	b.WriteString(" &middot; ")
	b.WriteString(fmt.Sprintf("%d", len(data.Entries)))
	b.WriteString(" entries &middot; exported ")
	b.WriteString(data.Exported.Format("2006-01-02 15:04"))
	b.WriteString(`</div>
</div>
`)
	for _, entry := range data.Entries {
		s := EntryToHTML(entry)
		if s != "" {
			b.WriteString(s)
			b.WriteString("\n")
		}
	}

	b.WriteString(`
<div class="footer">
  Generated by Ion &middot; <a href="https://github.com/nijaru/ion" style="color: var(--muted)">github.com/nijaru/ion</a>
</div>
</body>
</html>`)

	return b.String()
}

// --- helpers ---

func extractText(content []session.Content) string {
	var sb strings.Builder
	for _, c := range content {
		switch c := c.(type) {
		case session.TextContent:
			sb.WriteString(c.Text)
		}
	}
	return sb.String()
}

func extractThinking(content []session.Content) string {
	for _, c := range content {
		if tc, ok := c.(session.ThinkingContent); ok {
			return tc.Text
		}
	}
	return ""
}

// BundleToHTML converts a SessionBundle to HTML string.
func BundleToHTML(bundle SessionBundle) (string, error) {
	data := SessionData{
		SessionID: bundle.RootSessionID,
		Exported:  bundle.ExportedAt,
	}
	for _, rec := range bundle.Sessions {
		data.Entries = append(data.Entries, rec.Events...)
	}
	return GenerateHTML(data), nil
}
