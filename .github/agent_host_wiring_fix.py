from pathlib import Path

p = Path("crates/ion/src/acp.rs")
text = p.read_text()
old = ''') -> AcpSession<P>\nwhere\n    P: Provider + 'static,\n{\n    let _agent_family = match crate::enable_agents(catalog, &runtime, 4).await {'''
new = ''') -> Result<AcpSession<P>, String>\nwhere\n    P: Provider + 'static,\n{\n    let _agent_family = match crate::enable_agents(catalog, &runtime, 4).await {'''
if old not in text:
    raise SystemExit("ACP attach return-type anchor missing")
p.write_text(text.replace(old, new, 1))

p = Path("crates/ion-core/src/tests/agent_family.rs")
text = p.read_text()
old = '''    let runtime = start_runtime_with_store(provider.clone(), catalog.clone().snapshot(), store.clone());'''
new = '''    let runtime = Runtime::start_with_policy(\n        provider.clone(),\n        catalog.clone(),\n        store.clone(),\n        permissive_policy(),\n    );'''
if old not in text:
    raise SystemExit("agent catalog runtime anchor missing")
p.write_text(text.replace(old, new, 1))
