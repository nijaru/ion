//! Typed RPC translation for the shared Host's single live responder lease.
use super::*;
use ion_host::{MAX_ANSWER_BYTES, MAX_OPTIONS, MAX_QUESTIONS, QuestionAnswer, QuestionRequestId};
use serde::Deserialize;
use tokio::io::AsyncWrite;

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Answer<'a> {
    Option { index: usize },
    Options { indices: Vec<usize> },
    Text { text: &'a str },
}

impl Control {
    pub(super) fn get_question(&self, value: &Value) -> Result<Value> {
        fields(value, &["type", "id"])?;
        Ok(json!({"question":self.host.pending_question()}))
    }

    pub(super) fn answer_question(&self, value: &Value) -> Result<Value> {
        fields(value, &["type", "id", "request", "answers"])?;
        let request = QuestionRequestId(required_string(value, "request")?.parse()?);
        let answers = value["answers"]
            .as_array()
            .context("answers must be an array")?;
        ensure!(answers.len() <= MAX_QUESTIONS, "too many question answers");
        let answers = answers
            .iter()
            .map(|answer| {
                Ok(match Answer::deserialize(answer)? {
                    Answer::Option { index } => QuestionAnswer::Option { index },
                    Answer::Options { indices } => {
                        ensure!(indices.len() <= MAX_OPTIONS, "too many option indices");
                        QuestionAnswer::Options { indices }
                    }
                    Answer::Text { text } => {
                        ensure!(
                            text.len() <= MAX_ANSWER_BYTES,
                            "custom answer exceeds {MAX_ANSWER_BYTES} UTF-8 bytes"
                        );
                        QuestionAnswer::Text {
                            text: text.to_owned(),
                        }
                    }
                })
            })
            .collect::<Result<Vec<_>>>()?;
        self.question_responder
            .as_ref()
            .context("question responder is unavailable")?
            .answer(request, answers)?;
        Ok(json!({"request":request,"disposition":"submitted"}))
    }

    pub(super) fn cancel_question(&self, value: &Value) -> Result<Value> {
        fields(value, &["type", "id", "request"])?;
        let request = QuestionRequestId(required_string(value, "request")?.parse()?);
        self.question_responder
            .as_ref()
            .context("question responder is unavailable")?
            .cancel(request)?;
        Ok(json!({"request":request,"disposition":"submitted"}))
    }

    /// Retain one invalidation until it fits, even when model progress has
    /// already gapped. The request itself stays solely in Host custody.
    pub(super) fn publish_question_change<W: AsyncWrite + Unpin>(
        &mut self,
        output: &mut Output<W>,
    ) -> Result<()> {
        if self.question_changed
            && output.is_healthy()
            && output.try_enqueue(json!({"type":"question_changed","resync":"get_question"}))?
        {
            self.question_changed = false;
        }
        Ok(())
    }
}
