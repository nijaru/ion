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

fn reference_dag(levels: usize) -> Value {
    let mut definitions = serde_json::Map::new();
    definitions.insert("n0".into(), json!({"type": "integer"}));
    for n in 1..=levels {
        let reference = format!("#/$defs/n{}", n - 1);
        definitions.insert(
            format!("n{n}"),
            json!({"allOf": [{"$ref": reference}, {"$ref": reference}]}),
        );
    }
    json!({"$defs": definitions, "$ref": format!("#/$defs/n{levels}")})
}

#[test]
fn reference_expansion_must_fit_the_validation_budget() {
    let source = reference_dag(16);
    let captured = OutputSchema::capture(source.clone());
    assert!(
        captured.unchecked_reason().is_some(),
        "compact schema admitted exponential validation work"
    );
    assert_eq!(
        captured.hint().schema,
        source,
        "unchecked status discarded the advertised hint"
    );
    let result: CallToolResult =
        serde_json::from_value(json!({"structuredContent": 7, "isError": false})).unwrap();
    let before = result.clone();
    assert_eq!(
        serde_json::to_value(captured.validate(&result)).unwrap()["status"],
        "not_checked"
    );
    assert_eq!(result, before);
    let small = OutputSchema::capture(reference_dag(3));
    assert!(small.unchecked_reason().is_none());
    assert_eq!(
        serde_json::to_value(small.validate(&result)).unwrap()["status"],
        "valid"
    );
    let recursive = OutputSchema::capture(json!({
        "$defs": {"node": {"type": ["object", "null"], "properties": {"next": {"$ref": "#/$defs/node"}}}},
        "$ref": "#/$defs/node"
    }));
    assert!(recursive.unchecked_reason().is_some());
}

#[test]
fn non_linear_patterns_are_unchecked_not_failed_data() {
    let result =
        serde_json::from_value(json!({"structuredContent": "a".repeat(512) + "!"})).unwrap();
    let schema = OutputSchema::capture(json!({"type": "string", "pattern": "^(a+)+(?=b)$"}));
    assert!(schema.unchecked_reason().is_some());
    let status = schema.validate(&result);
    assert!(status.needs_raw_result());
    assert_eq!(
        serde_json::to_value(status).unwrap()["status"],
        "not_checked"
    );
    let simple = OutputSchema::capture(json!({"pattern": "^a+$"}));
    assert!(simple.unchecked_reason().is_none());
    assert_eq!(
        serde_json::to_value(simple.validate(&result)).unwrap()["status"],
        "invalid"
    );
}

#[test]
fn engine_annotation_revalidation_is_unchecked_before_evaluation() {
    let mut source = json!({});
    for _ in 0..6 {
        source = json!({"anyOf": [source], "unevaluatedProperties": false});
    }
    // Admission alone must reject this family. Do not run a deep exponential
    // instance probe merely to demonstrate the engine's repeated marking work.
    assert!(OutputSchema::capture(source).unchecked_reason().is_some());
    assert!(
        OutputSchema::capture(json!({"unevaluatedItems": false}))
            .unchecked_reason()
            .is_some()
    );
}

#[test]
fn engine_pattern_semantics_must_match_ecmascript() {
    // ECMAScript ^.$ rejects CR; ^a\\b matches before non-word é. A successful
    // Rust regex compilation does not establish these semantics.
    for source in [
        json!({"pattern": "^.$"}),
        json!({"pattern": "^a\\b"}),
        json!({"patternProperties": {"^.$": {"type": "integer"}}}),
        json!({"pattern": "[a--b]"}),
    ] {
        assert!(OutputSchema::capture(source).unchecked_reason().is_some());
    }
    let ascii = OutputSchema::capture(json!({"pattern": "^[A-Za-z0-9_-]{3,8}$"}));
    assert!(ascii.unchecked_reason().is_none());
    for (value, expected) in [("Ion_1", "valid"), ("ééé", "invalid"), ("Io\n", "invalid")] {
        let result = serde_json::from_value(json!({"structuredContent": value})).unwrap();
        assert_eq!(
            serde_json::to_value(ascii.validate(&result)).unwrap()["status"],
            expected
        );
    }
}

#[test]
fn compilation_is_offline_bounded_and_diagnostics_do_not_quote_values() {
    use std::io::ErrorKind;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let remote = format!("http://{}/SCHEMA_CANARY", listener.local_addr().unwrap());
    let root = std::env::temp_dir().join(format!("ion-schema-offline-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&root).unwrap();
    let local = root.join("SCHEMA_CANARY.json");
    std::fs::write(&local, r#"{"type":"integer"}"#).unwrap();
    for schema in [
        json!({"$ref": remote}),
        json!({"$ref": url::Url::from_file_path(&local).unwrap().to_string()}),
        json!({"$schema": "https://example.invalid/SCHEMA_CANARY"}),
        json!({"type": "SCHEMA_CANARY"}),
        json!({"enum": vec![Value::Null; MAX_SCHEMA_NODES + 1]}),
        json!({"description": "x".repeat(MAX_SCHEMA_TEXT_BYTES + 1)}),
    ] {
        let captured = OutputSchema::capture(schema);
        let error = captured
            .unchecked_reason()
            .expect("schema should be unchecked");
        assert!(!error.contains("SCHEMA_CANARY"), "{error}");
    }
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == ErrorKind::WouldBlock),
        "schema compilation contacted the reference server"
    );
    let mut deep = json!({"type": "object"});
    for _ in 0..=MAX_SCHEMA_DEPTH {
        deep = json!({"properties": {"nested": deep}});
    }
    assert!(OutputSchema::capture(deep).unchecked_reason().is_some());
    let legacy = OutputSchema::capture(
        json!({"$schema": "http://json-schema.org/draft-07/schema#", "const": "SCHEMA_CANARY"}),
    );
    let result = serde_json::from_value(json!({"structuredContent": "VALUE_CANARY"})).unwrap();
    let validation = legacy.validate(&result);
    assert!(validation.needs_raw_result());
    let rendered = serde_json::to_string(&validation).unwrap();
    assert!(!rendered.contains("SCHEMA_CANARY") && !rendered.contains("VALUE_CANARY"));
    std::fs::remove_dir_all(root).unwrap();
}
