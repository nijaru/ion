use super::*;

#[tokio::test]
async fn keyed_submission_reconciles_before_busy_and_only_acknowledges_once() {
    let mut fixture = Fixture::new();
    let mut control = fixture.take();
    let key = json!({"scope":"rpc-test","id":"delivery"});
    let request = json!({"type":"submit_request","id":"first","key":key,"message":"/literal $1"});
    let first = invoke(&mut control, request.clone()).unwrap();
    assert_eq!(first["success"], true);
    assert_eq!(first["data"]["disposition"], "started");
    let receipt = first["data"]["receipt"].clone();
    assert_eq!(receipt["turn"], 1);
    assert_eq!(receipt["key"], key);
    assert!(receipt["end"].is_null());
    assert!(control.host.is_busy());
    let entries = control.host.session().view().unwrap().entries;
    assert!(entries.iter().any(|entry| matches!(entry,
        ion_core::SessionEntry::TurnStarted { input, .. }
            if input == &ion_ai::Message::user_input("/literal $1".into(), [])
    )));

    let mut duplicate = request.clone();
    duplicate["id"] = json!("redelivery");
    let repeated = invoke(&mut control, duplicate).unwrap();
    assert_eq!(repeated["id"], "redelivery");
    assert_eq!(
        repeated["data"],
        json!({"disposition":"existing","receipt":receipt})
    );
    assert_eq!(control.correlations.len(), 1);
    let mut conflict = request.clone();
    conflict["message"] = json!("different input");
    assert_eq!(invoke(&mut control, conflict).unwrap()["success"], false);
    let mut other = request.clone();
    other["key"]["id"] = json!("new-work-while-busy");
    assert_eq!(invoke(&mut control, other).unwrap()["success"], false);
    let lookup = invoke(&mut control, json!({"type":"get_request","key":key})).unwrap();
    assert_eq!(lookup["data"]["receipt"], receipt);
    assert_eq!(control.host.session().view().unwrap().entries, entries);

    // Stop before the submitted task's first poll. Its known durable Turn still
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
    let known = invoke(&mut control, request).unwrap();
    assert_eq!(known["data"]["disposition"], "existing");
    assert_eq!(known["data"]["receipt"]["session"], receipt["session"]);
    assert_eq!(known["data"]["receipt"]["turn"], 1);
    assert!(!known["data"]["receipt"]["end"].is_null());
    assert_eq!(control.host.session().view().unwrap().entries, settled);
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
        assert_eq!(invoke(&mut control, value).unwrap()["success"], false);
    }
    let unknown = invoke(
        &mut control,
        json!({"type":"get_request","key":{"scope":"rpc-test","id":"missing"}}),
    )
    .unwrap();
    assert_eq!(unknown["data"], json!({"receipt":null}));
    assert!(!control.host.is_busy());
    assert_eq!(control.host.session().view().unwrap().entries, entries);
    assert!(control.correlations.is_empty());
    let report = control.host.shutdown().await;
    assert!(report.cleanup_errors.is_empty());
}
