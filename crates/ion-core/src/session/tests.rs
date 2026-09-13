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

    let (successor, _) = session
        .settle_task_with(
            task.task_id,
            recover.generation,
            completed("done"),
            None,
            |transaction| transaction.create_task(task_request(root, Vec::new())),
        )
        .expect("terminal plan");
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
fn input_disposition_tracks_assignment_then_consumption() {
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let input = session
        .admit_input(InputRequest {
            target: root,
            sender: InputSender::User,
            mode: InputMode::Submit,
            request_key: None,
            body: InputBody::Text("hello".to_owned()),
        })
        .expect("input");
    let task = session
        .create_task(task_request(root, Vec::new()))
        .expect("task");
    session
        .set_input_disposition(input.input_id, InputDisposition::Assigned(task.task_id))
        .expect("assignment");

    let entry = session
        .append_entry(EntryRequest {
            conversation_id: root,
            kind: crate::EntryKind::new("user").expect("entry kind"),
            data: json!({"text": "hello"}),
            projection: Vec::new(),
            context: crate::conversation::context::ContextControl::none(),
        })
        .expect("entry");
    session
        .set_input_disposition(input.input_id, InputDisposition::Consumed(entry.entry_id))
        .expect("consumption");
    let before = session.snapshot().last_commit;
    let invalid = session
        .set_input_disposition(input.input_id, InputDisposition::Cancelled)
        .expect_err("consumed input is terminal");
    assert!(matches!(invalid, SessionError::InvalidInputDisposition(_)));
    assert_eq!(session.snapshot().last_commit, before);
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
        .submit_input(
            InputRequest {
                target: other,
                sender: InputSender::User,
                mode: InputMode::Submit,
                request_key: None,
                body: InputBody::Text("elsewhere".to_owned()),
            },
            task_request(root, Vec::new()),
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
