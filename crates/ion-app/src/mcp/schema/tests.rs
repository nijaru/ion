use super::*;
use serde_json::json;

#[test]
fn conformance_preserves_observations_and_distinguishes_missing_null_and_errors() {
    let source = json!({
        "$defs": { "row": { "type": "integer", "minimum": 0 } },
        "type": "array", "items": { "$ref": "#/$defs/row" }
    });
    let compiled = OutputSchema::capture(source.clone());
    assert!(compiled.unchecked_reason().is_none());
    assert_eq!(
        compiled.hint(),
        ToolResultSchema {
            value_path: vec!["structured_content".into()],
            schema: source
        }
    );
    for (wire, expected) in [
        (json!({"content": [], "structuredContent": [0, 2]}), "valid"),
        (
            json!({"content": [], "structuredContent": [0, "VALUE_CANARY"]}),
            "invalid",
        ),
        (
            json!({"content": [], "isError": true}),
            "skipped_remote_error",
        ),
    ] {
        let result: CallToolResult = serde_json::from_value(wire).unwrap();
        let before = result.clone();
        let status = serde_json::to_value(compiled.validate(&result)).unwrap();
        assert_eq!(status["status"], expected);
        assert!(!status.to_string().contains("VALUE_CANARY"));
        assert_eq!(result, before, "validation rewrote the observed response");
    }
    let null_schema = OutputSchema::capture(json!({"type": "null"}));
    for (wire, expected) in [
        (json!({"structuredContent": null}), "valid"),
        (json!({"content": []}), "invalid"),
    ] {
        let result = serde_json::from_value(wire).unwrap();
        assert_eq!(
            serde_json::to_value(null_schema.validate(&result)).unwrap()["status"],
            expected
        );
    }
    for (kind, value) in [
        ("boolean", json!(false)),
        ("integer", json!(0)),
        ("string", json!("")),
    ] {
        let schema = OutputSchema::capture(json!({"type": kind}));
        let result = serde_json::from_value(json!({"structuredContent": value})).unwrap();
        assert_eq!(
            serde_json::to_value(schema.validate(&result)).unwrap()["status"],
            "valid"
        );
    }
}

#[test]
fn unchecked_contract_keeps_hint_and_raw_evidence_but_remote_error_takes_precedence() {
    let source = json!({"$ref":"https://example.invalid/SCHEMA_CANARY"});
    let captured = OutputSchema::capture(source.clone());
    assert!(captured.unchecked_reason().is_some());
    assert_eq!(captured.hint().schema, source);
    for (is_error, expected, raw) in [
        (false, "not_checked", true),
        (true, "skipped_remote_error", false),
    ] {
        let result: CallToolResult =
            serde_json::from_value(json!({"structuredContent":"VALUE_CANARY", "isError":is_error}))
                .unwrap();
        let before = result.clone();
        let status = captured.validate(&result);
        assert_eq!(status.needs_raw_result(), raw);
        let rendered = serde_json::to_value(status).unwrap();
        assert_eq!(rendered["status"], expected);
        assert!(!rendered.to_string().contains("CANARY"));
        assert_eq!(result, before);
    }
}
