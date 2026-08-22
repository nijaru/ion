//! Print frontend: projects session events to a writer (DESIGN.md §21.1).
//! One frontend over the runtime contract; it owns no agent truth.

use std::io::Write;

use ion_core::{CommandError, RuntimeError, RuntimeEvent, SessionHandle};

pub struct PrintFrontend<W> {
    writer: W,
}

impl<W: Write> PrintFrontend<W> {
    pub fn new(writer: W) -> Self {
        Self { writer }
    }

    pub async fn run(
        &mut self,
        session: &SessionHandle,
        prompt: impl Into<String>,
    ) -> Result<(), RuntimeError> {
        let (_snapshot, mut events) = session.subscribe().await?;
        session.submit(prompt).await?;
        loop {
            match events.recv().await? {
                RuntimeEvent::AssistantTextDelta { text, .. } => {
                    self.writer
                        .write_all(text.as_bytes())
                        .map_err(|err| RuntimeError::OperationFailed(err.to_string()))?;
                    self.writer
                        .flush()
                        .map_err(|err| RuntimeError::OperationFailed(err.to_string()))?;
                }
                // Tool settlement is durable-state news, not output.
                RuntimeEvent::ToolStarted { .. } | RuntimeEvent::ToolSettled { .. } => {}
                // Print mode is quiet output only.
                RuntimeEvent::ThinkingDelta { .. } => {}
                RuntimeEvent::OperationFinished { .. } => return Ok(()),
                RuntimeEvent::OperationCancelled { .. } => {
                    return Err(RuntimeError::OperationCancelled);
                }
                RuntimeEvent::OperationApprovalRequired { tool, .. } => {
                    return Err(RuntimeError::ApprovalRequired { tool });
                }
                RuntimeEvent::OperationFailed { message, .. } => {
                    return Err(RuntimeError::OperationFailed(message));
                }
                RuntimeEvent::SessionClosed { .. } => {
                    return Err(RuntimeError::Command(CommandError::Closed));
                }
                RuntimeEvent::OperationStarted { .. } => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_core::{Runtime, ScriptedMessage, ScriptedProvider, SessionStore, ToolRegistry};

    #[tokio::test]
    async fn print_frontend_writes_streamed_text() {
        let store = SessionStore::open_in_memory().expect("in-memory store");
        let runtime = Runtime::start_with_store(
            ScriptedProvider::new(vec![
                ScriptedMessage::text("hel"),
                ScriptedMessage::text("lo\n"),
            ]),
            ToolRegistry::default(),
            store,
        );
        let session = runtime.session();
        let mut buf = Vec::new();
        PrintFrontend::new(&mut buf)
            .run(&session, "hi")
            .await
            .expect("print");
        assert_eq!(String::from_utf8(buf).expect("utf8"), "hello\n");
        session.close().await.expect("close");
        runtime.join().await.expect("join");
    }
}
