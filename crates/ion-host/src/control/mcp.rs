//! Explicit user management, separate from Core operations and input custody.
use super::*;

pub(super) struct Reconnect<M> {
    pub id: OperationId,
    pub server: String,
    pub metadata: M,
    pub reservation: InputReservation,
    pub work: crate::McpReconnect,
}

impl<M: Serialize + Send + 'static> SessionControl<M> {
    /// Admit a reconnect without stopping any whole Turn. The eventual result
    /// has independent reliable custody; this return only acknowledges admission.
    pub fn reconnect_mcp(
        &mut self,
        server: &str,
        metadata: M,
    ) -> Result<OperationId, (M, anyhow::Error)> {
        let prepared = (|| {
            if let Some(owner) = self.background() {
                owner.ensure_open()?;
            }
            ensure!(!self.closed, "session control is closed");
            let tools = self
                .setup
                .mcp_tools()
                .context("MCP management is not enabled by this application")?;
            let reservation = self.budget.reserve(
                &Message::user_input(server.to_owned(), []),
                &metadata,
                self.setup.runtime.agent.limits(),
            )?;
            let mut custody = lock(&self.custody);
            ensure!(
                self.mcp_reconnect.is_none()
                    && !custody
                        .updates
                        .iter()
                        .any(|update| matches!(update, ControlUpdate::McpReconnectFinished(_))),
                "drain the previous MCP reconnect result before starting another"
            );
            ensure!(
                custody.occupied < MAX_ADMISSIONS,
                "session result/admission capacity is full"
            );
            // Steering can admit from another thread while a Turn runs.
            // Reserve under the same lock as its capacity check, before effects.
            custody.occupied += 1;
            drop(custody);
            let work = match tools.reconnect_with_stop(server, self.work_stop()) {
                Ok(work) => work,
                Err(error) => {
                    lock(&self.custody).occupied -= 1;
                    return Err(error);
                }
            };
            Ok((reservation, work))
        })();
        let (reservation, work) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => return Err((metadata, error)),
        };
        let id = OperationId(identity());
        self.mcp_reconnect = Some(Reconnect {
            id,
            server: server.to_owned(),
            metadata,
            reservation,
            work,
        });
        Ok(id)
    }

    pub(super) fn finish_mcp_reconnect(&mut self, result: Result<crate::McpReconnectReport>) {
        let reconnect = self.mcp_reconnect.take().expect("retained MCP reconnect");
        let stop_requested = reconnect.work.is_stopped();
        lock(&self.custody)
            .updates
            .push_back(ControlUpdate::McpReconnectFinished(McpReconnectFinished {
                operation: reconnect.id,
                server: reconnect.server,
                metadata: reconnect.metadata,
                result,
                stop_requested,
                reservation: Some(reconnect.reservation),
            }));
    }
}
