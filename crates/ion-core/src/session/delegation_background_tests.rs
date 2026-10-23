use super::*;
use crate::{DelegationOutcome, DelegationOwnership};

#[test]
fn transferred_children_reconcile_without_a_live_turn_and_copies_never_acquire_them() {
    for entry in ["new", "shell", "resume", "active_new"] {
        let (root, session, binding, record) = super::delegation_tests::pending_for(
            false,
            DelegationOwnership::Background {
                owner: uuid::Uuid::new_v4(),
            },
        );
        let lease = Arc::new(());
        session.transfer_delegation(&record, &lease).unwrap();
        if matches!(entry, "new" | "shell") {
            session
                .record_tool_result(
                    1,
                    ToolResult {
                        call_id: "call".into(),
                        name: "delegate".into(),
                        result: serde_json::Value::Null,
                        images: vec![],
                        is_error: false,
                    },
                )
                .unwrap();
            session
                .end_turn(1, TurnEndReason::Completed, &BTreeSet::new())
                .unwrap();
            for copy in [
                session.clone_to(root.join("clone.sqlite")).unwrap(),
                session
                    .fork_to(root.join("fork.sqlite"), ForkPoint::AfterTurn(1))
                    .unwrap(),
            ] {
                let before = copy.view().unwrap().entries.len();
                copy.begin_turn("new copy work".into(), record.spec.model.clone())
                    .unwrap();
                let view = copy.view().unwrap();
                assert!(view.delegations().is_empty());
                assert!(!view.entries[before..].iter().any(|entry| matches!(entry,
                    SessionEntry::DelegationSettled { run, .. } if *run == record.run)));
            }
        }
        let point = session.recovery_state().unwrap().map(|state| state.point);
        drop(lease);
        let before = session.view().unwrap();
        assert!(before.delegations()[0].transferred);
        assert!(
            before.delegations()[0].outcome.is_none(),
            "inspection is passive"
        );
        let reject = match entry {
            "shell" => "user_shell_admitted",
            "resume" => "tool_result",
            _ => "turn_started",
        };
        session.store.lock().unwrap().connection.execute_batch(&format!(
            "CREATE TRIGGER reject_admission BEFORE INSERT ON entries WHEN json_extract(NEW.body,'$.kind') = '{reject}' BEGIN SELECT RAISE(ABORT, 'injected'); END;"
        )).unwrap();
        let admit = || match entry {
            "shell" => session.admit_user_shell("true".into(), false).map(|_| ()),
            "resume" => session.resume_turn(point.unwrap(), &binding).map(|_| ()),
            _ => session
                .begin_turn("new work".into(), record.spec.model.clone())
                .map(|_| ()),
        };
        assert!(admit().is_err());
        assert_eq!(
            session.view().unwrap().entries,
            before.entries,
            "orphan closure and admission roll back together"
        );
        session
            .store
            .lock()
            .unwrap()
            .connection
            .execute_batch("DROP TRIGGER reject_admission;")
            .unwrap();
        admit().unwrap();
        let after = session.view().unwrap();
        assert_eq!(
            after.delegations()[0].outcome,
            Some(DelegationOutcome::Unknown)
        );
        let suffix = &after.entries[before.entries.len()..];
        let unknown = suffix.iter().position(|entry| matches!(entry, SessionEntry::DelegationSettled { run, outcome: DelegationOutcome::Unknown } if *run == record.run)).unwrap();
        let dependent = suffix
            .iter()
            .position(|entry| {
                matches!(
                    entry,
                    SessionEntry::TurnStarted { .. }
                        | SessionEntry::UserShellAdmitted { .. }
                        | SessionEntry::ToolResult { .. }
                )
            })
            .unwrap();
        assert!(unknown < dependent);
        if entry == "resume" {
            assert!(matches!(suffix[0], SessionEntry::TurnResumed { .. }));
        }
        assert!(
            !root.join("children").exists(),
            "recovery never materializes or replays the old child"
        );
        let path = session.path().to_path_buf();
        drop(session);
        let reopened = Session::open(path).unwrap();
        assert_eq!(
            reopened.view().unwrap().delegations()[0].outcome,
            Some(DelegationOutcome::Unknown)
        );
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }
}
