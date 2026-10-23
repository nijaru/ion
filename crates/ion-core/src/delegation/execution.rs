//! Full child execution and cleanup, independent of the current join owner.
use tokio_util::sync::CancellationToken;

use super::*;
use crate::CodingSession;

pub(super) type Completion = (DelegationOutcome, Option<String>);

pub(super) async fn run_child(
    record: DelegationRecord,
    prepared: ChildMaterialization,
    work: DelegationWork,
    stop: CancellationToken,
) -> Completion {
    if stop.is_cancelled() {
        return (
            DelegationOutcome::Joined {
                execution_timed_out: false,
                elapsed_ms: None,
                partial_output: None,
                receipt: None,
                error: Some("stopped before child materialization".into()),
                answer: None,
            },
            None,
        );
    }
    let child = match CodingSession::materialize_delegation(&record, prepared) {
        Ok(child) => child,
        Err(error) => {
            let message = format!(
                "child materialization failed at {}: {error}",
                record.path.display()
            );
            return (
                DelegationOutcome::Joined {
                    execution_timed_out: false,
                    elapsed_ms: None,
                    partial_output: None,
                    receipt: None,
                    error: Some(message.clone()),
                    answer: None,
                },
                Some(message),
            );
        }
    };
    let submission = request(&record);
    let key = submission.key.clone();
    // The callback receives the already-linked scope before child admission;
    // Host must use it rather than start with an independent fresh token.
    let completion = work(child.clone(), submission, stop).await;
    let shutdown = child.shutdown().await;
    let receipt = child.reconcile_request(&key, &record.spec.input);
    let mut infrastructure = None;
    let mut errors = Vec::new();
    let mut answer = None;
    if completion.execution_timed_out {
        errors.push(
            match (completion.elapsed_ms, record.spec.execution_timeout_ms) {
                (Some(ms), Some(budget)) => {
                    format!("child execution deadline exceeded after {ms} ms (budget {budget} ms)")
                }
                (Some(ms), None) => format!("child execution deadline exceeded after {ms} ms"),
                (None, _) => "child execution deadline exceeded".into(),
            },
        );
    }
    match completion.result {
        Ok(observed) => answer = observed,
        Err(DelegationFailure::Execution(error)) => errors.push(error),
        Err(DelegationFailure::Infrastructure(error)) => {
            infrastructure = Some(error.clone());
            errors.push(error);
        }
    }
    if let Err(error) = shutdown {
        infrastructure = Some(error.to_string());
        errors.push(error.to_string());
    }
    let receipt = match receipt {
        Ok(receipt) => receipt,
        Err(error) => {
            infrastructure = Some(error.to_string());
            errors.push(error.to_string());
            None
        }
    };
    if receipt.as_ref().is_none_or(|receipt| receipt.end.is_none()) && errors.is_empty() {
        let error = "child callback returned without a committed settlement".to_owned();
        infrastructure = Some(error.clone());
        errors.push(error);
    }
    if answer.is_some()
        && receipt
            .as_ref()
            .is_none_or(|receipt| receipt.end != Some(crate::TurnEndReason::Completed))
    {
        let error = "child reported an answer without its completed request".to_owned();
        infrastructure = Some(error.clone());
        errors.push(error);
    }
    if !errors.is_empty() {
        answer = None;
    }
    (
        DelegationOutcome::Joined {
            execution_timed_out: completion.execution_timed_out,
            elapsed_ms: completion.elapsed_ms,
            partial_output: if errors.is_empty() {
                None
            } else {
                completion.partial_output
            },
            receipt,
            error: (!errors.is_empty()).then(|| errors.join("; ")),
            answer,
        },
        infrastructure,
    )
}
