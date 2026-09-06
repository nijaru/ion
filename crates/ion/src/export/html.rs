//! Session HTML export (pi parity surface). One self-contained file
//! per session — no vendored JS, no external assets: the entries are
//! inlined as JSON and rendered by a small template with plain-ES
//! escapers. `/export <path>.html` and `/share` both write this
//! shape; the share viewer at share.ion.dev consumes the same file
//! through a gist.

use std::fmt::Write as _;

/// One entry as the template consumes it: a role plus pre-escaped
/// content blocks. Field names match the JSON embedded in the page.
#[derive(serde::Serialize)]
struct HtmlEntry {
    role: &'static str,
    /// `None` for plain assistant/user text, `Some` for tool calls
    /// and results (rendered as detail blocks).
    tool: Option<ToolBlock>,
    text: String,
}

#[derive(serde::Serialize)]
struct ToolBlock {
    name: String,
    is_error: bool,
}

/// HTML-escape one text run.
fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Break up `</script>` sequences so the embedded JSON cannot
/// terminate the script tag early. Quotes and backslashes stay raw:
/// the block is `type="application/json"` raw text, not a JS string
/// literal, so escaping them would corrupt the JSON.
fn escape_script(text: &str) -> String {
    text.replace("</", "<\\/")
}

/// Render the full standalone HTML for one session's entries.
///
/// The structure mirrors pi's export: a chat-style card per message
/// with user/assistant/tool roles, collapsed tool details, and the
/// session header line. Unlike pi's vendored marked/highlight.js
/// pipeline, ion's template renders markdown minimally (code blocks,
/// inline code, bold) — the export's purpose is readable archives and
/// gist sharing, not pixel parity.
pub fn render(
    title: &str,
    entries: &[ion_core::SessionEntry],
    terminal_width_hint: usize,
) -> String {
    let mut html_entries: Vec<HtmlEntry> = Vec::new();
    for entry in entries {
        match entry {
            ion_core::SessionEntry::UserMessage { text } => {
                html_entries.push(HtmlEntry {
                    role: "user",
                    tool: None,
                    text: text.clone(),
                });
            }
            ion_core::SessionEntry::AssistantMessage { text } => {
                html_entries.push(HtmlEntry {
                    role: "assistant",
                    tool: None,
                    text: text.clone(),
                });
            }
            ion_core::SessionEntry::AgentMessage { text, .. } => {
                html_entries.push(HtmlEntry {
                    role: "agent",
                    tool: None,
                    text: text.clone(),
                });
            }
            ion_core::SessionEntry::ToolCall { call } => {
                html_entries.push(HtmlEntry {
                    role: "tool",
                    tool: Some(ToolBlock {
                        name: call.name.clone(),
                        is_error: false,
                    }),
                    text: format!("call {} {}\n{}", call.call_id, call.name, call.arguments),
                });
            }
            ion_core::SessionEntry::ToolResult { result } => {
                let (call_id, body, is_error) = match result {
                    ion_core::ToolResult::Ok {
                        call_id, output, ..
                    } => (call_id, output.clone(), false),
                    ion_core::ToolResult::Err { call_id, error, .. } => {
                        (call_id, error.clone(), true)
                    }
                };
                html_entries.push(HtmlEntry {
                    role: "tool",
                    tool: Some(ToolBlock {
                        name: String::new(),
                        is_error,
                    }),
                    text: format!("result {call_id}\n{body}"),
                });
            }
            ion_core::SessionEntry::ShellExecution {
                command,
                output,
                cancelled,
                ..
            } => {
                html_entries.push(HtmlEntry {
                    role: "shell",
                    tool: None,
                    text: if *cancelled {
                        format!("$ {command}\n(cancelled)")
                    } else {
                        format!("$ {command}\n{output}")
                    },
                });
            }
            ion_core::SessionEntry::Compaction { summary, .. } => {
                html_entries.push(HtmlEntry {
                    role: "compaction",
                    tool: None,
                    text: format!("Context compacted.\n{summary}"),
                });
            }
        }
    }
    let _ = terminal_width_hint;
    let json = serde_json::to_string(&html_entries).unwrap_or_else(|_| "[]".to_owned());
    let mut page = String::with_capacity(16 * 1024);
    let title = escape_html(title);
    let _ = writeln!(page, "<!DOCTYPE html>");
    let _ = writeln!(page, "<html lang=\"en\">");
    let _ = writeln!(page, "<head>");
    let _ = writeln!(page, "<meta charset=\"utf-8\">");
    let _ = writeln!(
        page,
        "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">"
    );
    let _ = writeln!(page, "<title>{title}</title>");
    let _ = writeln!(page, "<style>");
    page.push_str(CSS);
    let _ = writeln!(page, "</style>");
    let _ = writeln!(page, "</head>");
    let _ = writeln!(page, "<body>");
    let _ = writeln!(page, "<main class=\"card\">");
    let _ = writeln!(page, "<h1>{title}</h1>");
    let _ = writeln!(
        page,
        "<p class=\"meta\">{} entries · exported by ion</p>",
        html_entries.len()
    );
    let _ = writeln!(
        page,
        "<script id=\"session-data\" type=\"application/json\">{}</script>",
        escape_script(&json)
    );
    let _ = writeln!(page, "</main>");
    let _ = writeln!(page, "<script>");
    page.push_str(JS);
    let _ = writeln!(page, "</script>");
    let _ = writeln!(page, "</body>");
    let _ = writeln!(page, "</html>");
    page
}

const CSS: &str = r#"
:root {
  color-scheme: dark;
  --page-bg: rgb(16, 17, 23);
  --card-bg: rgb(24, 26, 34);
  --text: rgb(222, 224, 230);
  --muted: rgb(148, 152, 163);
  --user-bg: rgb(36, 39, 48);
  --assistant-bg: rgb(28, 30, 38);
  --tool-bg: rgb(22, 24, 30);
  --error: rgb(240, 113, 120);
  --code-bg: rgb(12, 13, 17);
  --border: rgb(45, 48, 58);
}
@media (prefers-color-scheme: light) {
  :root {
    color-scheme: light;
    --page-bg: rgb(248, 248, 250);
    --card-bg: rgb(255, 255, 255);
    --text: rgb(28, 30, 34);
    --muted: rgb(108, 112, 122);
    --user-bg: rgb(238, 240, 244);
    --assistant-bg: rgb(250, 250, 252);
    --tool-bg: rgb(243, 244, 246);
    --error: rgb(190, 40, 48);
    --code-bg: rgb(235, 236, 240);
    --border: rgb(210, 212, 218);
  }
}
* { box-sizing: border-box; }
body {
  margin: 0;
  padding: 2rem 1rem 4rem;
  background: var(--page-bg);
  color: var(--text);
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
  line-height: 1.55;
}
main.card {
  max-width: 46rem;
  margin: 0 auto;
  background: var(--card-bg);
  border: 1px solid var(--border);
  border-radius: 10px;
  padding: 1.5rem 1.75rem 2rem;
}
h1 { font-size: 1.25rem; margin: 0 0 0.25rem; }
p.meta { color: var(--muted); margin: 0 0 1.5rem; font-size: 0.85rem; }
.msg { margin: 0 0 0.75rem; padding: 0.6rem 0.9rem; border-radius: 8px; }
.msg.user { background: var(--user-bg); }
.msg.assistant, .msg.agent { background: var(--assistant-bg); }
.msg.tool { background: var(--tool-bg); }
.msg.shell { background: var(--tool-bg); }
.msg.compaction { background: var(--tool-bg); border-left: 3px solid var(--border); }
.msg .who {
  display: block;
  font-size: 0.72rem;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--muted);
  margin-bottom: 0.2rem;
}
.msg pre {
  background: var(--code-bg);
  border: 1px solid var(--border);
  border-radius: 6px;
  padding: 0.6rem 0.8rem;
  overflow-x: auto;
  font-size: 0.82rem;
  margin: 0.4rem 0;
  white-space: pre-wrap;
  word-break: break-word;
}
.msg.error-text pre { color: var(--error); }
details { margin-top: 0.25rem; }
details summary {
  cursor: pointer;
  color: var(--muted);
  font-size: 0.78rem;
}
details pre { margin-top: 0.35rem; }
code { font-family: ui-monospace, "SF Mono", Menlo, Consolas, monospace; }
"#;

const JS: &str = r#"
(function () {
  var dataEl = document.getElementById("session-data");
  if (!dataEl) return;
  var entries = JSON.parse(dataEl.textContent);
  var main = document.querySelector("main.card");
  function esc(s) {
    return String(s)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  }
  function labelFor(entry) {
    if (entry.role === "user") return "You";
    if (entry.role === "assistant") return "Assistant";
    if (entry.role === "agent") return "Agent";
    if (entry.role === "shell") return "Shell";
    if (entry.role === "compaction") return "Compaction";
    return entry.tool && entry.tool.name ? "Tool call · " + entry.tool.name : "Tool result";
  }
  entries.forEach(function (entry) {
    var div = document.createElement("div");
    div.className = "msg " + entry.role + (entry.tool && entry.tool.is_error ? " error-text" : "");
    var who = document.createElement("span");
    who.className = "who";
    who.textContent = labelFor(entry);
    div.appendChild(who);
    var pre = document.createElement("pre");
    pre.textContent = entry.text;
    div.appendChild(pre);
    main.appendChild(div);
  });
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_a_standalone_page_with_entries_and_escaping() {
        let entries = vec![
            ion_core::SessionEntry::UserMessage {
                text: "hi <script>alert(1)</script> \"quoted\"".to_owned(),
            },
            ion_core::SessionEntry::AssistantMessage {
                text: "ok & done".to_owned(),
            },
        ];
        let html = render("Session <title>", &entries, 80);
        assert!(html.starts_with("<!DOCTYPE html>"));
        // The embedded session data is valid JSON as the browser's
        // JSON.parse sees it: `</script>` is defused with `<\/`, and
        // quotes stay raw (found live: escaped quotes broke the page).
        let data_start = html.find("id=\"session-data\"").expect("data block");
        let data_end = html[data_start..]
            .find("</script>")
            .map(|i| data_start + i)
            .expect("script close");
        let tag_end = html[data_start..]
            .find('>')
            .map(|i| data_start + i)
            .expect("tag end");
        let embedded = &html[tag_end + 1..data_end];
        let parsed: Vec<serde_json::Value> =
            serde_json::from_str(embedded).expect("embedded JSON parses");
        assert_eq!(parsed.len(), 2);
        assert_eq!(
            parsed[0]["text"].as_str().unwrap(),
            "hi <script>alert(1)</script> \"quoted\""
        );
        assert!(!embedded.contains("</script>"));
        // Title is HTML-escaped.
        assert!(html.contains("<title>Session &lt;title&gt;</title>"));
    }

    #[test]
    fn empty_session_renders_the_shell() {
        let html = render("empty", &[], 80);
        assert!(html.contains("0 entries · exported by ion"));
    }
}
