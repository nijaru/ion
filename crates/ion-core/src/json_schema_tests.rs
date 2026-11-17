use super::*;
use serde_json::json;

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
    assert!(
        CompiledSchema::compile(&reference_dag(16)).is_err(),
        "compact schema admitted exponential validation work"
    );
    let small = CompiledSchema::compile(&reference_dag(3)).unwrap();
    assert!(small.validate(&json!(7)).is_ok());
    assert!(small.validate(&json!("7")).is_err());
    let recursive = json!({
        "$defs": {"node": {"type": ["object", "null"], "properties": {"next": {"$ref": "#/$defs/node"}}}},
        "$ref": "#/$defs/node"
    });
    assert!(CompiledSchema::compile(&recursive).is_err());
}

#[test]
fn non_linear_patterns_are_unavailable_not_failed_data() {
    assert!(
        CompiledSchema::compile(&json!({"type": "string", "pattern": "^(a+)+(?=b)$"})).is_err()
    );
    let simple = CompiledSchema::compile(&json!({"pattern": "^a+$"})).unwrap();
    assert!(matches!(
        simple.validate(&json!("a".repeat(512) + "!")),
        Err(ValidationFailure::Invalid { .. })
    ));
}

#[test]
fn engine_annotation_revalidation_is_unavailable_before_evaluation() {
    let mut source = json!({});
    for _ in 0..6 {
        source = json!({"anyOf": [source], "unevaluatedProperties": false});
    }
    // Admission alone must reject this family; never run exponential probes.
    assert!(CompiledSchema::compile(&source).is_err());
    assert!(CompiledSchema::compile(&json!({"unevaluatedItems": false})).is_err());
}

#[test]
fn engine_pattern_semantics_must_match_ecmascript() {
    // ECMAScript ^.$ rejects CR; ^a\b matches before non-word é. A successful
    // Rust regex compilation does not establish these semantics.
    for source in [
        json!({"pattern": "^.$"}),
        json!({"pattern": "^a\\b"}),
        json!({"patternProperties": {"^.$": {"type": "integer"}}}),
        json!({"pattern": "[a--b]"}),
    ] {
        assert!(CompiledSchema::compile(&source).is_err());
    }
    let ascii = CompiledSchema::compile(&json!({"pattern": "^[A-Za-z0-9_-]{3,8}$"})).unwrap();
    for (value, valid) in [("Ion_1", true), ("ééé", false), ("Io\n", false)] {
        assert_eq!(ascii.validate(&json!(value)).is_ok(), valid);
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
        json!({"$ref": format!("file://{}", local.display())}),
        json!({"$schema": format!("http://{}/SCHEMA_CANARY", listener.local_addr().unwrap())}),
        json!({"type": "SCHEMA_CANARY"}),
        json!({"enum": vec![Value::Null; MAX_SCHEMA_NODES + 1]}),
        json!({"description": "x".repeat(MAX_SCHEMA_TEXT_BYTES + 1)}),
        json!({"$ref":"#/$defs/missing"}),
        json!({"$id":"SCHEMA_CANARY"}),
        json!({"$anchor":"SCHEMA_CANARY"}),
        json!({"$dynamicRef":"#SCHEMA_CANARY"}),
        json!({"$recursiveRef":"#"}),
    ] {
        let error = CompiledSchema::compile(&schema)
            .err()
            .expect("schema should be unavailable");
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
    assert!(CompiledSchema::compile(&deep).is_err());
    let legacy = CompiledSchema::compile(
        &json!({"$schema": "http://json-schema.org/draft-07/schema#", "const": "SCHEMA_CANARY"}),
    )
    .unwrap();
    let validation = legacy.validate(&json!("VALUE_CANARY")).unwrap_err();
    let rendered = validation.to_string();
    assert!(!rendered.contains("SCHEMA_CANARY") && !rendered.contains("VALUE_CANARY"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn strict_checks_preserve_input_and_bound_issue_paths() {
    let schema = CompiledSchema::compile(&json!({
        "type":"object", "additionalProperties":false, "required":["count"],
        "properties":{"count":{"type":"integer"}, "label":{"default":"not inserted"}}
    }))
    .unwrap();
    for (value, valid) in [
        (json!({"count":1}), true),
        (json!({"count":"1"}), false),
        (json!({}), false),
        (json!({"count":1,"extra":true}), false),
    ] {
        let before = value.clone();
        assert_eq!(schema.validate(&value).is_ok(), valid);
        assert_eq!(value, before);
    }
    let name = "x".repeat(MAX_PATH_BYTES + 1);
    let schema =
        CompiledSchema::compile(&json!({"properties": {&name: {"type":"integer"}}})).unwrap();
    let error = schema.validate(&json!({&name:"VALUE_CANARY"})).unwrap_err();
    let ValidationFailure::Invalid { issue } = error else {
        panic!("expected mismatch")
    };
    assert_eq!(issue.instance_path, None);
    assert_eq!(issue.schema_path, None);
    assert_eq!(issue.keyword, "type");
}
