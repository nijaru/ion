//! Registry tests.

use super::support::*;

// ---- Tool registry unit tests ----

#[tokio::test]
async fn registry_exposes_core_tool_specs() {
    let registry = ToolRegistry::default();
    let specs = registry.specs();
    let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"read"));
    assert!(names.contains(&"write"));
    assert!(names.contains(&"edit"));
    assert!(names.contains(&"bash"));
    assert!(names.contains(&"search"));
    assert!(names.contains(&"find"));
    assert!(!names.contains(&"compact"));
}

#[tokio::test]
async fn registry_rejects_missing_required_args() {
    let registry = ToolRegistry::default();
    let err = registry
        .validate("read", &json!({}))
        .expect_err("missing path should fail");
    assert!(err.contains("path"), "got: {err}");

    assert!(
        registry.validate("read", &json!({"path": 5})).is_ok(),
        "present key is structurally valid"
    );
}

#[tokio::test]
async fn registry_rejects_unknown_tool() {
    let registry = ToolRegistry::default();
    let cancel = tokio_util::sync::CancellationToken::new();
    let outcome = registry.execute("nope", &json!({}), cancel).await;
    assert!(outcome.is_error);
    assert!(outcome.output.contains("unknown tool"));
}

#[test]
fn tool_result_classifies_ok_and_err() {
    let ok = ToolResult::Ok {
        call_id: 1,
        output: "hi".into(),
        artifact: None,
    };
    let err = ToolResult::Err {
        call_id: 2,
        error: "boom".into(),
        artifact: None,
    };
    assert_eq!(ok.call_id(), 1);
    assert!(ok.is_ok());
    assert_eq!(err.call_id(), 2);
    assert!(!err.is_ok());
    assert_eq!(
        ToolResult::Ok {
            call_id: 3,
            output: "x".into(),
            artifact: None,
        }
        .model_text(),
        "x"
    );
    assert_eq!(err.model_text(), "boom");
}

#[test]
fn tool_spec_is_clonable_and_comparable() {
    let a = spec("read");
    let b = a.clone();
    assert_eq!(a, b);
}
