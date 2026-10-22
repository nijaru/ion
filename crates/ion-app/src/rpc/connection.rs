//! Foreground I/O, bounded protocol admission, and unconditional shared shutdown.
use super::{
    Control, failure,
    input::{CommandReader, Input},
};
use crate::publication::{Output, ReplyCredits};
use anyhow::Result;
use ion_host::ControlUpdate;
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncWrite};

pub(super) const REPLY_SLOTS: usize = 32;

/// Each admitted input produces at most four reliable frames: queued/accepted
/// acknowledgement, return or follow_up_started, root settlement and (if still
/// pending) a coalesced gap before settlement. Direct actions produce at most
/// three, ordinary requests one. Credits are retained through Host
/// custody AND physical delivery, so consuming a Host update cannot reopen
/// transport admission behind a blocked pipe. Protocol input is capped at 8 MiB;
/// Host bounds retained inputs and native results separately. One dedicated
/// inspection slot prevents materializing many unbounded Session views.
///
/// When a lane is exhausted, commands are consumed without effects and counted
/// in a coalesced publication_gap (not an unbounded list of rejected IDs). Two
/// reserved control slots keep abort/clear_queue and question replies available
/// behind bulk replies.
/// Abort always conveys stop intent even if its own reply lane is exhausted;
/// such an unacknowledged abort has its own counter in the gap. No capacity refusal
/// starts work. Clients must reconcile gaps, not assume exactly-once requests.
pub(super) async fn connection<R: AsyncBufRead + Unpin, W: AsyncWrite + Unpin>(
    mut control: Control,
    mut input: CommandReader<R>,
    mut output: Output<W>,
) -> Result<()> {
    let replies = ReplyCredits::new(REPLY_SLOTS);
    let inspections = ReplyCredits::new(1);
    let controls = ReplyCredits::new(2);
    let mut poll_host = true;
    let application_owner = control.host.owner();
    let background = control.host.background();
    let mut background_revision = 0;
    let result: Result<()> = async {
        // Bind the actual transport consumer before admitting any work. Setup
        // failures still pass through the same unconditional stop/join below.
        control.question_responder = control.host.attach_question_responder()?;
        loop {
            control.publish_gap(&mut output)?;
            control.publish_question_change(&mut output)?;
            tokio::select! {
                published = output.advance(), if output.has_pending() => { published?; }
                update = control.host.next(), if poll_host => {
                    poll_host = !matches!(update, ControlUpdate::Idle);
                    control.update(update, &mut output)?;
                }
                revision = async { background.as_ref().expect("guarded owner").changed(background_revision).await }, if background.is_some() => {
                    background_revision = revision;
                    let owner = background.as_ref().expect("guarded owner");
                    owner.check()?;
                    if output.can_acquire() { output.enqueue(serde_json::json!({"type":"background_changed","revision":revision,"resync":"get_background"}))?; }
                    else { control.mark_gap(0); }
                }
                line = input.next() => {
                    let value = match line? {
                        Input::Eof => break,
                        Input::Line(line) => serde_json::from_slice::<Value>(&line).map_err(|error| error.to_string()),
                        Input::TooLarge => Err("command exceeds 8 MiB".into()),
                        Input::Incomplete => Err("command is missing its final newline".into()),
                    };
                    let command = value.as_ref().ok().filter(|value| value.is_object() && value.get("id").is_none_or(Value::is_string)).and_then(|value| value.get("type")).and_then(Value::as_str).unwrap_or("parse");
                    let pool = match command {
                        "abort" | "clear_queue" | "cancel_background" | "answer_question" | "cancel_question" => &controls,
                        "get_question" | "get_context" | "inspect" | "get_request" | "get_background" | "acknowledge_background" | "list_sessions" | "list_turns" | "list_models" | "list_resources" => &inspections,
                        _ => &replies,
                    };
                    let Some(credit) = pool.acquire() else {
                        if command == "abort" {
                            control.host.request_stop();
                            poll_host = true;
                            control.unacknowledged_stops = control.unacknowledged_stops.saturating_add(1);
                        } else if command == "cancel_background" && value.as_ref().is_ok_and(|value| control.cancel_background(value).is_ok()) {
                            control.unacknowledged_stops = control.unacknowledged_stops.saturating_add(1);
                        } else {
                            control.refused = control.refused.saturating_add(1);
                        }
                        continue;
                    };
                    let reply = match value {
                        Ok(value) => control.command(value, credit),
                        Err(error) => Some((failure(None, "parse", &error), credit)),
                    };
                    // Even an idle command may have created reliable Host updates.
                    poll_host = true;
                    if let Some((record, credit)) = reply { output.enqueue_reliable(record, credit)?; }
                }
            }
        }
        Ok(())
    }.await;

    // No publication wait can postpone stop/join. Borrowed shutdown itself is
    // cancellation safe; this foreground owner awaits it to completion on every
    // EOF/read/write/protocol-invariant/unknown-task failure path.
    if let Some(owner) = &application_owner {
        owner.request_shutdown();
    }
    if control.host.is_busy() {
        // shutdown deliberately discards observations while joining. Zero is
        // a resync request, not a fabricated count of missing shell events.
        control.mark_gap(0);
    }
    let report = control.host.shutdown().await;
    let mut result = result;
    if let Some(owner) = &application_owner {
        result = combine_errors(
            result,
            owner.join_shutdown().await,
            "application cleanup also failed",
        );
    }
    for update in report.updates {
        result = combine_errors(
            result,
            control.update(update, &mut output),
            "Host recovery also failed",
        );
    }
    result = combine_errors(
        result,
        cleanup_result(report.cleanup_errors),
        "Host cleanup also failed",
    );
    // Recovery is encoded only after started work joined. A healthy final pipe
    // still requires a reader; there is deliberately no detached writer task.
    if output.is_healthy() {
        // At most one coalesced gap record is left outside the encoded queue.
        // If the bounded queue is full, make room only after settlement joined.
        while control.has_gap() && output.is_healthy() {
            if let Err(error) = control.publish_gap(&mut output) {
                result = combine_errors(result, Err(error), "RPC gap encoding also failed");
                break;
            }
            if control.has_gap()
                && output.has_pending()
                && let Err(error) = output.advance().await
            {
                result = combine_errors(result, Err(error), "RPC gap output also failed");
            }
        }
    }
    combine_errors(
        result,
        output.finish().await,
        "RPC recovery output also failed",
    )
}

/// Setup errors still release any application resources captured by the recipe.
pub(super) async fn startup_failure(control: &mut Control, error: anyhow::Error) -> Result<()> {
    let owner = control.host.owner();
    if let Some(owner) = &owner {
        owner.request_shutdown();
    }
    let report = control.host.shutdown().await;
    let result = combine_errors(
        Err(error),
        cleanup_result(report.cleanup_errors),
        "Host shutdown also failed",
    );
    if let Some(owner) = &owner {
        combine_errors(
            result,
            owner.join_shutdown().await,
            "application cleanup also failed",
        )
    } else {
        result
    }
}

pub(super) fn cleanup_result(errors: Vec<ion_core::CodingSessionError>) -> Result<()> {
    let mut result = Ok(());
    for error in errors {
        result = combine_errors(result, Err(error.into()), "additional cleanup failure");
    }
    result
}
pub(super) fn combine_errors(first: Result<()>, second: Result<()>, context: &str) -> Result<()> {
    match (first, second) {
        (Err(error), Err(second)) => Err(error.context(format!("{context}: {second:#}"))),
        (Err(error), _) => Err(error),
        (Ok(()), second) => second,
    }
}
