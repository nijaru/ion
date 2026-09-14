use serde_json::json;

use super::*;
use crate::{
    ConversationId, InputBody, InputDisposition, InputMode, InputSender, InvocationKind,
    TaskKindName, TaskOutcome, TaskOutcomeKind, TaskStatus,
};

fn task_request(root: ConversationId, dependencies: Vec<crate::TaskId>) -> TaskRequest {
    TaskRequest {
        conversation_id: root,
        kind: TaskKindName::new("test").expect("task kind"),
        schema_version: 1,
        input: json!({"work": true}),
        dependencies,
    }
}

fn completed(value: &str) -> TaskOutcome {
    TaskOutcome {
        kind: TaskOutcomeKind::Completed,
        value: json!(value),
    }
}

fn input_request(target: ConversationId, mode: InputMode) -> InputRequest {
    InputRequest {
        target,
        sender: InputSender::User,
        mode,
        request_key: None,
        body: InputBody::Text("hello".to_owned()),
    }
}

#[test]
fn admission_policy_follows_mode_and_conversation_state() {
    // Idle: a turn-starting mode opens the turn; queue-only and notice do not.
    for (mode, starts) in [
        (InputMode::Submit, true),
        (InputMode::Steer, true),
        (InputMode::FollowUp, true),
        (InputMode::QueueOnly, false),
        (InputMode::Notice, false),
    ] {
        let mut session = Session::new().expect("session");
        let root = session.root_conversation();
        let receipt = session
            .admit_input(
                input_request(root, mode),
                Some(task_request(root, Vec::new())),
            )
            .expect("idle admission");
        assert_eq!(receipt.started_turn(), starts, "idle {mode:?}");
        let snapshot = session.snapshot();
        let disposition = &snapshot.inputs[0].disposition;
        if starts {
            // The turn-starting admission places the input in the same commit.
            assert_eq!(
                disposition.placement().expect("placed").turn,
                receipt.task_id.expect("turn root")
            );
            assert!(
                matches!(disposition, InputDisposition::Placed(_)),
                "idle {mode:?}"
            );
        } else {
            assert_eq!(disposition, &InputDisposition::Queued, "idle {mode:?}");
        }
    }

    // Busy: only submit is refused; every other mode queues.
    for (mode, refused) in [
        (InputMode::Submit, true),
        (InputMode::Steer, false),
        (InputMode::FollowUp, false),
        (InputMode::QueueOnly, false),
        (InputMode::Notice, false),
    ] {
        let mut session = Session::new().expect("session");
        let root = session.root_conversation();
        session
            .create_turn(task_request(root, Vec::new()))
            .expect("live turn");
        let before = session.snapshot();
        let receipt = session.admit_input(
            input_request(root, mode),
            Some(task_request(root, Vec::new())),
        );
        if refused {
            let error = receipt.expect_err("a busy submit is refused");
            assert!(matches!(error, SessionError::ForegroundTurnBusy(id) if id == root));
            assert_eq!(session.snapshot(), before, "a refusal admits nothing");
        } else {
            let receipt = receipt.expect("busy admission queues");
            assert!(!receipt.started_turn(), "busy {mode:?}");
            assert_eq!(
                session.snapshot().inputs[0].disposition,
                InputDisposition::Queued
            );
        }
    }
}

#[test]
fn reservation_checkpoint_and_recovery_are_generation_fenced() {
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let task = session
        .create_task(task_request(root, Vec::new()))
        .expect("task");
    let execute = session
        .reserve_task_invocation(task.task_id, InvocationKind::Execute)
        .expect("execute reservation");
    assert_eq!(execute.generation, 1);
    assert_eq!(execute.kind, InvocationKind::Execute);

    session
        .checkpoint_task(
            task.task_id,
            execute.generation,
            Some(json!({"phase": 1})),
            None,
        )
        .expect("checkpoint");
    let recover = session
        .reserve_task_invocation(task.task_id, InvocationKind::Recover)
        .expect("recovery reservation");
    assert_eq!(recover.generation, 2);

    let stale = session
        .checkpoint_task(
            task.task_id,
            execute.generation,
            Some(json!({"phase": 2})),
            None,
        )
        .expect_err("old generation must be fenced");
    assert!(matches!(stale, SessionError::StaleInvocation { .. }));

    let settlement = session
        .settle_task_with(
            task.task_id,
            recover.generation,
            completed("done"),
            None,
            |transaction| transaction.create_task(task_request(root, Vec::new())),
        )
        .expect("terminal plan");
    let successor = settlement.value;
    let snapshot = session.snapshot();
    let settled = snapshot
        .tasks
        .iter()
        .find(|record| record.id == task.task_id)
        .expect("settled task");
    assert!(matches!(settled.status, TaskStatus::Terminal(_)));
    assert!(snapshot.tasks.iter().any(|record| record.id == successor));
}

#[test]
fn cancellation_fences_normal_invocation_and_abort_gets_fresh_generation() {
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let task = session
        .create_task(task_request(root, Vec::new()))
        .expect("task");
    let execute = session
        .reserve_task_invocation(task.task_id, InvocationKind::Execute)
        .expect("execute reservation");
    let cancellation = session
        .mark_task_cancellation(task.task_id)
        .expect("cancellation mark");
    assert!(cancellation.changed);

    let fenced = session
        .checkpoint_task(task.task_id, execute.generation, Some(json!("late")), None)
        .expect_err("normal invocation must be fenced");
    assert!(matches!(fenced, SessionError::CancellationFence(_)));

    let abort = session
        .reserve_task_invocation(task.task_id, InvocationKind::Abort)
        .expect("abort reservation");
    assert_eq!(abort.generation, execute.generation + 1);
    session
        .settle_task_with(
            task.task_id,
            abort.generation,
            TaskOutcome {
                kind: TaskOutcomeKind::Aborted,
                value: json!("cancelled"),
            },
            None,
            |_| Ok(()),
        )
        .expect("abort settlement");

    let no_change = session
        .mark_task_cancellation(task.task_id)
        .expect("terminal cancellation is no-op");
    assert!(!no_change.changed);
}

#[test]
fn dependency_readiness_requires_terminal_predecessors() {
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let first = session
        .create_task(task_request(root, Vec::new()))
        .expect("first");
    let second = session
        .create_task(task_request(root, vec![first.task_id]))
        .expect("second");

    let blocked = session
        .reserve_task_invocation(second.task_id, InvocationKind::Execute)
        .expect_err("dependency must block");
    assert!(matches!(blocked, SessionError::DependenciesNotReady(_)));

    let first_run = session
        .reserve_task_invocation(first.task_id, InvocationKind::Execute)
        .expect("first reservation");
    session
        .settle_task_with(
            first.task_id,
            first_run.generation,
            completed("first"),
            None,
            |_| Ok(()),
        )
        .expect("first settlement");
    session
        .reserve_task_invocation(second.task_id, InvocationKind::Execute)
        .expect("second reservation");
}

#[test]
fn placement_is_committed_with_the_turn_that_answers() {
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let receipt = session
        .admit_input(
            input_request(root, InputMode::Submit),
            Some(task_request(root, Vec::new())),
        )
        .expect("admission");
    let (input, turn) = (
        receipt.input_id,
        receipt
            .task_id
            .expect("a submitting admission starts a turn"),
    );

    // Placement is part of the admission commit, not of the answer: the entry
    // exists and the input names it before any invocation ran.
    let snapshot = session.snapshot();
    let entry = snapshot
        .entries
        .iter()
        .find(|entry| entry.id == placement_of(&snapshot, input).entry)
        .expect("placed entry");
    assert_eq!(entry.kind.as_str(), ion_core_entry_kind());
    assert_eq!(entry.data, json!({"text": "hello"}));
    assert_eq!(placement_of(&snapshot, input).turn, turn);

    // A retry must name the attempt it replaces, and the replaced attempt must
    // have closed its turn, so two answer attempts cannot overlap.
    let stale = session
        .retry_input(
            input,
            turn,
            task_request(session.root_conversation(), Vec::new()),
        )
        .expect_err("an open turn cannot be retried");
    assert!(matches!(stale, SessionError::TurnStillOpen(root) if root == turn));
    let other = session
        .create_task(task_request(root, Vec::new()))
        .expect("unrelated task");
    let mismatch = session
        .retry_input(input, other.task_id, task_request(root, Vec::new()))
        .expect_err("a retry names the attempt it replaces");
    assert!(matches!(mismatch, SessionError::StaleInputBinding { .. }));
    assert!(matches!(
        session
            .abandon_input(input, other.task_id)
            .expect_err("abandonment names the attempt it ends"),
        SessionError::StaleInputBinding { .. }
    ));
}

fn placement_of(snapshot: &crate::SessionSnapshot, input: crate::InputId) -> crate::InputPlacement {
    snapshot
        .inputs
        .iter()
        .find(|candidate| candidate.id == input)
        .expect("admitted input")
        .disposition
        .placement()
        .expect("a placed input")
}

fn ion_core_entry_kind() -> &'static str {
    crate::conversation::INPUT_ENTRY
}

#[test]
fn submission_rejects_an_input_aimed_at_another_conversation() {
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let other = session
        .create_conversation(ConversationSpec::independent())
        .expect("conversation")
        .conversation_id;
    let before = session.snapshot();

    let error = session
        .admit_input(
            InputRequest {
                target: other,
                sender: InputSender::User,
                mode: InputMode::Submit,
                request_key: None,
                body: InputBody::Text("elsewhere".to_owned()),
            },
            Some(task_request(root, Vec::new())),
        )
        .expect_err("a mismatched target must be rejected");
    assert!(matches!(error, SessionError::InputTargetMismatch { .. }));
    assert_eq!(session.snapshot(), before, "nothing was admitted");
}

#[test]
fn failed_terminal_plan_rolls_back_successors_and_sequence_values() {
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let task = session
        .create_task(task_request(root, Vec::new()))
        .expect("task");
    let run = session
        .reserve_task_invocation(task.task_id, InvocationKind::Execute)
        .expect("reservation");
    let before = session.snapshot();

    let error = session
        .settle_task_with(
            task.task_id,
            run.generation,
            completed("unused"),
            None,
            |transaction| {
                let _ = transaction.create_task(task_request(root, Vec::new()))?;
                Err::<(), _>(SessionError::Invariant("reject terminal plan".to_owned()))
            },
        )
        .expect_err("terminal plan must roll back");
    assert!(matches!(error, SessionError::Invariant(_)));
    assert_eq!(session.snapshot(), before);
}
