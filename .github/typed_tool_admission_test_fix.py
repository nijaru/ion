from pathlib import Path

# Legacy DelegateTool runtime fixtures model the same structural host-control
# capability as production child installation. Keep direct Tool::call tests
# untouched; only catalog-based runtime fixtures need structural registration.
p = Path("crates/ion-core/src/tests/budget_children.rs")
text = p.read_text()
if "catalog.register_scope(" not in text:
    raise SystemExit("delegate runtime registration anchor missing")
text = text.replace("catalog.register_scope(", "catalog.register_structural_scope(")

# Unknown tools are model-visible denials and do not fail the whole operation.
# Test the non-widening invariant itself: bash is never in the child's durable
# capability snapshot, and a model-proposed bash call is rejected as unknown.
text = text.replace('json!({ "command": "rm -rf /" })', 'json!({ "command": "exit 97" })', 1)
old = '''    let loaded = store.load(parent_id).await.expect("load");
    let tool_output = loaded
        .entries
        .iter()
        .find_map(|record| {
            let entry = &record.entry;
            serde_json::to_string(entry)
                .ok()
                .filter(|text| text.contains("child failed"))
        })
        .expect("the escape attempt fails visibly");
    assert!(
        tool_output.contains("unknown tool") || tool_output.contains("approval"),
        "denial is about the capability, not a crash: {tool_output}"
    );
'''
new = '''    let loaded = store.load(parent_id).await.expect("load");
    let child_id = loaded
        .entries
        .iter()
        .find_map(|record| {
            serde_json::to_string(&record.entry)
                .ok()
                .and_then(|text| child_ids(&text).into_iter().next())
        })
        .expect("child reference");
    let child = store.load(child_id).await.expect("child session");
    assert!(
        child.operations.iter().all(|operation| {
            !operation
                .capability_snapshot
                .tools
                .iter()
                .any(|tool| tool.name == "bash")
        }),
        "read-only child must never advertise bash"
    );
    assert!(
        child.entries.iter().any(|record| matches!(
            &record.entry,
            SessionEntry::ToolResult {
                result: ToolResult::Err { error, .. },
            } if error.contains("unknown tool") && error.contains("bash")
        )),
        "model-proposed bash must be rejected before execution: {:?}",
        child.entries
    );
'''
if old not in text:
    raise SystemExit("legacy child capability assertion anchor missing")
text = text.replace(old, new, 1)
p.write_text(text)

# Same-name scopes intentionally collide in a merged catalog, so policy-route
# metadata must be tested with independent catalogs rather than relying on
# hash-map iteration order to choose one duplicate.
p = Path("crates/ion-core/src/tool/catalog.rs")
text = p.read_text()
old = '''    #[test]
    fn structural_policy_route_is_available_only_through_core_composition() {
        let catalog = ToolCatalog::with_cwd("/tmp");
        catalog.register_scope("ordinary", vec![Arc::new(EchoTool)]);
        let ordinary = catalog
            .snapshot()
            .resolve_invocation("mcp_echo", &json!({}))
            .expect("ordinary resolution");
        assert_eq!(ordinary.policy_route, PolicyRoute::Gated);

        catalog.register_structural_scope("host-control", vec![Arc::new(EchoTool)]);
        let structural = catalog
            .snapshot()
            .resolve_invocation("mcp_echo", &json!({}))
            .expect("structural resolution");
        assert_eq!(structural.policy_route, PolicyRoute::Structural);
    }
'''
new = '''    #[test]
    fn structural_policy_route_is_available_only_through_core_composition() {
        let ordinary_catalog = ToolCatalog::with_cwd("/tmp");
        ordinary_catalog.register_scope("ordinary", vec![Arc::new(EchoTool)]);
        let ordinary = ordinary_catalog
            .snapshot()
            .resolve_invocation("mcp_echo", &json!({}))
            .expect("ordinary resolution");
        assert_eq!(ordinary.policy_route, PolicyRoute::Gated);

        let structural_catalog = ToolCatalog::with_cwd("/tmp");
        structural_catalog.register_structural_scope("host-control", vec![Arc::new(EchoTool)]);
        let structural = structural_catalog
            .snapshot()
            .resolve_invocation("mcp_echo", &json!({}))
            .expect("structural resolution");
        assert_eq!(structural.policy_route, PolicyRoute::Structural);
    }
'''
if old not in text:
    raise SystemExit("structural policy-route test anchor missing")
p.write_text(text.replace(old, new, 1))
