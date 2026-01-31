use crate::agent::AgentEvent;
use crate::provider::{ContentBlock, ToolCallEvent};
use crate::session::Session;
use crate::tool::{ToolContext, ToolOrchestrator};
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

pub(crate) async fn execute_tools_parallel(
    orchestrator: &Arc<ToolOrchestrator>,
    session: &Session,
    tool_calls: Vec<ToolCallEvent>,
    tx: &mpsc::Sender<AgentEvent>,
    abort_token: CancellationToken,
) -> Result<Vec<ContentBlock>> {
    let mut set = JoinSet::new();
    let num_tools = tool_calls.len();

    if abort_token.is_cancelled() {
        return Err(anyhow::anyhow!("Cancelled"));
    }

    let ctx = ToolContext {
        working_dir: session.working_dir.clone(),
        session_id: session.id.clone(),
        abort_signal: session.abort_token.clone(),
        no_sandbox: session.no_sandbox,
        index_callback: None,
        discovery_callback: None,
    };

    for (index, call) in tool_calls.into_iter().enumerate() {
        let orchestrator = orchestrator.clone();
        let tx = tx.clone();
        let ctx_clone = ctx.clone();

        set.spawn(async move {
            let result = orchestrator
                .call_tool(&call.name, call.arguments, &ctx_clone)
                .await;
            let block = match result {
                Ok(res) => {
                    let _ = tx
                        .send(AgentEvent::ToolCallResult(
                            call.id.clone(),
                            res.content.clone(),
                            res.is_error,
                        ))
                        .await;
                    ContentBlock::ToolResult {
                        tool_call_id: call.id,
                        content: res.content,
                        is_error: res.is_error,
                    }
                }
                Err(e) => {
                    let error_msg = e.to_string();
                    let _ = tx
                        .send(AgentEvent::ToolCallResult(
                            call.id.clone(),
                            error_msg.clone(),
                            true,
                        ))
                        .await;
                    ContentBlock::ToolResult {
                        tool_call_id: call.id,
                        content: error_msg,
                        is_error: true,
                    }
                }
            };
            (index, block)
        });
    }

    let mut results = vec![None; num_tools];
    loop {
        tokio::select! {
            () = abort_token.cancelled() => {
                set.abort_all();
                return Err(anyhow::anyhow!("Cancelled"));
            }
            res = set.join_next() => {
                match res {
                    Some(Ok(result)) => {
                        let (index, block) = result;
                        results[index] = Some(block);
                    }
                    Some(Err(e)) => {
                        // JoinError: task panicked or was cancelled
                        if e.is_panic() {
                            return Err(anyhow::anyhow!("Tool task panicked unexpectedly"));
                        }
                        return Err(anyhow::anyhow!("Tool task cancelled"));
                    }
                    None => break,
                }
            }
        }
    }

    // Collect results, returning error if any slot is missing
    results
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| anyhow::anyhow!("Tool execution incomplete - some results missing"))
}
