use super::*;
use serde_json::json;

fn declaration(schema: Value) -> String {
    render(&ToolSpec {
        name: "server.lookup-item".into(),
        description: String::new(),
        input_schema: schema,
    })
}

#[test]
fn declares_required_optional_local_refs_and_raw_reply() {
    let rendered = declaration(json!({
        "type":"object", "additionalProperties":false,
        "properties":{
            "query":{"$ref":"#/$defs/a~1b%20~0"},
            "mode":{"enum":["short", "full"]},
            "limit":{"type":["integer", "null"],"minimum":1}
        },
        "required":["query"], "$defs":{"a/b ~":{"type":"string"}}
    }));
    assert!(rendered.contains("\"server.lookup-item\"(args: {"));
    assert!(rendered.contains("\"query\": string;"));
    assert!(rendered.contains("\"mode\"?: (\"short\" | \"full\");"));
    assert!(rendered.contains("\"limit\"?: (number | null);"));
    assert!(rendered.contains("value: unknown; is_error: boolean;"));
    assert!(rendered.contains("images: Array<{ type: \"image\"; id: string; mime_type: string }>"));
    assert!(!rendered.contains("[key: string]"));
}

#[test]
fn approximations_do_not_rule_out_valid_pattern_keys_or_optional_prefixes() {
    let rendered = declaration(json!({"type":"object","properties":{
        "mixed":{"type":"object","properties":{"label":{"type":"string"}},"additionalProperties":{"type":"number"}},
        "pattern":{"type":"object","patternProperties":{"^x":{"type":"integer"}},"additionalProperties":false},
        "prefix":{"type":"array","prefixItems":[{"type":"string"},{"type":"integer"}],"items":false},
        "empty":{"type":"object","additionalProperties":false},
        "untyped":{"properties":{"x":{"type":"string"}}},
        "draft7":{"$schema":"http://json-schema.org/draft-07/schema#","type":"array","prefixItems":[{"type":"string"}]}
    }}));
    assert!(rendered.contains("[key: string]: (number | string | undefined);"));
    assert!(rendered.contains("\"pattern\"?: { [key: string]: unknown; }"));
    assert!(rendered.contains("\"prefix\"?: [(string)?, (number)?, ]"));
    assert!(rendered.contains("\"empty\"?: Record<string, never>"));
    assert!(rendered.contains("\"untyped\"?: unknown"));
    assert!(rendered.contains("\"draft7\"?: unknown"));
}

#[test]
fn complex_schemas_fall_back_completely_without_unbounded_expansion() {
    for schema in [
        json!({"$ref":"#"}),
        json!({"enum":vec![true; MAX_NODES + 1]}),
        json!({"const":"x".repeat(MAX_TYPE_BYTES + 1)}),
        json!({"type":"object","properties":{("x".repeat(MAX_TYPE_BYTES + 1)):true}}),
        json!({"type":"object","required":vec!["x"; MAX_NODES + 1]}),
    ] {
        let rendered = declaration(schema);
        assert!(
            rendered.contains("Input type exceeds declaration limits"),
            "{rendered}"
        );
        assert!(rendered.contains("(args: unknown): Promise<"));
        assert!(rendered.len() < 1024);
    }
    for schema in [
        json!({"$ref":"https://invalid.test/schema"}),
        json!({"$ref":"#/%ff"}),
    ] {
        assert!(declaration(schema).contains("(args: unknown): Promise<"));
    }
}

#[test]
fn nested_resources_do_not_rebase_refs_to_the_outer_schema() {
    let rendered = declaration(json!({
        "type":"object", "$defs":{"T":{"type":"number"}},
        "properties":{
            "nested":{"$id":"https://example.test/inner","type":"object",
                "$defs":{"T":{"type":"string"}},"properties":{"x":{"$ref":"#/$defs/T"}}},
            "alias":{"$ref":"#/properties/nested/properties/x"}
        }
    }));
    assert!(rendered.contains("\"nested\"?: unknown;"));
    assert!(rendered.contains("\"alias\"?: unknown;"));
}

#[test]
fn hostile_names_and_literals_stay_inside_quoted_types() {
    let name = "\";\n}; declare const injected: any; //\u{2028}";
    let rendered = render(&ToolSpec {
        name: name.into(),
        description: "*/ hostile description".into(),
        input_schema: json!({"type":"object","properties":{(name):{"const":"*/\n\"\u{2029}"}}}),
    });
    let quoted = serde_json::to_string(name)
        .unwrap()
        .replace('\u{2028}', "\\u2028");
    assert!(rendered.contains(&format!("{quoted}(args:")));
    assert!(!rendered.contains(['\u{2028}', '\u{2029}']));
    assert!(!rendered.contains("hostile description"));
    assert!(rendered.contains("\\n\\\"\\u2029"));
}
