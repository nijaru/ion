from pathlib import Path


def replace(path: str, old: str, new: str, count: int = 1) -> None:
    p = Path(path)
    text = p.read_text()
    if text.count(old) < count:
        raise SystemExit(f"anchor missing in {path}: {old[:180]!r}")
    p.write_text(text.replace(old, new, count))


# Host composition gets one explicit helper for the common family authority.
# Structural publication remains inside ion-core, so external tool providers
# cannot opt themselves into the host-control policy route.
p = Path("crates/ion/src/lib.rs")
text = p.read_text()
anchor = '''pub use acp::AcpConfig;\npub use settings::Settings;\n'''
addition = '''pub use acp::AcpConfig;\npub use settings::Settings;\n\n/// Attach the durable shared-history agent family to one runtime and publish\n/// its model-facing structural controls into `tools`. Identity remains durable\n/// even when execution capacity is exhausted.\npub async fn enable_agents(\n    tools: &ion_core::ToolCatalog,\n    runtime: &ion_core::Runtime,\n    max_active: usize,\n) -> Result<Arc<ion_core::AgentFamily>, ion_core::AgentError> {\n    let family = Arc::new(runtime.agent_family(max_active).await?);\n    ion_core::install_agent_tools(tools, Arc::clone(&family));\n    Ok(family)\n}\n'''
if anchor not in text:
    raise SystemExit("ion lib export anchor missing")
p.write_text(text.replace(anchor, addition, 1))

# TUI host: install lane-agent controls before the legacy fresh/fork child
# surface. Keep the family handle in host scope; tools also retain it.
p = Path("crates/ion/src/main.rs")
text = p.read_text()
old = '''    let child_manager = ion::enable_children_with_model_resolver(\n        &tools,\n        &store,\n        Arc::clone(&make_provider),\n        make_provider_for_model,\n        runtime.session_id(),\n        trusted_resources.clone(),\n    );\n'''
new = '''    let _agent_family = match ion::enable_agents(&tools, &runtime, 4).await {\n        Ok(family) => family,\n        Err(err) => {\n            let _ = writeln!(io::stderr(), "agents: {err}");\n            let _ = runtime.session().close().await;\n            let _ = runtime.join().await;\n            if let Err(close_err) = tools.close().await {\n                tracing::error!(error = %close_err, "failed to close the tool catalog");\n            }\n            let _ = store.close().await;\n            return ExitCode::FAILURE;\n        }\n    };\n    let child_manager = ion::enable_children_with_model_resolver(\n        &tools,\n        &store,\n        Arc::clone(&make_provider),\n        make_provider_for_model,\n        runtime.session_id(),\n        trusted_resources.clone(),\n    );\n'''
if old not in text:
    raise SystemExit("main child-manager anchor missing")
p.write_text(text.replace(old, new, 1))

# ACP sessions get the same family controls. Attachment becomes async because
# Family reattaches durable identities/permits from the store before publishing.
p = Path("crates/ion/src/acp.rs")
text = p.read_text()
text = text.replace(
    '''    let session = attach_session(config, &catalog, runtime, session_id, trusted_resources);\n    Ok((session_id.to_string(), session))\n''',
    '''    let session = attach_session(config, &catalog, runtime, session_id, trusted_resources).await?;\n    Ok((session_id.to_string(), session))\n''',
    1,
)
text = text.replace(
    '''/// Attach bounded read-only child delegation (§20) to the catalog and wrap\n/// the runtime in the per-connection ACP state.\nfn attach_session<P>(\n''',
    '''/// Attach shared-history family controls plus the separate-session\n/// fresh/fork child migration surface, then wrap the runtime in ACP state.\nasync fn attach_session<P>(\n''',
    1,
)
old = '''{\n    let child_manager = crate::enable_children(\n        catalog,\n        &config.store,\n        Arc::clone(&config.make_provider),\n        session_id,\n        trusted_resources,\n    );\n    AcpSession {\n        handle: runtime.session(),\n        runtime,\n        catalog: catalog.clone(),\n        child_manager,\n        active_prompt: Arc::new(Mutex::new(None)),\n    }\n}\n'''
new = '''{\n    let _agent_family = match crate::enable_agents(catalog, &runtime, 4).await {\n        Ok(family) => family,\n        Err(err) => {\n            let handle = runtime.session();\n            let _ = handle.close().await;\n            let _ = runtime.join().await;\n            return Err(format!("attach agent family: {err}"));\n        }\n    };\n    let child_manager = crate::enable_children(\n        catalog,\n        &config.store,\n        Arc::clone(&config.make_provider),\n        session_id,\n        trusted_resources,\n    );\n    Ok(AcpSession {\n        handle: runtime.session(),\n        runtime,\n        catalog: catalog.clone(),\n        child_manager,\n        active_prompt: Arc::new(Mutex::new(None)),\n    })\n}\n'''
if old not in text:
    raise SystemExit("ACP attach body anchor missing")
text = text.replace(old, new, 1)
text = text.replace(
    '''    let session = attach_session(config, &catalog, runtime, session_id, trusted_resources);\n    Ok((session_id_text.to_owned(), session, history))\n''',
    '''    let session = attach_session(config, &catalog, runtime, session_id, trusted_resources).await?;\n    Ok((session_id_text.to_owned(), session, history))\n''',
    1,
)
p.write_text(text)

# Strengthen the lane capability regression: even after root structural agent
# controls are installed, descendants retain only their read-only selection.
p = Path("crates/ion-core/src/tests/agent_family.rs")
text = p.read_text()
old = '''    let runtime =\n        start_runtime_with_store(provider.clone(), ToolRegistry::default(), store.clone());\n    let family = runtime.agent_family(1).await.expect("family");\n    let agent = family\n'''
new = '''    let catalog = ToolCatalog::default();\n    let runtime = start_runtime_with_store(provider.clone(), catalog.clone().snapshot(), store.clone());\n    let family = Arc::new(runtime.agent_family(1).await.expect("family"));\n    crate::install_agent_tools(&catalog, Arc::clone(&family));\n    let agent = family\n'''
if old not in text:
    raise SystemExit("agent read-only test setup anchor missing")
text = text.replace(old, new, 1)
text = text.replace(
    '''            .any(|name| matches!(*name, "write" | "edit" | "bash"))\n    );\n''',
    '''            .any(|name| matches!(*name, "write" | "edit" | "bash" | "spawn_agent" | "agent_start" | "agent_send"))\n    );\n''',
    1,
)
p.write_text(text)
