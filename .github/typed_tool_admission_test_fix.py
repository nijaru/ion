from pathlib import Path

# Legacy DelegateTool runtime fixtures model the same structural host-control
# capability as production child installation. Keep direct Tool::call tests
# untouched; only catalog-based runtime fixtures need structural registration.
p = Path("crates/ion-core/src/tests/budget_children.rs")
text = p.read_text()
if "catalog.register_scope(" not in text:
    raise SystemExit("delegate runtime registration anchor missing")
text = text.replace("catalog.register_scope(", "catalog.register_structural_scope(")
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
