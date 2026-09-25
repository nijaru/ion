use super::*;
use crate::{InvocationId, SemanticCompatibilityId, StartReceipt};
use serde_json::{Value, json};

fn intent() -> ToolAttempt {
    ToolAttempt {
        id: AttemptId::new(1).unwrap(),
        invocation: InvocationId::new(2).unwrap(),
        ordinal: 1,
        generation: 0,
        executor: SemanticCompatibilityId::new("test-executor").unwrap(),
        progress: None,
        state: ToolAttemptState::IntentCommitted {
            start_receipt: None,
        },
    }
}

fn state(value: Value, effect: EffectSummary, receipt: Option<StartReceipt>) -> ToolAttemptState {
    ToolAttemptState::Settled {
        result: ToolResult {
            value,
            is_error: false,
            capture: OutputCapture::CompleteInline,
        },
        effect,
        receipt,
        retryable: true,
    }
}

#[test]
fn exact_state_record_boundary_still_preserves_terminal_effect() {
    let attempt = intent();
    let build = |len| {
        state(
            json!("ok"),
            EffectSummary::Receipt {
                kind: "effect".into(),
                data: json!("x".repeat(len)),
            },
            None,
        )
    };
    let base = serde_json::to_vec(&build(0)).unwrap().len();
    let settled = build(MAX_TOOL_RECORD_BYTES - base);
    assert_eq!(
        serde_json::to_vec(&settled).unwrap().len(),
        MAX_TOOL_RECORD_BYTES
    );
    assert!(!attempt_fits(&attempt, &settled));
    let normalized = bounded_effect(&attempt, settled, 1024, None);
    assert!(attempt_fits(&attempt, &normalized));
    assert!(matches!(
        normalized,
        ToolAttemptState::Settled {
            effect: EffectSummary::MayHaveMutated,
            retryable: false,
            result: ToolResult {
                capture: OutputCapture::Incomplete { .. },
                ..
            },
            ..
        }
    ));
}

#[test]
fn oversized_new_receipt_and_output_cannot_strand_mutated_terminal_attempt() {
    let attempt = intent();
    let new_receipt = StartReceipt {
        kind: "native".into(),
        data: json!("r".repeat(MAX_TOOL_RECORD_BYTES)),
    };
    let normalized = bounded_effect(
        &attempt,
        state(
            json!("x".repeat(90_000)),
            EffectSummary::KnownChanges {
                paths: vec!["changed.txt".into()],
            },
            Some(new_receipt),
        ),
        512,
        None,
    );
    assert!(attempt_fits(&attempt, &normalized));
    assert!(matches!(
        normalized,
        ToolAttemptState::Settled {
            effect: EffectSummary::KnownChanges { .. },
            receipt: None,
            retryable: false,
            result: ToolResult {
                capture: OutputCapture::Incomplete { .. },
                ..
            },
            ..
        }
    ));
}

#[test]
fn omitted_or_conflicting_backend_receipt_cannot_erase_prior_evidence() {
    let receipt = StartReceipt {
        kind: "prior".into(),
        data: json!("durable"),
    };
    let mut attempt = intent();
    attempt.state = ToolAttemptState::IntentCommitted {
        start_receipt: Some(receipt.clone()),
    };
    let missing = bounded_effect(
        &attempt,
        state(json!("ok"), EffectSummary::NoMutation, None),
        1024,
        None,
    );
    assert!(matches!(missing, ToolAttemptState::Settled {
        receipt: Some(actual), result: ToolResult { capture: OutputCapture::CompleteInline, .. }, ..
    } if actual == receipt));
    let conflicting = bounded_effect(
        &attempt,
        state(
            json!("ok"),
            EffectSummary::NoMutation,
            Some(StartReceipt {
                kind: "other".into(),
                data: json!(1),
            }),
        ),
        1024,
        None,
    );
    assert!(matches!(conflicting, ToolAttemptState::Settled {
        receipt: Some(actual), effect: EffectSummary::MayHaveMutated,
        result: ToolResult { capture: OutputCapture::Incomplete { .. }, .. },
        retryable: false,
    } if actual == receipt));
}

#[test]
fn prior_committed_receipt_survives_output_loss() {
    let receipt = StartReceipt {
        kind: "native".into(),
        data: json!("r".repeat(4000)),
    };
    let mut attempt = intent();
    attempt.state = ToolAttemptState::IntentCommitted {
        start_receipt: Some(receipt.clone()),
    };
    let normalized = bounded_effect(
        &attempt,
        state(
            json!("x".repeat(90_000)),
            EffectSummary::KnownChanges {
                paths: vec!["changed.txt".into()],
            },
            Some(receipt.clone()),
        ),
        512,
        None,
    );
    assert!(attempt_fits(&attempt, &normalized));
    assert!(matches!(normalized, ToolAttemptState::Settled {
        receipt: Some(actual), retryable: false, ..
    } if actual == receipt));
}
