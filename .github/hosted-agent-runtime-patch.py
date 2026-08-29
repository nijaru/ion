from pathlib import Path
import re


def replace_one(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected 1 match, found {count}")
    return text.replace(old, new, 1)


def remove_between(text: str, start: str, end: str, label: str) -> str:
    i = text.find(start)
    if i < 0:
        raise SystemExit(f"{label}: start marker missing")
    j = text.find(end, i)
    if j < 0:
        raise SystemExit(f"{label}: end marker missing")
    return text[:i] + text[j:]


# ---------------------------------------------------------------------------
# Production hosted-agent runtime owner. The semantic family now owns address,
# topology, observation, wait, and cancellation; this module retains only the
# provider/runtime/catalog residency needed by fresh/fork agents. The old
# synchronous delegate fixture remains cfg(test) until its budget tests migrate.
# ---------------------------------------------------------------------------
p = Path("crates/ion-core/src/delegate.rs")
text = p.read_text()
use_pos = text.find("use std::{")
if use_pos < 0:
    raise SystemExit("delegate imports marker missing")
text = '''//! Hosted fresh/fork agent runtime composition.\n//!\n//! Durable agent identity, topology, status, waiting, cancellation, and exact\n//! results belong to `agent::Family`. This module owns only process-local\n//! provider/runtime/catalog residency for agents whose history lives in a\n//! separate session. Closing residency never deletes durable agent state.\n//!\n//! A synchronous `delegate` implementation remains under `cfg(test)` solely\n//! for older budget/policy coverage while that fixture is migrated.\n\n''' + text[use_pos:]

# Domain names that are still real after the child-only control surface is gone.
for old, new in [
    ("ChildContextMode", "HostedHistory"),
    ("ChildSpec", "HostedAgentSpec"),
    ("DelegateConfig", "HostedAgentConfig"),
    ("ManagedChild", "HostedRuntime"),
    ("ChildManager", "HostedAgentRuntimes"),
    ("child_budget_default", "hosted_agent_budget_default"),
    ("max_active_children", "max_active"),
    ("child_budget", "budget"),
]:
    text = text.replace(old, new)

text = text.replace("    fmt,\n", "")
text = text.replace(
    "use crate::ids::{OperationId, SessionId};",
    "use crate::ids::{AgentId, OperationId, SessionId};",
)
text = text.replace(
    "use crate::session::{OperationOutcome, OperationState};",
    "use crate::session::OperationState;",
)
text = text.replace("pub enum HostedHistory", "enum HostedHistory")
text = text.replace("pub struct HostedAgentSpec", "struct HostedAgentSpec")

# Remove the obsolete durable child identity/status projection. AgentId + Family
# are now the only model-facing semantic address/state contract.
text = remove_between(
    text,
    "/// Stable durable identity for a child session",
    "struct HostedRuntime {",
    "obsolete child identity/status block",
)

# Narrow the live residency record: operation/result/objective are durable and
# read through Family, not duplicated here.
text = replace_one(
    text,
    '''struct HostedRuntime {\n    session: crate::runtime::SessionHandle,\n    runtime: Option<Runtime>,\n    catalog: crate::tool::ToolCatalog,\n    operation_id: OperationId,\n    objective: String,\n    cancel: CancellationToken,\n    cancel_watch: JoinHandle<()>,\n}\n''',
    '''struct HostedRuntime {\n    session: crate::runtime::SessionHandle,\n    runtime: Option<Runtime>,\n    catalog: crate::tool::ToolCatalog,\n    cancel: CancellationToken,\n    cancel_watch: JoinHandle<()>,\n}\n''',
    "hosted runtime fields",
)
text = text.replace(
    "/// Process-owned manager for durable child runtimes. The store is the\n/// authority for status; this registry only retains live runtime incarnations\n/// so cancellation, waiting, and shutdown can be routed to them.\n",
    "/// Process-owned residency for separately hosted fresh/fork agents.\n/// Family/store state is authoritative; this registry owns only live runtime\n/// resources and registers their `SessionHandle`s with the family authority.\n",
)
text = text.replace("    children: Mutex<HashMap<SessionId, HostedRuntime>>,", "    runtimes: Mutex<HashMap<SessionId, HostedRuntime>>,")
text = text.replace("            children: Mutex::new(HashMap::new()),", "            runtimes: Mutex::new(HashMap::new()),")
text = text.replace("self.children", "self.runtimes")
text = text.replace("child manager poisoned", "hosted agent runtimes poisoned")
text = text.replace("release_live_child", "release_hosted_runtime")
text = text.replace("reap_finished_children", "reap_finished")

# Replace the now-unused SessionHandle getter with a liveness predicate used by
# resume. Family owns the actual live handle registry.
old_live = '''    fn live_session(&self, session_id: SessionId) -> Option<crate::runtime::SessionHandle> {\n        self.runtimes\n            .lock()\n            .expect("hosted agent runtimes poisoned")\n            .get(&session_id)\n            .map(|child| child.session.clone())\n    }\n'''
new_live = '''    fn is_live(&self, session_id: SessionId) -> bool {\n        self.runtimes\n            .lock()\n            .expect("hosted agent runtimes poisoned")\n            .contains_key(&session_id)\n    }\n'''
text = replace_one(text, old_live, new_live, "live residency predicate")

# Public constructor replaces child_tools(), which previously existed only to
# smuggle the manager out beside a now-unused duplicate tool namespace.
impl_marker = "impl<P> HostedAgentRuntimes<P> {\n"
constructor = '''/// Construct the process-owned fresh/fork runtime residency for one root family.\npub fn hosted_agent_runtimes<P: Provider + 'static>(\n    config: HostedAgentConfig<P>,\n    parent_id: SessionId,\n) -> Arc<HostedAgentRuntimes<P>> {\n    HostedAgentRuntimes::new(Arc::new(config), parent_id)\n}\n\n'''
if impl_marker not in text:
    raise SystemExit("hosted runtime impl marker missing")
text = text.replace(impl_marker, constructor + impl_marker, 1)

text = text.replace(
    "    /// Reclaim completed live runtimes before applying the live-child bound.\n    /// Status comes from the durable store, not from task liveness.\n",
    "    /// Reclaim completed live runtimes before applying the hosted-runtime bound.\n    /// Completion comes from the durable store, not from task liveness.\n",
)
text = text.replace("child {session_id} unavailable while reaping", "hosted agent {session_id} unavailable while reaping")
text = text.replace("child limit reached (max {})", "hosted agent runtime limit reached (max {})")
text = text.replace("child agent admission failed", "agent admission failed")
text = text.replace("child agent {agent_id} was admitted", "agent {agent_id} was admitted")
text = text.replace("child close failed", "hosted agent close failed")
text = text.replace("child join failed", "hosted agent join failed")
text = text.replace("child catalog close failed", "hosted agent catalog close failed")
text = text.replace("child cancellation watcher failed", "hosted agent cancellation watcher failed")

# Spawn now returns the semantic AgentId directly.
text = replace_one(
    text,
    ") -> Result<ChildHandle, String>\n    where\n        P: Provider,\n    {",
    ") -> Result<AgentId, String>\n    where\n        P: Provider,\n    {",
    "hosted spawn return type",
)
old_spawn_tail = '''        let handle = ChildHandle {\n            session_id,\n            control_parent_session_id: self.parent_id,\n        };\n        let cancel = parent_cancel.child_token();\n        let cancel_for_watch = cancel.clone();\n        let session_for_watch = session.clone();\n        let cancel_watch = tokio::spawn(async move {\n            cancel_for_watch.cancelled().await;\n            let _ = session_for_watch.cancel(operation_id).await;\n        });\n        let objective = spec.objective;\n        self.runtimes\n            .lock()\n            .expect("hosted agent runtimes poisoned")\n            .insert(\n                session_id,\n                HostedRuntime {\n                    session,\n                    runtime: Some(runtime),\n                    catalog,\n                    operation_id,\n                    objective: objective.clone(),\n                    cancel,\n                    cancel_watch,\n                },\n            );\n        report_progress(progress, format!("child {handle} accepted: {objective}")).await;\n        Ok(handle)\n    }\n'''
new_spawn_tail = '''        let cancel = parent_cancel.child_token();\n        let cancel_for_watch = cancel.clone();\n        let session_for_watch = session.clone();\n        let cancel_watch = tokio::spawn(async move {\n            cancel_for_watch.cancelled().await;\n            let _ = session_for_watch.cancel(operation_id).await;\n        });\n        let objective = spec.objective;\n        self.runtimes\n            .lock()\n            .expect("hosted agent runtimes poisoned")\n            .insert(\n                session_id,\n                HostedRuntime {\n                    session,\n                    runtime: Some(runtime),\n                    catalog,\n                    cancel,\n                    cancel_watch,\n                },\n            );\n        report_progress(progress, format!("agent {agent_id} accepted: {objective}")).await;\n        Ok(agent_id)\n    }\n'''
text = replace_one(text, old_spawn_tail, new_spawn_tail, "spawn tail")

# Remove manager-owned observe/wait; Family owns both. Keep and simplify resume.
text = remove_between(
    text,
    "    async fn observe(&self, handle: ChildHandle)",
    "    async fn resume(",
    "manager observe/wait",
)
resume_start = text.find("    async fn resume(")
resume_end = text.find("    async fn cancel(", resume_start)
if resume_start < 0 or resume_end < 0:
    raise SystemExit("resume/cancel markers missing")
resume = text[resume_start:resume_end]
resume = resume.replace(
    '''    async fn resume(\n        self: &Arc<Self>,\n        handle: ChildHandle,\n        parent_cancel: CancellationToken,\n        progress: Option<&ToolProgressSender>,\n    ) -> Result<ChildObservation, String>''',
    '''    async fn resume(\n        self: &Arc<Self>,\n        agent_id: AgentId,\n        session_id: SessionId,\n        parent_cancel: CancellationToken,\n        progress: Option<&ToolProgressSender>,\n    ) -> Result<(), String>''',
)
resume = resume.replace("if self.live_session(handle.session_id).is_some() {\n            return self.observe(handle).await;\n        }", "if self.is_live(session_id) {\n            return Ok(());\n        }")
resume = resume.replace("handle.session_id", "session_id")
resume = resume.replace(
    '''        if loaded.session.control_parent_session_id != Some(self.parent_id)\n            || handle.control_parent_session_id != self.parent_id\n        {\n            return Err(format!(\n                "child {} is not owned by this parent",\n                session_id\n            ));\n        }\n''',
    '''        let belongs_to_family = loaded.agents.iter().any(|agent| {\n            agent.id == agent_id\n                && agent.family_session_id == self.parent_id\n                && agent.session_id == session_id\n        });\n        if loaded.session.control_parent_session_id != Some(self.parent_id) || !belongs_to_family {\n            return Err(format!("agent {agent_id} is not owned by this family"));\n        }\n''',
)
resume = resume.replace(
    '''            return Err(format!(\n                "child {} has no operation to resume",\n                session_id\n            ));''',
    '''            return Err(format!("agent {agent_id} has no operation to resume"));''',
)
resume = resume.replace("return self.observe(handle).await;", "return Ok(());")
resume = resume.replace("unavailable for this child", "unavailable for this hosted agent")
resume = resume.replace('format!("child {} resume failed: {err}", session_id)', 'format!("agent {agent_id} resume failed: {err}")')
resume = resume.replace(
    '''        let objective = loaded\n            .entries\n            .iter()\n            .find_map(|record| match &record.entry {\n                crate::session::SessionEntry::UserMessage { text } => Some(text.clone()),\n                _ => None,\n            });\n''',
    "",
)
resume = resume.replace(
    '''                HostedRuntime {\n                    session,\n                    runtime: Some(runtime),\n                    catalog,\n                    operation_id,\n                    objective: objective.unwrap_or_else(|| "resumed child".to_owned()),\n                    cancel,\n                    cancel_watch,\n                },''',
    '''                HostedRuntime {\n                    session,\n                    runtime: Some(runtime),\n                    catalog,\n                    cancel,\n                    cancel_watch,\n                },''',
)
resume = resume.replace(
    'report_progress(progress, format!("child {} resumed", session_id)).await;\n        self.observe(handle).await',
    'report_progress(progress, format!("agent {agent_id} resumed")).await;\n        Ok(())',
)
if "ChildHandle" in resume or "ChildObservation" in resume or "self.observe" in resume:
    raise SystemExit("resume still depends on child observation surface")
text = text[:resume_start] + resume + text[resume_end:]

# Manager cancellation is also Family-owned; remove it.
text = remove_between(
    text,
    "    async fn cancel(&self, handle: ChildHandle)",
    "    pub async fn close(&self)",
    "manager cancel",
)

# Remove obsolete child observation helpers and the duplicate child tool
# constructor/installer. HostAgentTool is the sole production model-facing API.
text = remove_between(
    text,
    "impl ChildObservation {",
    "#[derive(Clone, Copy)]\nenum HostAgentToolKind",
    "child control namespace",
)

# HostedAgentRuntimes names in the unified host.
text = text.replace("children: Arc<HostedAgentRuntimes<P>>", "hosted: Arc<HostedAgentRuntimes<P>>")
text = text.replace("children.bind_family", "hosted.bind_family")
text = text.replace("Arc::clone(&children)", "Arc::clone(&hosted)")
text = text.replace("children: Arc<HostedAgentRuntimes<P>>,", "hosted: Arc<HostedAgentRuntimes<P>>,")
text = text.replace("agent_host_tools(family, children)", "agent_host_tools(family, hosted)")
text = text.replace("self.children", "self.hosted")
text = text.replace("children: Arc<HostedAgentRuntimes<P>>", "hosted: Arc<HostedAgentRuntimes<P>>")
# Constructor field assignment after parameter rename.
text = text.replace("            children: Arc::clone(&hosted),", "            hosted: Arc::clone(&hosted),")

# Spawn no longer returns a session-handle wrapper.
text = text.replace("let child_spec = HostedAgentSpec", "let hosted_spec = HostedAgentSpec")
text = text.replace(".spawn(child_spec, cancel, progress.as_ref())", ".spawn(hosted_spec, cancel, progress.as_ref())")
old_ok = '''                                Ok(handle) => {\n                                    let agent_id = crate::ids::AgentId::root(handle.session_id);\n                                    ToolOutcome::text(format!("agent handle: {agent_id}\\nstarted"))\n                                }\n'''
new_ok = '''                                Ok(agent_id) => {\n                                    ToolOutcome::text(format!("agent handle: {agent_id}\\nstarted"))\n                                }\n'''
text = replace_one(text, old_ok, new_ok, "host spawn result")
text = text.replace("self.hosted.release_live_child", "self.hosted.release_hosted_runtime")
text = text.replace("self.hosted.release_hosted_runtime", "self.hosted.release_hosted_runtime")

# Resume takes semantic identity + already-resolved durable target session.
old_resume_call = '''                            let handle =\n                                child_handle_for_session(session_id, self.hosted.parent_id);\n                            match self\n                                .hosted\n                                .resume(handle, cancel, progress.as_ref())\n                                .await\n                            {\n                                Ok(_) => match self.family.observe(agent_id).await {\n'''
new_resume_call = '''                            match self\n                                .hosted\n                                .resume(agent_id, session_id, cancel, progress.as_ref())\n                                .await\n                            {\n                                Ok(()) => match self.family.observe(agent_id).await {\n'''
text = replace_one(text, old_resume_call, new_resume_call, "host resume call")

# Remove any residual child helper/tool implementation between the family
# renderer and the test-only synchronous delegate fixture.
legacy_tool_impl = text.find("impl<P: Provider + 'static> Tool for ChildTool<P>")
legacy_fixture = text.find("/// Legacy synchronous delegation fixture")
if legacy_tool_impl >= 0:
    if legacy_fixture < legacy_tool_impl:
        raise SystemExit("legacy child tool fixture ordering corrupt")
    text = text[:legacy_tool_impl] + text[legacy_fixture:]
# Helper is no longer meaningful after ChildHandle removal.
text = re.sub(
    r"\nfn child_handle_for_session\(.*?\n}\n",
    "\n",
    text,
    count=1,
    flags=re.S,
)
# parse_handle belonged only to the removed child tool namespace.
text = re.sub(
    r"\nfn parse_handle\(arguments: &Value\).*?\n}\n",
    "\n",
    text,
    count=1,
    flags=re.S,
)

# Test-only synchronous delegate uses the same hosted config/spec vocabulary.
text = text.replace("Arc<HostedAgentConfig<P>>", "Arc<HostedAgentConfig<P>>")
text = text.replace("let semaphore = Arc::new(tokio::sync::Semaphore::new(self.config.max_active));", "let semaphore = Arc::new(tokio::sync::Semaphore::new(self.config.max_active));")

# Public production API guardrails.
for forbidden in [
    "pub struct ChildHandle",
    "pub enum ChildStatus",
    "pub struct ChildObservation",
    "pub struct ChildManager",
    "pub fn child_tools",
    "pub fn install_child_tools",
    'name: "spawn_child"',
    'name: "child_status"',
    'name: "child_wait"',
    'name: "child_cancel"',
    'name: "child_resume"',
]:
    if forbidden in text:
        raise SystemExit(f"obsolete production child surface remains: {forbidden}")
if "ChildHandle" in text or "ChildStatus" in text or "ChildObservation" in text:
    raise SystemExit("child handle/status types remain after cleanup")

# Rename module file to match the production ownership boundary.
new_path = Path("crates/ion-core/src/agent_host.rs")
new_path.write_text(text)
p.unlink()

# ---------------------------------------------------------------------------
# Core exports: one agent namespace + hosted runtime residency. The synchronous
# DelegateTool stays test-only.
# ---------------------------------------------------------------------------
p = Path("crates/ion-core/src/lib.rs")
text = p.read_text()
text = replace_one(text, "mod delegate;", "mod agent_host;", "core module rename")
text = replace_one(text, "pub use delegate::DelegateTool;", "pub use agent_host::DelegateTool;", "test delegate export")
old = '''pub use delegate::{\n    ChildContextMode, ChildHandle, ChildManager, ChildObservation, ChildSpec, ChildStatus,\n    DelegateConfig, agent_host_tools, child_budget_default, child_tools, install_agent_host_tools,\n    install_child_tools,\n};\n'''
new = '''pub use agent_host::{\n    HostedAgentConfig, HostedAgentRuntimes, agent_host_tools, hosted_agent_budget_default,\n    hosted_agent_runtimes, install_agent_host_tools,\n};\n'''
text = replace_one(text, old, new, "core hosted exports")
p.write_text(text)

# ---------------------------------------------------------------------------
# Application host composition consumes the runtime-residency constructor
# directly instead of constructing a dead child tool set to obtain its manager.
# ---------------------------------------------------------------------------
p = Path("crates/ion/src/lib.rs")
text = p.read_text()
text = text.replace("children: Arc<ion_core::ChildManager<P>>", "hosted: Arc<ion_core::HostedAgentRuntimes<P>>")
text = text.replace("self.children.close().await", "self.hosted.close().await")
old = '''    let (children, _legacy_child_tools) = ion_core::child_tools(\n        ion_core::DelegateConfig {\n            store: store.clone(),\n            make_provider,\n            make_provider_for_model,\n            max_active_children: 4,\n            child_budget: ion_core::child_budget_default(),\n            trusted_resources,\n            cwd: tools.cwd().to_path_buf(),\n        },\n        runtime.session_id(),\n    );\n    ion_core::install_agent_host_tools(tools, Arc::clone(&family), Arc::clone(&children));\n    Ok(AgentHost { family, children })\n'''
new = '''    let hosted = ion_core::hosted_agent_runtimes(\n        ion_core::HostedAgentConfig {\n            store: store.clone(),\n            make_provider,\n            make_provider_for_model,\n            max_active: 4,\n            budget: ion_core::hosted_agent_budget_default(),\n            trusted_resources,\n            cwd: tools.cwd().to_path_buf(),\n        },\n        runtime.session_id(),\n    );\n    ion_core::install_agent_host_tools(tools, Arc::clone(&family), Arc::clone(&hosted));\n    Ok(AgentHost { family, hosted })\n'''
text = replace_one(text, old, new, "application host construction")
text = text.replace("separately hosted fresh/fork descendants share one host lifetime even while\n/// the latter still use the migration child runtime internally.", "separately hosted fresh/fork descendants share one host lifetime. The latter\n/// retain only process-local provider/runtime residency outside `Family`.")
p.write_text(text)

# ---------------------------------------------------------------------------
# Family tests: construct hosted runtime residency directly.
# ---------------------------------------------------------------------------
p = Path("crates/ion-core/src/tests/agent_family.rs")
text = p.read_text()
text = text.replace(
    "let (children, _legacy_child_tools) = crate::child_tools(",
    "let hosted = crate::hosted_agent_runtimes(",
)
text = text.replace("crate::DelegateConfig {", "crate::HostedAgentConfig {")
text = text.replace("max_active_children:", "max_active:")
text = text.replace("child_budget:", "budget:")
text = text.replace("Arc::clone(&children)", "Arc::clone(&hosted)")
text = text.replace("children.close().await", "hosted.close().await")
p.write_text(text)

# ---------------------------------------------------------------------------
# The legacy child-handle test is superseded by unified agent-family coverage.
# Keep only the cfg(test) synchronous delegate tests that still exercise budget
# semantics, and use the renamed config vocabulary there.
# ---------------------------------------------------------------------------
p = Path("crates/ion-core/src/tests/budget_children.rs")
text = p.read_text()
start = text.find("#[tokio::test]\nasync fn durable_child_handles_support_spawn_status_wait_and_cancel()")
end = text.find("fn delegate_tool(", start)
if start < 0 or end < 0:
    raise SystemExit("legacy durable child handle test markers missing")
text = text[:start] + text[end:]
text = text.replace("crate::DelegateConfig {", "crate::HostedAgentConfig {")
text = text.replace("max_active_children:", "max_active:")
text = text.replace("child_budget:", "budget:")
text = text.replace("// ---- Bounded child delegation (§20, Step 7) ----", "// ---- Test-only synchronous delegation budget fixture ----")
p.write_text(text)

# ---------------------------------------------------------------------------
# Replace the child-tool lifecycle test with the same capacity invariant through
# the unified fresh-agent surface, then rename the module accordingly.
# ---------------------------------------------------------------------------
old_test = Path("crates/ion-core/src/tests/child_lifecycle.rs")
if not old_test.exists():
    raise SystemExit("child_lifecycle test file missing")
new_test = Path("crates/ion-core/src/tests/hosted_agent_lifecycle.rs")
new_test.write_text(r'''//! Separately hosted agent runtime lifecycle tests.

use super::support::*;

fn agent_handle(output: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix("agent handle: "))
        .expect("durable agent handle")
        .to_owned()
}

#[tokio::test]
async fn completed_hosted_agents_release_live_runtime_slots() {
    let store = SessionStore::open_in_memory().expect("store");
    let parent_runtime = Runtime::start_with_store(
        ScriptedProvider::new(Vec::new()),
        ToolRegistry::default(),
        store.clone(),
    );
    let family = Arc::new(parent_runtime.agent_family(2).await.expect("family"));
    let hosted = crate::hosted_agent_runtimes(
        crate::HostedAgentConfig {
            store: store.clone(),
            make_provider: Arc::new(|| {
                ScriptedProvider::new(vec![ScriptedMessage::text("agent answer")])
            }),
            make_provider_for_model: None,
            max_active: 1,
            budget: crate::RuntimeBudget::unbounded(),
            trusted_resources: Vec::new(),
            cwd: std::env::current_dir().expect("cwd"),
        },
        parent_runtime.session_id(),
    );
    let tools = crate::agent_host_tools(Arc::clone(&family), Arc::clone(&hosted));
    let spawn = tools
        .iter()
        .find(|tool| tool.spec().name == "spawn_agent")
        .expect("spawn agent tool");
    let wait = tools
        .iter()
        .find(|tool| tool.spec().name == "agent_wait")
        .expect("wait agent tool");

    let first = spawn
        .call(
            json!({"objective": "first", "topology": "fresh"}),
            CancellationToken::new(),
        )
        .await;
    assert!(!first.is_error, "first agent failed: {first:?}");
    let first_handle = agent_handle(&first.output);
    let waited = wait
        .call(
            json!({"handle": first_handle}),
            CancellationToken::new(),
        )
        .await;
    assert!(!waited.is_error, "first wait failed: {waited:?}");
    assert!(waited.output.contains("finished"), "{waited:?}");

    let second = spawn
        .call(
            json!({"objective": "second", "topology": "fresh"}),
            CancellationToken::new(),
        )
        .await;
    assert!(
        !second.is_error,
        "a completed hosted agent must not consume the one live slot: {second:?}"
    );
    let second_handle = agent_handle(&second.output);
    let waited = wait
        .call(
            json!({"handle": second_handle}),
            CancellationToken::new(),
        )
        .await;
    assert!(!waited.is_error, "second wait failed: {waited:?}");

    hosted.close().await.expect("close hosted agents");
    parent_runtime
        .session()
        .close()
        .await
        .expect("close parent");
    parent_runtime.join().await.expect("join parent");
    store.close().await.expect("close store");
}
''')
old_test.unlink()

p = Path("crates/ion-core/src/tests.rs")
text = p.read_text()
text = replace_one(text, "mod child_lifecycle;", "mod hosted_agent_lifecycle;", "test module rename")
p.write_text(text)

# ---------------------------------------------------------------------------
# Canonical architecture follows the ownership change; remove stale names and
# record that lane/fresh/fork are one production agent namespace now.
# ---------------------------------------------------------------------------
p = Path("DESIGN.md")
text = p.read_text()
text = text.replace("### 10.2 Child/agent creation", "### 10.2 Agent creation")
text = text.replace("such as `write`, `edit`, `bash`, or `spawn_child`", "such as `write`, `edit`, `bash`, or `spawn_agent`")
text = text.replace("A fresh child may have control lineage without history lineage. A user fork may have history lineage without a control parent. A fork-context child may have both.", "A fresh agent may have control lineage without history lineage. A user fork may have history lineage without a control parent. A forked agent may have both.")
text = text.replace("`ChildManager` and the test-only delegation path are migration scaffolding rather than target architecture.", "The remaining test-only synchronous delegation path is migration scaffolding rather than target architecture.")
old_checkpoint = "Storage, recovery, and live execution now support multiple concurrent lanes under one session writer. Operation residency/effects/continuation are operation-addressed, family-scoped retained agents have separate execution permits, waits are event-driven, and agent messaging uses the durable input path. The current client snapshot still projects `main`; scoped capabilities and replacement of the remaining child-only scaffolding are the active boundary."
new_checkpoint = "Storage, recovery, and live execution now support multiple concurrent lanes under one session writer. Operation residency/effects/continuation are operation-addressed, family-scoped retained agents have separate execution permits, waits are event-driven across shared and separately hosted sessions, and agent messaging uses the durable input path. Lane/fresh/fork agents share one model-facing namespace and durable family authority; a hosted-runtime service owns only fresh/fork provider/runtime/catalog residency. The current client snapshot still projects `main`; scoped capabilities and migration of the final test-only synchronous delegation fixture are the active boundary."
text = replace_one(text, old_checkpoint, new_checkpoint, "design checkpoint")
p.write_text(text)

# Repository-level stale production API guard. The test-only words `delegate`
# and `child` are still expected in the synchronous fixture and its tests.
for path in [
    Path("crates/ion-core/src/lib.rs"),
    Path("crates/ion/src/lib.rs"),
    Path("crates/ion-core/src/agent_host.rs"),
]:
    data = path.read_text()
    for forbidden in ["ChildManager", "ChildHandle", "ChildStatus", "ChildObservation", "child_tools", "install_child_tools"]:
        if forbidden in data:
            raise SystemExit(f"{path}: stale {forbidden}")
