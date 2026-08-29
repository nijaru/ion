from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one match, found {count}")
    file.write_text(text.replace(old, new, 1))


catalog = "crates/ion-core/src/tool/catalog.rs"
replace_once(
    catalog,
    '''    /// Activate one configured MCP server for future model-step snapshots.
    /// Server lifecycle and discovery remain owned by [`crate::McpService`].
    /// An unknown name is harmless: a later discovery can publish the scope.
    pub fn activate_mcp_server(&self, name: impl AsRef<str>) {
        let name = name.as_ref().trim();
        if !name.is_empty() {
            self.active_mcp_scopes
                .write()
                .expect("active MCP scope set poisoned")
                .insert(format!("mcp:{name}"));
        }
    }

    /// Replace the active MCP server set used by future model-step snapshots.
    /// The set is intentionally explicit and may be empty.
''',
    '''    /// Set the host-selected MCP server set used by future model-step
    /// snapshots. Current composition calls this before session admission;
    /// durable lane authority remains the structural grant, not this live
    /// publication filter. The set is intentionally explicit and may be empty.
''',
)
replace_once(
    catalog,
    '''
    /// Deactivate one MCP server. Its live process may remain supervised, but
    /// its tools disappear from future model-step snapshots.
    pub fn deactivate_mcp_server(&self, name: impl AsRef<str>) -> bool {
        let name = name.as_ref().trim();
        if name.is_empty() {
            return false;
        }
        self.active_mcp_scopes
            .write()
            .expect("active MCP scope set poisoned")
            .remove(&format!("mcp:{name}"))
    }
''',
    '''
''',
)
replace_once(
    catalog,
    '''    #[test]
    fn mcp_snapshots_use_only_the_explicit_active_set() {
        let catalog = ToolCatalog::with_cwd("/tmp");
        catalog.register_scope("mcp:docs", vec![Arc::new(EchoTool)]);
        assert!(!catalog.specs().iter().any(|s| s.name == "mcp_echo"));

        catalog.activate_mcp_server("docs");
        assert!(catalog.specs().iter().any(|s| s.name == "mcp_echo"));

        assert!(catalog.deactivate_mcp_server("docs"));
        assert!(!catalog.specs().iter().any(|s| s.name == "mcp_echo"));

        catalog.set_active_mcp_servers(["docs", "unknown", " "]);
        assert!(catalog.specs().iter().any(|s| s.name == "mcp_echo"));
        assert!(catalog.deactivate_mcp_server("unknown"));
        assert!(!catalog.deactivate_mcp_server("unknown"));
    }
''',
    '''    #[test]
    fn mcp_snapshots_use_only_the_explicit_active_set() {
        let catalog = ToolCatalog::with_cwd("/tmp");
        catalog.register_scope("mcp:docs", vec![Arc::new(EchoTool)]);
        assert!(!catalog.specs().iter().any(|s| s.name == "mcp_echo"));

        catalog.set_active_mcp_servers(["docs"]);
        assert!(catalog.specs().iter().any(|s| s.name == "mcp_echo"));

        catalog.set_active_mcp_servers(std::iter::empty::<&str>());
        assert!(!catalog.specs().iter().any(|s| s.name == "mcp_echo"));

        catalog.set_active_mcp_servers(["docs", "unknown", " "]);
        assert!(catalog.specs().iter().any(|s| s.name == "mcp_echo"));
        assert!(catalog.admission_scopes().contains("mcp:docs"));
        assert!(!catalog.admission_scopes().contains("mcp:unknown"));
    }
''',
)

mcp_tests = Path("crates/ion-core/src/tests/mcp.rs")
text = mcp_tests.read_text()
expected = {
    'catalog.activate_mcp_server("fake");': 4,
    'catalog.activate_mcp_server("restarting");': 1,
}
for needle, count in expected.items():
    actual = text.count(needle)
    if actual != count:
        raise SystemExit(f"mcp tests: expected {count} occurrences of {needle!r}, found {actual}")
text = text.replace('catalog.activate_mcp_server("fake");', 'catalog.set_active_mcp_servers(["fake"]);')
text = text.replace(
    'catalog.activate_mcp_server("restarting");',
    'catalog.set_active_mcp_servers(["restarting"]);',
)
mcp_tests.write_text(text)

replace_once(
    "DESIGN.md",
    '''Configured MCP/extension structural identities are declared before discovery and remain distinct from their currently live tool generation, so transient peer loss/restart does not accidentally revoke or ambiently re-grant authority. The current client snapshot still projects `main`; explicit host-driven scope deconfiguration/teardown beyond this declared/live split remains active work.''',
    '''Configured MCP/extension structural identities are declared before discovery and remain distinct from their currently live tool generation, so transient peer loss/restart does not accidentally revoke or ambiently re-grant authority. The current host has no live settings-reload or unload command that owns durable scope revocation: MCP selection is launch-time composition, and transient peer removal only unpublishes a generation. Do not add a generic revoke surface until a concrete host action can own that transition without resetting prior lane narrowing. Step 7 is complete for the currently implemented owners; a future live deconfiguration feature must add explicit structural revocation semantics. The current client snapshot still projects `main`.''',
)
replace_once(
    "DESIGN.md",
    '''7. Make scoped capability publication/teardown structural at lane/agent admission and exact on recovery; capability narrowing must never be reset by unrelated lane configuration changes.
8. Finish typed tool/effect admission boundaries and expand `ion-eval` around recovery and multi-agent invariants.''',
    '''7. Make scoped capability publication/teardown structural at lane/agent admission and exact on recovery; capability narrowing must never be reset by unrelated lane configuration changes. Current owners are established; live host deconfiguration stays deferred until a concrete owner exists.
8. Finish typed tool/effect admission boundaries and expand `ion-eval` around recovery and multi-agent invariants.''',
)

print("step7 deconfiguration boundary cleanup applied")
