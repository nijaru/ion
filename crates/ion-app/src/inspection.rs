use std::{
    io::{BufWriter, Write},
    path::PathBuf,
};

use ion_ai::{Message, ModelExecution, ModelRef, Reasoning, Usage};
use ion_core::{
    ModelContextSnapshot, SessionEntry, SessionView, TurnEndReason, UnobservedUserShell,
};
use serde::{
    Serialize, Serializer,
    ser::{Error as _, SerializeSeq},
};

// A borrowed diagnostic projection, not another Session representation. Remote
// derive checks these field types against Core; only potentially image-bearing
// fields need temporary JSON values. Entries/messages are redacted one at a time.
#[derive(Serialize)]
#[serde(remote = "SessionView")]
struct Inspection {
    cwd: PathBuf,
    name: Option<String>,
    #[serde(serialize_with = "redacted_items")]
    entries: Vec<SessionEntry>,
    #[serde(serialize_with = "redacted_items")]
    messages: Vec<Message>,
    unfinished_turn: Option<u64>,
    unfinished_user_shell: Option<UnobservedUserShell>,
    last_end: Option<(u64, TurnEndReason)>,
    last_model: Option<ModelRef>,
    reasoning: Reasoning,
    last_effective_model: Option<ModelRef>,
    #[serde(serialize_with = "redacted_value")]
    last_context: Option<ModelContextSnapshot>,
    compacted_through: Option<u64>,
    last_execution: Option<ModelExecution>,
    last_usage: Option<Usage>,
}

pub(crate) fn write(view: &SessionView, output: &mut impl Write) -> anyhow::Result<()> {
    let mut output = BufWriter::new(output);
    let result = (|| {
        Inspection::serialize(view, &mut serde_json::Serializer::pretty(&mut output))?;
        output.write_all(b"\n")?;
        output.flush()?;
        Ok(())
    })();
    // BufWriter's Drop would retry retained bytes after a failed flush. Discard
    // them instead: partial diagnostic output is an error, never a retry.
    let _ = output.into_parts();
    result
}

fn redacted_items<T: Serialize, S: Serializer>(
    items: &[T],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut sequence = serializer.serialize_seq(Some(items.len()))?;
    for item in items {
        sequence.serialize_element(&Redacted(item))?;
    }
    sequence.end()
}

fn redacted_value<T: Serialize, S: Serializer>(
    value: &T,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    Redacted(value).serialize(serializer)
}

struct Redacted<'a, T>(&'a T);

impl<T: Serialize> Serialize for Redacted<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut value = serde_json::to_value(self.0).map_err(S::Error::custom)?;
        redact_image_payloads(&mut value);
        value.serialize(serializer)
    }
}

pub(crate) fn redact_image_payloads(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(fields) => {
            if fields
                .get("mime_type")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|mime| mime.starts_with("image/"))
                && let Some(serde_json::Value::String(data)) = fields.get_mut("data")
            {
                *data = format!("[base64 image data omitted: {} characters]", data.len());
            }
            for child in fields.values_mut() {
                redact_image_payloads(child);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                redact_image_payloads(child);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_ai::{Content, Role, ToolSpec};
    use serde_json::json;

    fn view() -> SessionView {
        let model = ModelRef {
            provider: "test".into(),
            model: "test".into(),
        };
        let image = ion_ai::normalize_rgba(1, 1, vec![20, 40, 60, 255])
            .unwrap()
            .content;
        let input = Message {
            role: Role::User,
            content: vec![
                Content::Text(
                    "literal \u{1b}[31m {\"mime_type\":\"image/png\",\"data\":\"keep text\"}"
                        .into(),
                ),
                Content::Image(image),
            ],
            provider_replay: None,
        };
        let context = ModelContextSnapshot {
            instructions: "inspect only".into(),
            tools: vec![ToolSpec {
                name: "probe".into(),
                description: "Probe".into(),
                input_schema: json!({
                    "type":"object", "examples":[
                        {"nested":{"mime_type":"image/png","data":"1234"}},
                        {"mime_type":"text/plain","data":"keep"},
                        {"mime_type":"image/png","data":7}
                    ]
                }),
            }],
        };
        SessionView {
            cwd: PathBuf::from("/inspection-fixture"),
            name: Some("inspect".into()),
            entries: vec![
                SessionEntry::ReasoningSelected {
                    reasoning: Reasoning::High,
                },
                SessionEntry::TurnStarted {
                    turn: 1,
                    input: input.clone(),
                    model: model.clone(),
                },
                SessionEntry::ModelContextChanged {
                    turn: 1,
                    context: context.clone(),
                },
            ],
            messages: vec![input],
            unfinished_turn: Some(1),
            unfinished_user_shell: None,
            last_end: None,
            last_model: Some(model),
            reasoning: Reasoning::High,
            last_effective_model: None,
            last_context: Some(context),
            compacted_through: None,
            last_execution: None,
            last_usage: None,
        }
    }

    #[test]
    fn streamed_inspection_preserves_json_and_only_omits_image_payloads() {
        let view = view();
        let original = serde_json::to_value(&view).unwrap();
        let mut expected = original.clone();
        redact_image_payloads(&mut expected);
        let mut bytes = Vec::new();
        write(&view, &mut bytes).unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        let actual: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(
            serde_json::to_value(&view).unwrap(),
            original,
            "inspection mutated facts"
        );
        let image_length = original["messages"][0]["content"][1]["Image"]["data"]
            .as_str()
            .unwrap()
            .len();
        assert_eq!(
            actual["messages"][0]["content"][1]["Image"]["data"],
            format!("[base64 image data omitted: {image_length} characters]")
        );
        let examples = &actual["last_context"]["tools"][0]["input_schema"]["examples"];
        assert_eq!(
            examples[0]["nested"]["data"],
            "[base64 image data omitted: 4 characters]"
        );
        assert_eq!(examples[1]["data"], "keep");
        assert_eq!(examples[2]["data"], 7);
    }

    #[test]
    fn partial_output_failure_does_not_retry_buffered_bytes() {
        struct FailedOutput {
            calls: usize,
        }
        impl Write for FailedOutput {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.calls += 1;
                if self.calls == 1 {
                    return Ok(bytes.len().min(3));
                }
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "owned output failure",
                ))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        // Cover failure during serialization and in its final flush.
        for length in [0, 18_000] {
            let mut view = view();
            view.name = Some("x".repeat(length));
            let mut output = FailedOutput { calls: 0 };
            assert!(
                write(&view, &mut output)
                    .unwrap_err()
                    .to_string()
                    .contains("owned output failure")
            );
            assert_eq!(output.calls, 2, "buffer retried after output failure");
        }
    }
}
