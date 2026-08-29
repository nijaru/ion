from pathlib import Path

# Replace the expanded child constructor parameters with one typed topology
# boundary and make the reopen config name control lineage explicitly.
p = Path("crates/ion-core/src/runtime/mod.rs")
text = p.read_text()
anchor = '''/// Host dependencies needed to reopen a durable child runtime.\n#[derive(Clone)]\npub struct ChildRuntimeConfig {\n    pub policy: Arc<dyn PolicyEngine>,\n    pub budget: RuntimeBudget,\n    pub parent: SessionId,\n    pub trusted_resources: Vec<TrustedResource>,\n}\n'''
replacement = '''/// Exact semantic source of an explicitly forked separately hosted session.\n#[derive(Debug, Clone, Copy, PartialEq, Eq)]\npub struct SessionForkSource {\n    pub session_id: SessionId,\n    pub entry_id: Option<EntryId>,\n}\n\n/// Durable topology supplied when creating a separately hosted child session.\n/// Control ownership and history inheritance are deliberately independent.\n#[derive(Debug, Clone, Copy, PartialEq, Eq)]\npub struct ChildSessionLineage {\n    pub control_parent: SessionId,\n    pub fork_source: Option<SessionForkSource>,\n}\n\n/// Host dependencies needed to reopen a durable child runtime.\n#[derive(Clone)]\npub struct ChildRuntimeConfig {\n    pub policy: Arc<dyn PolicyEngine>,\n    pub budget: RuntimeBudget,\n    pub control_parent: SessionId,\n    pub trusted_resources: Vec<TrustedResource>,\n}\n'''
if anchor not in text:
    raise SystemExit("ChildRuntimeConfig anchor missing")
text = text.replace(anchor, replacement, 1)
old = '''        budget: RuntimeBudget,\n        parent: SessionId,\n        fork_source: Option<(SessionId, Option<EntryId>)>,\n        trusted_resources: Vec<TrustedResource>,\n    ) -> Self {\n        let tools = tools.into();\n        let cwd = tools.cwd().to_string_lossy().into_owned();\n        let mut composition = Composition::new(provider, tools, store);\n        composition.policy = policy;\n        composition.budget = budget;\n        composition.parent = Some(parent);\n        composition.fork_source = fork_source;\n'''
new = '''        budget: RuntimeBudget,\n        lineage: ChildSessionLineage,\n        trusted_resources: Vec<TrustedResource>,\n    ) -> Self {\n        let tools = tools.into();\n        let cwd = tools.cwd().to_string_lossy().into_owned();\n        let mut composition = Composition::new(provider, tools, store);\n        composition.policy = policy;\n        composition.budget = budget;\n        composition.parent = Some(lineage.control_parent);\n        composition.fork_source = lineage\n            .fork_source\n            .map(|source| (source.session_id, source.entry_id));\n'''
if old not in text:
    raise SystemExit("expanded start_child signature anchor missing")
text = text.replace(old, new, 1)
text = text.replace("Some(config.parent)", "Some(config.control_parent)")
text = text.replace("composition.parent = Some(config.parent);", "composition.parent = Some(config.control_parent);")
p.write_text(text)

# Convert both production and test child construction sites to the typed lineage
# value. The local fork_source tuple is kept only as short-lived composition
# data before it crosses the runtime API.
p = Path("crates/ion-core/src/delegate.rs")
text = p.read_text()
old = '''            self.config.child_budget,\n            self.parent_id,\n            fork_source,\n            self.config.trusted_resources.clone(),\n'''
new = '''            self.config.child_budget,\n            crate::runtime::ChildSessionLineage {\n                control_parent: self.parent_id,\n                fork_source: fork_source.map(|(session_id, entry_id)| {\n                    crate::runtime::SessionForkSource {\n                        session_id,\n                        entry_id,\n                    }\n                }),\n            },\n            self.config.trusted_resources.clone(),\n'''
if old not in text:
    raise SystemExit("production typed lineage call anchor missing")
text = text.replace(old, new, 1)
old = '''        config.child_budget,\n        parent_id,\n        fork_source,\n        config.trusted_resources.clone(),\n'''
new = '''        config.child_budget,\n        crate::runtime::ChildSessionLineage {\n            control_parent: parent_id,\n            fork_source: fork_source.map(|(session_id, entry_id)| {\n                crate::runtime::SessionForkSource {\n                    session_id,\n                    entry_id,\n                }\n            }),\n        },\n        config.trusted_resources.clone(),\n'''
if old not in text:
    raise SystemExit("fixture typed lineage call anchor missing")
text = text.replace(old, new, 1)
text = text.replace("parent: self.parent_id,", "control_parent: self.parent_id,")
p.write_text(text)

# Public runtime API exports the typed topology vocabulary.
p = Path("crates/ion-core/src/lib.rs")
text = p.read_text()
old = '''    ChildRuntimeConfig, EventSubscription, IndeterminateWarning, LiveOperationState,\n    OperationStatus, PendingTool, Runtime, RuntimeBudget, RuntimeEvent, RuntimeHandle,\n    SessionHandle, SessionSnapshot,\n'''
new = '''    ChildRuntimeConfig, ChildSessionLineage, EventSubscription, IndeterminateWarning,\n    LiveOperationState, OperationStatus, PendingTool, Runtime, RuntimeBudget, RuntimeEvent,\n    RuntimeHandle, SessionForkSource, SessionHandle, SessionSnapshot,\n'''
if old not in text:
    raise SystemExit("runtime export anchor missing")
p.write_text(text.replace(old, new, 1))
