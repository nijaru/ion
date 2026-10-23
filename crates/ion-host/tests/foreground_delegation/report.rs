use super::*;

#[tokio::test]
async fn completion_only_incomplete_text_is_a_partial_report_not_an_answer() {
    let child = Arc::new(ScriptedModelService::new([Script::Stream(vec![
        ModelStreamEvent::Completed(ModelResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![
                    Content::Thinking("do not disclose".into()),
                    Content::Text("first visible block".into()),
                    Content::Text("second 🚀".into()),
                ],
                provider_replay: None,
            },
            usage: Usage::unknown(),
            termination: ResponseTermination::Incomplete(ion_ai::IncompleteReason::ContentFilter),
            returned_model: None,
        }),
    ])]));
    let mut f = Fixture::with_child_service(
        Wait::Join,
        false,
        false,
        None,
        None,
        Some(60_000),
        Some(child.clone()),
    );
    f.start();
    assert!(f.settle().await.cleanup_errors.is_empty());
    let outcome = f.session.view().unwrap().delegations()[0]
        .outcome
        .clone()
        .unwrap();
    let DelegationOutcome::Joined {
        execution_timed_out: false,
        elapsed_ms: Some(_),
        answer: None,
        partial_output: Some(output),
        error: Some(_),
        receipt: Some(receipt),
    } = outcome
    else {
        panic!("failed joined report");
    };
    assert!(matches!(receipt.end, Some(TurnEndReason::Failed(_))));
    assert_eq!(output.text, "first visible block\nsecond 🚀");
    assert!(!output.truncated && !output.observation_gap);
    assert_eq!(child.requests().len(), 1);
    assert_eq!(f.effect.calls.load(Ordering::SeqCst), 0);
    f.control.shutdown().await;
    let root = f.root.clone();
    drop(f);
    std::fs::remove_dir_all(root).unwrap();
}
