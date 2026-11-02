use super::*;

struct PendingModel(tokio::sync::Notify);
impl ion_ai::ModelService for PendingModel {
    fn stream<'a>(
        &'a self,
        _: ion_ai::ModelRequest,
    ) -> ion_ai::BoxFuture<'a, Result<ion_ai::ModelStream, ion_ai::ProviderError>> {
        Box::pin(async move {
            self.0.notify_one();
            std::future::pending().await
        })
    }
}
struct RequestApplication(Arc<PendingModel>);
impl ion_app::Application for RequestApplication {
    fn resources(&self, _: Option<&std::path::Path>) -> Result<ion_app::Resources> {
        Ok(ion_app::Resources::new(
            "Credential-free request fixture".into(),
        ))
    }
    fn runtime(
        &self,
        _: Option<&std::path::Path>,
        model: Option<&ion_ai::ModelRef>,
        _: Option<Arc<ion_app::QuestionCustody>>,
    ) -> Result<ion_app::ApplicationRuntime> {
        Ok(ion_app::ApplicationRuntime::new(Arc::new(
            ion_core::Agent::with_tool_set(
                self.0.clone(),
                Arc::new(ion_core::ToolSet::new([])),
                model.expect("fixture has a selected model").clone(),
            ),
        )))
    }
}

#[tokio::test]
async fn keyed_submission_reconciles_before_busy_and_only_acknowledges_once() {
    let model = Arc::new(PendingModel(tokio::sync::Notify::new()));
    let mut fixture = Fixture::with_application(Some(Arc::new(RequestApplication(model.clone()))));
    let mut control = fixture.take();
    let key = json!({"scope":"rpc-test","id":"delivery"});
    let request = json!({"type":"submit_request","id":"first","key":key,"message":"/literal $1"});
    let first = invoke(&mut control, request.clone()).await.unwrap();
    assert_eq!(first["success"], true);
    assert_eq!(first["data"]["disposition"], "started");
    let receipt = first["data"]["receipt"].clone();
    assert_eq!(receipt["turn"], 1);
    assert_eq!(receipt["key"], key);
    assert!(receipt["end"].is_null());
    assert!(control.host.is_busy());
    tokio::time::timeout(Duration::from_secs(5), model.0.notified())
        .await
        .unwrap();
    let entries = control.host.session().view().unwrap().entries;
    assert!(entries.iter().any(|entry| matches!(entry,
        ion_core::SessionEntry::TurnStarted { input, .. }
            if input == &ion_ai::Message::user_input("/literal $1".into(), [])
    )));

    let mut duplicate = request.clone();
    duplicate["id"] = json!("redelivery");
    let repeated = invoke(&mut control, duplicate).await.unwrap();
    assert_eq!(repeated["id"], "redelivery");
    assert_eq!(
        repeated["data"],
        json!({"disposition":"existing","receipt":receipt})
    );
    assert_eq!(control.correlations.len(), 1);
    let mut conflict = request.clone();
    conflict["message"] = json!("different input");
    assert_eq!(
        invoke(&mut control, conflict).await.unwrap()["success"],
        false
    );
    let mut other = request.clone();
    other["key"]["id"] = json!("new-work-while-busy");
    assert_eq!(invoke(&mut control, other).await.unwrap()["success"], false);
    let lookup = invoke(&mut control, json!({"type":"get_request","key":key}))
        .await
        .unwrap();
    assert_eq!(lookup["data"]["receipt"], receipt);
    assert_eq!(control.host.session().view().unwrap().entries, entries);

    // Stop while provider construction waits. Its known durable Turn still
    // produces turn_end, not an uncommitted input or duplicate acknowledgement.
    let report = control.host.shutdown().await;
    assert!(report.cleanup_errors.is_empty());
    let mut bytes = Vec::new();
    let mut output = Output::new(&mut bytes);
    for update in report.updates {
        control.update(update, &mut output).unwrap();
    }
    output.finish().await.unwrap();
    drop(output);
    let records = records(&bytes);
    assert!(
        records
            .iter()
            .any(|r| r["type"] == "turn_end" && r["turn"] == 1 && r["status"] == "cancelled")
    );
    assert!(!records.iter().any(|r| r["command"] == "submit_request"
        || r["type"] == "follow_up_started"
        || r["type"] == "operation_failed"));
    assert!(control.correlations.is_empty());
    let settled = control.host.session().view().unwrap().entries;
    let known = invoke(&mut control, request).await.unwrap();
    assert_eq!(known["data"]["disposition"], "existing");
    assert_eq!(known["data"]["receipt"]["session"], receipt["session"]);
    assert_eq!(known["data"]["receipt"]["turn"], 1);
    assert!(!known["data"]["receipt"]["end"].is_null());
    assert_eq!(control.host.session().view().unwrap().entries, settled);
}

#[test]
fn abort_remains_live_during_lookup_and_a_late_plan_cannot_start_input() {
    tokio::runtime::Builder::new_current_thread().enable_all().max_blocking_threads(1).build().unwrap().block_on(async {
        use tokio::io::AsyncBufReadExt;
        let mut fixture = Fixture::new();
        let control = fixture.take();
        let session = control.host.session().clone();
        let before = session.view().unwrap().entries;
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            entered.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(5)).is_ok()
        });
        ready.await.unwrap();
        let (input, mut commands) = tokio::io::duplex(4096);
        let (writer, reader) = tokio::io::duplex(65536);
        let running = tokio::spawn(connection::connection(control,
            CommandReader::new(BufReader::new(input)), Output::new(writer), std::future::pending()));
        commands.write_all(format!("{}\n{}\n",
            json!({"type":"submit_request","id":"pending","key":{"scope":"rpc","id":"stop-before-admission"},"message":"must not run"}),
            json!({"type":"abort","id":"stop"})).as_bytes()).await.unwrap();
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line)).await.unwrap().unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], "stop");
        assert_eq!(reply["success"], true);
        assert!(!blocker.is_finished(), "abort reply must not wait for the storage leaf");
        release.send(()).unwrap();
        assert!(blocker.await.unwrap());
        commands.shutdown().await.unwrap();
        let mut rest = Vec::new();
        reader.read_to_end(&mut rest).await.unwrap();
        running.await.unwrap().unwrap();
        let replies = records(&rest);
        assert_eq!(replies.iter().filter(|reply| reply["id"] == "pending").count(), 1);
        assert!(replies.iter().any(|reply| reply["id"] == "pending" && reply["success"] == false));
        assert_eq!(session.view().unwrap().entries, before);
        assert!(session.request_receipt(&ion_core::RequestKey::new("rpc".into(), "stop-before-admission".into()).unwrap()).unwrap().is_none());
    });
}

#[tokio::test]
async fn literal_request_validation_and_unknown_lookup_have_no_effects() {
    let mut fixture = Fixture::new();
    let mut control = fixture.take();
    let entries = control.host.session().view().unwrap().entries;
    let valid = json!({"type":"submit_request","key":{"scope":"rpc-test","id":"delivery"},"message":"text"});
    let mut invalid = Vec::new();
    for field in ["key", "message"] {
        let mut value = valid.clone();
        value.as_object_mut().unwrap().remove(field);
        invalid.push(value);
    }
    for key in [
        json!({"scope":"","id":"delivery"}),
        json!({"scope":"rpc-test","id":"\n"}),
        json!({"scope":"rpc-test","id":"delivery","extra":1}),
    ] {
        let mut value = valid.clone();
        value["key"] = key;
        invalid.push(value);
    }
    for message in [
        json!(" \n"),
        json!({"role":"Assistant","content":[{"Text":"text"}]}),
    ] {
        let mut value = valid.clone();
        value["message"] = message;
        invalid.push(value);
    }
    // Do not silently accept ignored image paths or a second input shape.
    for field in ["images", "input"] {
        let mut value = valid.clone();
        value[field] = json!(["private.png"]);
        invalid.push(value);
    }
    for value in invalid {
        assert_eq!(invoke(&mut control, value).await.unwrap()["success"], false);
    }
    let unknown = invoke(
        &mut control,
        json!({"type":"get_request","key":{"scope":"rpc-test","id":"missing"}}),
    )
    .await
    .unwrap();
    assert_eq!(unknown["data"], json!({"receipt":null}));
    assert!(!control.host.is_busy());
    assert_eq!(control.host.session().view().unwrap().entries, entries);
    assert!(control.correlations.is_empty());
    let report = control.host.shutdown().await;
    assert!(report.cleanup_errors.is_empty());
}
