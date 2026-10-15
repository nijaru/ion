use std::{
    io::{BufWriter, Write},
    path::PathBuf,
};

use ion_ai::{
    Content, ImageContent, Message, ModelExecution, ModelRef, ProviderReplay, Reasoning,
    ResponseTermination, ToolCall, ToolResult, Usage,
};
use ion_core::{
    ChildIntent, ChildOutcome, CodingToolOutput, ModelContextSnapshot, SessionEntry, SessionView,
    StoredToolActivity, ToolOccurrence, ToolOutcome, ToolResultProjection, ToolSettlement,
    TurnEndReason, UnobservedUserShell, UserShellOutcome,
};
use serde::{Serialize, Serializer, ser::SerializeStruct};

// Remote derives check the typed fields/variants against their owners. Only
// attachment-bearing fields delegate to this projection; arbitrary JSON and
// opaque replay serialize unchanged. CLI streams directly from borrowed facts.
#[derive(Serialize)]
#[serde(remote = "SessionView")]
struct Inspection {
    id: uuid::Uuid,
    cwd: Option<PathBuf>,
    name: Option<String>,
    #[serde(serialize_with = "omit_each")]
    entries: Vec<SessionEntry>,
    #[serde(serialize_with = "omit_each")]
    messages: Vec<Message>,
    unfinished_turn: Option<u64>,
    unfinished_user_shell: Option<UnobservedUserShell>,
    last_end: Option<(u64, TurnEndReason)>,
    last_model: Option<ModelRef>,
    reasoning: Reasoning,
    last_effective_model: Option<ModelRef>,
    last_context: Option<ModelContextSnapshot>,
    compacted_through: Option<u64>,
    last_execution: Option<ModelExecution>,
    last_usage: Option<Usage>,
}

#[derive(Serialize)]
#[serde(
    remote = "SessionEntry",
    tag = "kind",
    content = "data",
    rename_all = "snake_case"
)]
enum EntryInspection {
    ReasoningSelected {
        reasoning: Reasoning,
    },
    ModelSelected {
        model: ModelRef,
    },
    EffectiveModelChanged {
        turn: u64,
        model: ModelRef,
    },
    ProviderReplayRebased {
        turn: u64,
    },
    ModelContextChanged {
        turn: u64,
        context: ModelContextSnapshot,
    },
    UserShellAdmitted {
        command: String,
        exclude_from_context: bool,
    },
    UserShellSettled {
        admission_entry: u64,
        outcome: UserShellOutcome,
    },
    Steering {
        turn: u64,
        #[serde(serialize_with = "omit")]
        input: Message,
    },
    CacheWarm {
        turn: u64,
        execution: ModelExecution,
        usage: Usage,
    },
    Compacted {
        through_entry: u64,
        summary: String,
        execution: ModelExecution,
        usage: Usage,
    },
    TurnStarted {
        turn: u64,
        #[serde(serialize_with = "omit")]
        input: Message,
        model: ModelRef,
        #[serde(skip_serializing_if = "Option::is_none")]
        request: Option<ion_core::RequestKey>,
        #[serde(skip_serializing_if = "Option::is_none")]
        recovery: Option<ion_core::RecoveryBinding>,
    },
    TurnResumed {
        turn: u64,
        checkpoint: u64,
    },
    Assistant {
        turn: u64,
        #[serde(serialize_with = "omit")]
        message: Message,
        tool_activities: Vec<StoredToolActivity>,
        execution: ModelExecution,
        usage: Usage,
        termination: ResponseTermination,
    },
    ToolResult {
        turn: u64,
        #[serde(serialize_with = "omit")]
        result: ToolSettlement,
    },
    ChildToolAdmitted {
        turn: u64,
        intent: ChildIntent,
    },
    ChildToolResult {
        turn: u64,
        parent: ToolOccurrence,
        child: usize,
        #[serde(serialize_with = "omit")]
        outcome: ChildOutcome,
    },
    TurnEnded {
        turn: u64,
        reason: TurnEndReason,
    },
}

#[derive(Serialize)]
#[serde(remote = "Message")]
struct MessageInspection {
    role: ion_ai::Role,
    #[serde(serialize_with = "omit_each")]
    content: Vec<Content>,
    provider_replay: Option<ProviderReplay>,
}

#[derive(Serialize)]
#[serde(remote = "Content")]
enum ContentInspection {
    Text(String),
    Thinking(String),
    Image(#[serde(serialize_with = "omit")] ImageContent),
    ToolCall(ToolCall),
    ToolResult(#[serde(serialize_with = "omit")] ToolResult),
}

#[derive(Serialize)]
#[serde(remote = "ToolResult")]
struct ResultInspection {
    call_id: String,
    name: String,
    result: serde_json::Value,
    #[serde(serialize_with = "omit_each", skip_serializing_if = "Vec::is_empty")]
    images: Vec<ImageContent>,
    is_error: bool,
}

#[derive(Serialize)]
#[serde(remote = "ToolSettlement")]
struct SettlementInspection {
    call_id: String,
    name: String,
    #[serde(serialize_with = "omit")]
    outcome: ToolOutcome,
}

#[derive(Serialize)]
#[serde(remote = "ToolOutcome", tag = "state", rename_all = "snake_case")]
enum OutcomeInspection {
    Observed {
        #[serde(serialize_with = "omit")]
        output: CodingToolOutput,
        projection: ToolResultProjection,
    },
    NotDispatched {
        reason: String,
    },
    Unknown,
}

#[derive(Serialize)]
#[serde(remote = "ChildOutcome", tag = "state", rename_all = "snake_case")]
enum ChildInspection {
    Observed {
        #[serde(serialize_with = "omit")]
        output: CodingToolOutput,
    },
    NotDispatched {
        reason: String,
    },
    Unknown,
}

#[derive(Serialize)]
#[serde(remote = "CodingToolOutput")]
struct OutputInspection {
    value: serde_json::Value,
    #[serde(serialize_with = "omit_each")]
    images: Vec<ImageContent>,
    is_error: bool,
}

struct OmitImages<'a, T>(&'a T);

// These wrappers only select the checked remote derive. They never inspect the
// spelling of keys in a Value to infer whether something is an attachment.
macro_rules! projection {
    ($($source:ty => $projection:ident),+ $(,)?) => {
        $(impl Serialize for OmitImages<'_, $source> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                $projection::serialize(self.0, serializer)
            }
        })+
    };
}
projection!(
    SessionView => Inspection,
    SessionEntry => EntryInspection,
    Message => MessageInspection,
    Content => ContentInspection,
    ToolResult => ResultInspection,
    ToolSettlement => SettlementInspection,
    ToolOutcome => OutcomeInspection,
    ChildOutcome => ChildInspection,
    CodingToolOutput => OutputInspection,
);

impl Serialize for OmitImages<'_, ImageContent> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut image = serializer.serialize_struct("ImageContent", 2)?;
        image.serialize_field("mime_type", &self.0.mime_type())?;
        image.serialize_field(
            "data",
            &format!(
                "[base64 image data omitted: {} characters]",
                self.0.data().len()
            ),
        )?;
        image.end()
    }
}

fn omit<T, S: Serializer>(value: &T, serializer: S) -> Result<S::Ok, S::Error>
where
    for<'a> OmitImages<'a, T>: Serialize,
{
    OmitImages(value).serialize(serializer)
}

fn omit_each<T, S: Serializer>(items: &[T], serializer: S) -> Result<S::Ok, S::Error>
where
    for<'a> OmitImages<'a, T>: Serialize,
{
    serializer.collect_seq(items.iter().map(OmitImages))
}

pub(crate) fn to_value(view: &SessionView) -> anyhow::Result<serde_json::Value> {
    Ok(serde_json::to_value(OmitImages(view))?)
}

pub(crate) fn write(view: &SessionView, output: &mut impl Write) -> anyhow::Result<()> {
    let mut output = BufWriter::new(output);
    let result = (|| {
        serde_json::to_writer_pretty(&mut output, &OmitImages(view))?;
        output.write_all(b"\n")?;
        output.flush()?;
        Ok(())
    })();
    // BufWriter's Drop would retry retained bytes after a failed flush. Discard
    // them instead: partial diagnostic output is an error, never a retry.
    let _ = output.into_parts();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_ai::{Role, ToolSpec};
    use serde_json::json;

    fn raw() -> serde_json::Value {
        let image = json!({"mime_type":"image/png","data":"keep raw JSON"});
        json!({"mime_type":"image/png","data":"keep raw JSON", "Image":image,
            "ToolResult":{"images":[image]}, "images":[image], "content":[{"Image":image}]})
    }

    fn message(content: Vec<Content>) -> Message {
        Message {
            role: Role::User,
            content,
            provider_replay: None,
        }
    }

    fn view() -> SessionView {
        let model = ModelRef {
            provider: "test".into(),
            model: "test".into(),
        };
        let image = ion_ai::normalize_rgba(1, 1, vec![20, 40, 60, 255])
            .unwrap()
            .content;
        let input = message(vec![
            Content::Text("literal \u{1b}[31m".into()),
            Content::Image(image.clone()),
        ]);
        let spec = ToolSpec {
            name: "probe".into(),
            description: "Probe".into(),
            input_schema: raw(),
        };
        let context = ModelContextSnapshot {
            instructions: "inspect only".into(),
            tools: vec![spec.clone()],
        };
        let call = ToolCall {
            id: "call".into(),
            name: "probe".into(),
            arguments: raw(),
            raw_arguments: None,
        };
        let mut assistant = message(vec![
            Content::ToolCall(call.clone()),
            Content::Image(image.clone()),
        ]);
        assistant.role = Role::Assistant;
        assistant.provider_replay = Some(ProviderReplay::new("test", "fixture", raw()));
        let output = CodingToolOutput {
            value: raw(),
            images: vec![image.clone()],
            is_error: false,
        };
        let parent = ToolOccurrence {
            assistant_entry: 5,
            ordinal: 0,
        };
        let result = ToolResult {
            call_id: "call".into(),
            name: "probe".into(),
            result: raw(),
            images: vec![image],
            is_error: false,
        };
        let empty = ToolResult {
            images: vec![],
            ..result.clone()
        };
        let messages = vec![
            input.clone(),
            assistant.clone(),
            message(vec![
                Content::ToolResult(result),
                Content::ToolResult(empty),
            ]),
        ];
        let entries = vec![
            SessionEntry::ModelContextChanged {
                turn: 1,
                context: context.clone(),
            },
            SessionEntry::TurnStarted {
                turn: 1,
                input: input.clone(),
                model: model.clone(),
                request: None,
                recovery: None,
            },
            SessionEntry::Steering { turn: 1, input },
            SessionEntry::Assistant {
                turn: 1,
                message: assistant,
                tool_activities: vec![],
                execution: ModelExecution {
                    route: ion_ai::ModelRoute::direct(
                        model.clone(),
                        ion_ai::ModelRouteReason::UserRequest,
                    ),
                    returned_model: None,
                },
                usage: Usage::unknown(),
                termination: ResponseTermination::Completed,
            },
            SessionEntry::ToolResult {
                turn: 1,
                result: ToolSettlement {
                    call_id: "call".into(),
                    name: "probe".into(),
                    outcome: ToolOutcome::Observed {
                        output: output.clone(),
                        projection: ToolResultProjection::ImagesUnsupported,
                    },
                },
            },
            SessionEntry::ChildToolAdmitted {
                turn: 1,
                intent: ChildIntent {
                    parent,
                    child: 0,
                    call,
                    definition: spec,
                    activity: ion_core::ToolActivity::external("probe"),
                },
            },
            SessionEntry::ChildToolResult {
                turn: 1,
                parent,
                child: 0,
                outcome: ChildOutcome::Observed { output },
            },
            SessionEntry::ChildToolResult {
                turn: 1,
                parent,
                child: 1,
                outcome: ChildOutcome::Unknown,
            },
            SessionEntry::ChildToolResult {
                turn: 1,
                parent,
                child: 2,
                outcome: ChildOutcome::NotDispatched {
                    reason: "not issued".into(),
                },
            },
            SessionEntry::UserShellSettled {
                admission_entry: 12,
                outcome: UserShellOutcome::Observed {
                    output: raw(),
                    is_error: false,
                },
            },
        ];
        SessionView {
            id: Default::default(),
            cwd: Some(PathBuf::from("/inspection-fixture")),
            name: Some("inspect".into()),
            entries,
            messages,
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
    fn inspection_omits_typed_attachments_without_rewriting_raw_json_or_facts() {
        let view = view();
        let original = serde_json::to_value(&view).unwrap();
        let mut expected = original.clone();
        let marker = format!(
            "[base64 image data omitted: {} characters]",
            view.messages[0]
                .content
                .iter()
                .find_map(|part| {
                    if let Content::Image(image) = part {
                        Some(image.data().len())
                    } else {
                        None
                    }
                })
                .unwrap()
        );
        // Independent typed locations, not the production projection/walker.
        for index in [1, 2] {
            expected["entries"][index]["data"]["input"]["content"][1]["Image"]["data"] =
                json!(marker);
        }
        expected["entries"][3]["data"]["message"]["content"][1]["Image"]["data"] = json!(marker);
        expected["entries"][4]["data"]["result"]["outcome"]["output"]["images"][0]["data"] =
            json!(marker);
        expected["entries"][6]["data"]["outcome"]["output"]["images"][0]["data"] = json!(marker);
        for index in [0, 1] {
            expected["messages"][index]["content"][1]["Image"]["data"] = json!(marker);
        }
        expected["messages"][2]["content"][0]["ToolResult"]["images"][0]["data"] = json!(marker);
        let mut bytes = Vec::new();
        write(&view, &mut bytes).unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            expected
        );
        assert_eq!(
            to_value(&view).unwrap(),
            expected,
            "RPC and CLI must share one projection"
        );
        assert_eq!(
            serde_json::to_value(&view).unwrap(),
            original,
            "inspection changed facts"
        );
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
