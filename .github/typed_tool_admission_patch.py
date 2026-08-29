from pathlib import Path


def replace(path: str, old: str, new: str, count: int = 1) -> None:
    p = Path(path)
    text = p.read_text()
    if text.count(old) < count:
        raise SystemExit(f"anchor missing in {path}: {old[:180]!r}")
    p.write_text(text.replace(old, new, count))


def insert_after(path: str, anchor: str, addition: str) -> None:
    replace(path, anchor, anchor + addition)


# ----- tool boundary: semantic metadata + typed resolution -----
replace(
    "crates/ion-core/src/tool/mod.rs",
    '''#[derive(Clone)]\nstruct ToolEntry {\n    tool: Arc<dyn Tool>,\n    spec: ToolSpec,\n    recovery_class: RecoveryClass,\n    capability_id: String,\n}\n''',
    '''#[derive(Clone)]\nstruct ToolEntry {\n    tool: Arc<dyn Tool>,\n    spec: ToolSpec,\n    recovery_class: RecoveryClass,\n    capability_id: String,\n    semantics: ToolSemantics,\n    policy_route: PolicyRoute,\n}\n\n/// Tool-owned invocation semantics. Public tool names are presentation/API\n/// identifiers; the runtime must never infer behavior from them.\n#[derive(Debug, Clone, Copy, PartialEq, Eq)]\nenum ToolSemantics {\n    RequiredPath,\n    OptionalPath,\n    ReconcileWrite,\n    ReconcileEdit,\n    Command,\n    Remote,\n}\n\n/// How an admitted tool reaches the approval policy. Structural host-control\n/// capabilities are trusted by composition; ordinary/native/remote effects\n/// are still individually gated.\n#[derive(Debug, Clone, Copy, PartialEq, Eq)]\npub(crate) enum PolicyRoute {\n    Gated,\n    Structural,\n}\n\n/// Typed result of resolving one model-proposed invocation against the exact\n/// immutable registry captured for the current model step.\n#[derive(Debug, Clone, PartialEq, Eq)]\npub(crate) struct ResolvedInvocation {\n    pub(crate) canonical: CanonicalTarget,\n    pub(crate) recovery_class: RecoveryClass,\n    pub(crate) policy_route: PolicyRoute,\n}\n''',
)

insert_after(
    "crates/ion-core/src/tool/mod.rs",
    '''pub enum CanonicalTarget {\n    /// Absolute, lexically normalized path (cwd-relative arguments are\n    /// resolved against the tool registry's working directory).\n    Path { path: std::path::PathBuf },\n    /// The exact shell command the executor will run.\n    Command { command: String },\n    /// A registered non-native tool (MCP/extension): the invocation\n    /// goes through its owning transport, not local I/O (§19.2).\n    Remote { tool: String },\n}\n''',
    '''\nimpl ToolSemantics {\n    fn canonicalize(\n        self,\n        cwd: &Path,\n        name: &str,\n        arguments: &Value,\n    ) -> Result<CanonicalTarget, String> {\n        let resolve = |key: &str| -> Result<PathBuf, String> {\n            let raw = arguments\n                .get(key)\n                .and_then(Value::as_str)\n                .ok_or_else(|| format!("missing string argument: {key}"))?;\n            resolve_under(cwd, raw)\n        };\n        match self {\n            Self::RequiredPath | Self::ReconcileWrite | Self::ReconcileEdit => {\n                Ok(CanonicalTarget::Path { path: resolve("path")? })\n            }\n            Self::OptionalPath => {\n                if arguments.get("path").is_some() {\n                    Ok(CanonicalTarget::Path { path: resolve("path")? })\n                } else {\n                    Ok(CanonicalTarget::Path {\n                        path: lexically_normalize(cwd),\n                    })\n                }\n            }\n            Self::Command => {\n                let command = arguments\n                    .get("command")\n                    .and_then(Value::as_str)\n                    .ok_or_else(|| "missing string argument: command".to_owned())?;\n                Ok(CanonicalTarget::Command {\n                    command: command.to_owned(),\n                })\n            }\n            Self::Remote => Ok(CanonicalTarget::Remote {\n                tool: name.to_owned(),\n            }),\n        }\n    }\n\n    const fn reconciliation_kind(self) -> Option<ReconciliationKind> {\n        match self {\n            Self::ReconcileWrite => Some(ReconciliationKind::Write),\n            Self::ReconcileEdit => Some(ReconciliationKind::Edit),\n            Self::RequiredPath | Self::OptionalPath | Self::Command | Self::Remote => None,\n        }\n    }\n\n    const fn accepts_artifact_root(self) -> bool {\n        matches!(self, Self::Command)\n    }\n}\n\n#[derive(Debug, Clone, Copy, PartialEq, Eq)]\nenum ReconciliationKind {\n    Write,\n    Edit,\n}\n''',
)

# Keep the compatibility helper for reconciliation tests, but make the actual
# implementation typed so registry callers do not branch on tool names.
replace(
    "crates/ion-core/src/tool/mod.rs",
    '''pub async fn reconciliation_evidence(\n    cwd: &Path,\n    name: &str,\n    arguments: &Value,\n) -> Result<Value, String> {\n''',
    '''pub(crate) async fn reconciliation_evidence(\n    cwd: &Path,\n    name: &str,\n    arguments: &Value,\n) -> Result<Value, String> {\n    let kind = match name {\n        "write" => ReconciliationKind::Write,\n        "edit" => ReconciliationKind::Edit,\n        other => return Err(format!("tool {other} takes no reconciliation evidence")),\n    };\n    reconciliation_evidence_for(cwd, kind, arguments).await\n}\n\nasync fn reconciliation_evidence_for(\n    cwd: &Path,\n    kind: ReconciliationKind,\n    arguments: &Value,\n) -> Result<Value, String> {\n''',
)
replace(
    "crates/ion-core/src/tool/mod.rs",
    '''    let postimage: Vec<u8> = match name {\n        "write" => arguments\n            .get("contents")\n            .and_then(|v| v.as_str())\n            .ok_or_else(|| "missing string argument: contents".to_owned())?\n            .as_bytes()\n            .to_vec(),\n        "edit" => {\n''',
    '''    let postimage: Vec<u8> = match kind {\n        ReconciliationKind::Write => arguments\n            .get("contents")\n            .and_then(|v| v.as_str())\n            .ok_or_else(|| "missing string argument: contents".to_owned())?\n            .as_bytes()\n            .to_vec(),\n        ReconciliationKind::Edit => {\n''',
)
replace(
    "crates/ion-core/src/tool/mod.rs",
    '''        }\n        other => return Err(format!("tool {other} takes no reconciliation evidence")),\n    };\n''',
    '''        }\n    };\n''',
)

# Resolve typed metadata at the registry boundary. Validation is intentionally
# part of resolution, so an invalid invocation never parks for approval.
replace(
    "crates/ion-core/src/tool/mod.rs",
    '''    /// Validate `arguments` against a tool's schema: the value must be an\n    /// object containing every name in the schema's `"required"` array.\n    /// Canonicalize one invocation's effective target (§17.3). Pure:\n    /// no filesystem access, so the decision input cannot change\n    /// between policy and executor.\n    pub fn canonicalize(&self, name: &str, arguments: &Value) -> Result<CanonicalTarget, String> {\n        let resolve = |key: &str| -> Result<std::path::PathBuf, String> {\n            let raw = arguments\n                .get(key)\n                .and_then(|v| v.as_str())\n                .ok_or_else(|| format!("missing string argument: {key}"))?;\n            resolve_under(&self.cwd, raw)\n        };\n        match name {\n            "read" | "write" | "edit" => Ok(CanonicalTarget::Path {\n                path: resolve("path")?,\n            }),\n            "search" | "find" => {\n                if arguments.get("path").is_some() {\n                    Ok(CanonicalTarget::Path {\n                        path: resolve("path")?,\n                    })\n                } else {\n                    Ok(CanonicalTarget::Path {\n                        path: lexically_normalize(&self.cwd),\n                    })\n                }\n            }\n            "bash" => {\n                let command = arguments\n                    .get("command")\n                    .and_then(|v| v.as_str())\n                    .ok_or_else(|| "missing string argument: command".to_owned())?;\n                Ok(CanonicalTarget::Command {\n                    command: command.to_owned(),\n                })\n            }\n            other => {\n                // Registered non-native tools (MCP/extension scopes)\n                // canonicalize to a remote target; truly unknown names\n                // still deny model-visibly.\n                if self.entries.contains_key(other) {\n                    Ok(CanonicalTarget::Remote {\n                        tool: other.to_owned(),\n                    })\n                } else {\n                    Err(format!("unknown tool: {other}"))\n                }\n            }\n        }\n    }\n''',
    '''    /// Canonicalize one invocation through tool-owned semantics. The\n    /// runtime sees the resulting target, never the name-to-semantics mapping.\n    pub fn canonicalize(&self, name: &str, arguments: &Value) -> Result<CanonicalTarget, String> {\n        let entry = self\n            .entries\n            .get(name)\n            .ok_or_else(|| format!("unknown tool: {name}"))?;\n        entry.semantics.canonicalize(&self.cwd, name, arguments)\n    }\n\n    pub(crate) fn resolve_invocation(\n        &self,\n        name: &str,\n        arguments: &Value,\n    ) -> Result<ResolvedInvocation, String> {\n        self.validate(name, arguments)?;\n        let entry = self\n            .entries\n            .get(name)\n            .ok_or_else(|| format!("unknown tool: {name}"))?;\n        Ok(ResolvedInvocation {\n            canonical: entry.semantics.canonicalize(&self.cwd, name, arguments)?,\n            recovery_class: entry.recovery_class,\n            policy_route: entry.policy_route,\n        })\n    }\n\n    pub(crate) async fn reconciliation_for(\n        &self,\n        name: &str,\n        arguments: &Value,\n    ) -> Result<Option<Value>, String> {\n        let entry = self\n            .entries\n            .get(name)\n            .ok_or_else(|| format!("unknown tool: {name}"))?;\n        match entry.semantics.reconciliation_kind() {\n            Some(kind) => reconciliation_evidence_for(&self.cwd, kind, arguments)\n                .await\n                .map(Some),\n            None => Ok(None),\n        }\n    }\n''',
)

# Executor enrichment also follows typed entry semantics, not public names.
replace(
    "crates/ion-core/src/tool/mod.rs",
    '''        if !matches!(name, "write" | "edit" | "bash")\n            || (reconciliation.is_none() && artifact_root.is_none())\n        {\n            return self\n                .execute_with_progress(name, arguments, cancel, progress)\n                .await;\n        }\n        let mut enriched = arguments.clone();\n        if let Some(object) = enriched.as_object_mut() {\n            if let Some(reconciliation) = reconciliation {\n                object.insert("__ion_reconciliation".to_owned(), reconciliation.clone());\n            }\n            if let Some(artifact_root) = artifact_root {\n                object.insert(\n                    "__ion_artifact_root".to_owned(),\n                    Value::String(artifact_root.to_string_lossy().into_owned()),\n                );\n            }\n        }\n''',
    '''        let Some(entry) = self.entries.get(name) else {\n            return ToolOutcome::error(format!("unknown tool: {name}"));\n        };\n        let needs_reconciliation = entry.semantics.reconciliation_kind().is_some();\n        let accepts_artifact_root = entry.semantics.accepts_artifact_root();\n        if (!needs_reconciliation || reconciliation.is_none())\n            && (!accepts_artifact_root || artifact_root.is_none())\n        {\n            return self\n                .execute_with_progress(name, arguments, cancel, progress)\n                .await;\n        }\n        let mut enriched = arguments.clone();\n        if let Some(object) = enriched.as_object_mut() {\n            if needs_reconciliation\n                && let Some(reconciliation) = reconciliation\n            {\n                object.insert("__ion_reconciliation".to_owned(), reconciliation.clone());\n            }\n            if accepts_artifact_root\n                && let Some(artifact_root) = artifact_root\n            {\n                object.insert(\n                    "__ion_artifact_root".to_owned(),\n                    Value::String(artifact_root.to_string_lossy().into_owned()),\n                );\n            }\n        }\n''',
)

# Core semantic metadata is declared once at construction.
replace(
    "crates/ion-core/src/tool/mod.rs",
    '''    let tools: Vec<(Arc<dyn Tool>, RecoveryClass)> = vec![\n        (\n            Arc::new(ReadTool {\n                cwd: cwd_path.clone(),\n            }),\n            RecoveryClass::ReplaySafe,\n        ),\n        (\n            Arc::new(WriteTool {\n                cwd: cwd_path.clone(),\n            }),\n            RecoveryClass::Reconcile,\n        ),\n        (\n            Arc::new(EditTool {\n                cwd: cwd_path.clone(),\n            }),\n            RecoveryClass::Reconcile,\n        ),\n        (\n            Arc::new(BashTool {\n                cwd: cwd_path.clone(),\n                sandbox,\n            }),\n            RecoveryClass::NeverReplay,\n        ),\n        (\n            Arc::new(SearchTool {\n                cwd: cwd_path.clone(),\n            }),\n            RecoveryClass::ReplaySafe,\n        ),\n        (\n            Arc::new(FindTool {\n                cwd: cwd_path.clone(),\n            }),\n            RecoveryClass::ReplaySafe,\n        ),\n    ];\n    let mut map = HashMap::new();\n    for (tool, recovery_class) in tools {\n''',
    '''    let tools: Vec<(Arc<dyn Tool>, RecoveryClass, ToolSemantics)> = vec![\n        (\n            Arc::new(ReadTool {\n                cwd: cwd_path.clone(),\n            }),\n            RecoveryClass::ReplaySafe,\n            ToolSemantics::RequiredPath,\n        ),\n        (\n            Arc::new(WriteTool {\n                cwd: cwd_path.clone(),\n            }),\n            RecoveryClass::Reconcile,\n            ToolSemantics::ReconcileWrite,\n        ),\n        (\n            Arc::new(EditTool {\n                cwd: cwd_path.clone(),\n            }),\n            RecoveryClass::Reconcile,\n            ToolSemantics::ReconcileEdit,\n        ),\n        (\n            Arc::new(BashTool {\n                cwd: cwd_path.clone(),\n                sandbox,\n            }),\n            RecoveryClass::NeverReplay,\n            ToolSemantics::Command,\n        ),\n        (\n            Arc::new(SearchTool {\n                cwd: cwd_path.clone(),\n            }),\n            RecoveryClass::ReplaySafe,\n            ToolSemantics::OptionalPath,\n        ),\n        (\n            Arc::new(FindTool {\n                cwd: cwd_path.clone(),\n            }),\n            RecoveryClass::ReplaySafe,\n            ToolSemantics::OptionalPath,\n        ),\n    ];\n    let mut map = HashMap::new();\n    for (tool, recovery_class, semantics) in tools {\n''',
)
replace(
    "crates/ion-core/src/tool/mod.rs",
    '''                spec,\n                recovery_class,\n            },\n''',
    '''                spec,\n                recovery_class,\n                semantics,\n                policy_route: PolicyRoute::Gated,\n            },\n''',
)

# ----- catalog: ordinary dynamic tools are gated; only core-owned composition
# can publish structural host-control tools. -----
replace(
    "crates/ion-core/src/tool/catalog.rs",
    '''fn dynamic_entries(scope: &str, generation: u64, tools: Vec<Arc<dyn Tool>>) -> Vec<ToolEntry> {\n''',
    '''fn dynamic_entries(\n    scope: &str,\n    generation: u64,\n    tools: Vec<Arc<dyn Tool>>,\n    policy_route: PolicyRoute,\n) -> Vec<ToolEntry> {\n''',
)
replace(
    "crates/ion-core/src/tool/catalog.rs",
    '''                spec,\n                recovery_class: RecoveryClass::NeverReplay,\n            }\n''',
    '''                spec,\n                recovery_class: RecoveryClass::NeverReplay,\n                semantics: ToolSemantics::Remote,\n                policy_route,\n            }\n''',
)
replace(
    "crates/ion-core/src/tool/catalog.rs",
    '''            .insert(scope.clone(), dynamic_entries(&scope, generation, tools));\n''',
    '''            .insert(\n                scope.clone(),\n                dynamic_entries(&scope, generation, tools, PolicyRoute::Gated),\n            );\n''',
    count=2,
)
insert_after(
    "crates/ion-core/src/tool/catalog.rs",
    '''    pub fn register_scope(&self, scope: impl Into<String>, tools: Vec<Arc<dyn Tool>>) {\n        let scope = scope.into();\n        let generation = next_generation(&self.generations, &scope);\n        self.dynamic\n            .write()\n            .expect("tool catalog poisoned")\n            .insert(\n                scope.clone(),\n                dynamic_entries(&scope, generation, tools, PolicyRoute::Gated),\n            );\n    }\n''',
    '''\n    /// Core-owned host controls may bypass per-effect approval because their\n    /// authority is structural and their spawned effects are gated separately.\n    /// This is crate-private so extensions/MCP cannot self-declare a bypass.\n    pub(crate) fn register_structural_scope(\n        &self,\n        scope: impl Into<String>,\n        tools: Vec<Arc<dyn Tool>>,\n    ) {\n        let scope = scope.into();\n        let generation = next_generation(&self.generations, &scope);\n        self.dynamic\n            .write()\n            .expect("tool catalog poisoned")\n            .insert(\n                scope.clone(),\n                dynamic_entries(&scope, generation, tools, PolicyRoute::Structural),\n            );\n    }\n''',
)

# Add a focused metadata test using the existing EchoTool.
insert_after(
    "crates/ion-core/src/tool/catalog.rs",
    '''    fn scope_registration_and_removal_change_future_snapshots() {\n        let catalog = ToolCatalog::with_cwd("/tmp");\n        assert!(!catalog.specs().iter().any(|s| s.name == "mcp_echo"));\n        catalog.register_scope("server-a", vec![Arc::new(EchoTool)]);\n        assert!(catalog.specs().iter().any(|s| s.name == "mcp_echo"));\n        // Removing the scope drops its tools from future snapshots.\n        assert!(catalog.remove_scope("server-a"));\n        assert!(!catalog.specs().iter().any(|s| s.name == "mcp_echo"));\n        assert!(!catalog.remove_scope("server-a"), "double remove is false");\n    }\n''',
    '''\n    #[test]\n    fn structural_policy_route_is_available_only_through_core_composition() {\n        let catalog = ToolCatalog::with_cwd("/tmp");\n        catalog.register_scope("ordinary", vec![Arc::new(EchoTool)]);\n        let ordinary = catalog\n            .snapshot()\n            .resolve_invocation("mcp_echo", &json!({}))\n            .expect("ordinary resolution");\n        assert_eq!(ordinary.policy_route, PolicyRoute::Gated);\n\n        catalog.register_structural_scope("host-control", vec![Arc::new(EchoTool)]);\n        let structural = catalog\n            .snapshot()\n            .resolve_invocation("mcp_echo", &json!({}))\n            .expect("structural resolution");\n        assert_eq!(structural.policy_route, PolicyRoute::Structural);\n    }\n''',
)

# ----- child composition: host uses the core-owned structural registration path. -----
insert_after(
    "crates/ion-core/src/delegate.rs",
    '''pub fn child_tools<P: Provider + 'static>(\n    config: DelegateConfig<P>,\n    parent_id: SessionId,\n) -> (Arc<ChildManager<P>>, Vec<Arc<dyn Tool>>) {\n    let manager = ChildManager::new(Arc::new(config), parent_id);\n    let tools = [\n        ChildToolKind::Spawn,\n        ChildToolKind::Status,\n        ChildToolKind::Wait,\n        ChildToolKind::Cancel,\n        ChildToolKind::Resume,\n    ]\n    .into_iter()\n    .map(|kind| {\n        Arc::new(ChildTool {\n            manager: Arc::clone(&manager),\n            kind,\n        }) as Arc<dyn Tool>\n    })\n    .collect();\n    (manager, tools)\n}\n''',
    '''\n/// Install the migration child-control surface as structural host capabilities.\n/// Extensions and MCP registrations cannot access this policy bypass.\npub fn install_child_tools<P: Provider + 'static>(\n    catalog: &crate::tool::ToolCatalog,\n    config: DelegateConfig<P>,\n    parent_id: SessionId,\n) -> Arc<ChildManager<P>> {\n    let (manager, tools) = child_tools(config, parent_id);\n    catalog.register_structural_scope("children", tools);\n    manager\n}\n''',
)
replace(
    "crates/ion-core/src/lib.rs",
    '''    DelegateConfig, child_budget_default, child_tools,\n''',
    '''    DelegateConfig, child_budget_default, child_tools, install_child_tools,\n''',
)
replace(
    "crates/ion/src/lib.rs",
    '''    let (manager, child_tools) = ion_core::child_tools(\n        ion_core::DelegateConfig {\n            store: store.clone(),\n            make_provider,\n            make_provider_for_model,\n            max_active_children: 4,\n            child_budget: ion_core::child_budget_default(),\n            trusted_resources,\n            cwd: tools.cwd().to_path_buf(),\n        },\n        parent_id,\n    );\n    tools.register_scope("children", child_tools);\n    manager\n''',
    '''    ion_core::install_child_tools(\n        tools,\n        ion_core::DelegateConfig {\n            store: store.clone(),\n            make_provider,\n            make_provider_for_model,\n            max_active_children: 4,\n            child_budget: ion_core::child_budget_default(),\n            trusted_resources,\n            cwd: tools.cwd().to_path_buf(),\n        },\n        parent_id,\n    )\n''',
)

# ----- runtime: consume typed resolution; no semantic name checks. -----
replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''    RecoveryClass, ToolCall, ToolCatalog, ToolProgress, ToolRegistry, ToolResult, ToolSelection,\n    ToolSpec,\n''',
    '''    PolicyRoute, RecoveryClass, ResolvedInvocation, ToolCall, ToolCatalog, ToolProgress,\n    ToolRegistry, ToolResult, ToolSelection, ToolSpec,\n''',
)

old = '''        let canonical = if active_capability == current_capability {\n            step_tools.canonicalize(&call.name, &call.arguments)\n        } else {\n            Err(format!("capability `{}` is no longer available", call.name))\n        };\n        let decision = match &canonical {\n            // Delegation is a structural capability (§20.4): every\n            // effect a child can produce is individually gated inside the\n            // child, so spawning one needs no grant.\n            Ok(_)\n                if matches!(\n                    call.name.as_str(),\n                    "delegate"\n                        | "spawn_child"\n                        | "child_status"\n                        | "child_wait"\n                        | "child_cancel"\n                        | "child_resume"\n                ) =>\n            {\n                PolicyDecision::Allow\n            }\n            Ok(target) => self.policy.decide(&call.name, target),\n            // Canonicalization failure is a model-visible denial, not a\n            // harness failure: the model produced an unusable input.\n            Err(message) => PolicyDecision::Deny(message.clone()),\n        };\n'''
new = '''        let resolved = if active_capability == current_capability {\n            step_tools.resolve_invocation(&call.name, &call.arguments)\n        } else {\n            Err(format!("capability `{}` is no longer available", call.name))\n        };\n        let decision = match &resolved {\n            Ok(invocation) if invocation.policy_route == PolicyRoute::Structural => {\n                PolicyDecision::Allow\n            }\n            Ok(invocation) => self.policy.decide(&call.name, &invocation.canonical),\n            // Resolution/validation failure is model-visible denial, not a\n            // harness failure: the model produced an unusable input.\n            Err(message) => PolicyDecision::Deny(message.clone()),\n        };\n'''
replace("crates/ion-core/src/runtime/mod.rs", old, new)

replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''        let mut denial: Option<String> = match decision {\n            PolicyDecision::Deny(message) => Some(message),\n            PolicyDecision::Allow => step_tools.validate(&call.name, &call.arguments).err(),\n            PolicyDecision::ApprovalRequired => unreachable!("handled above"),\n        };\n        // §12.3: file-mutating effects persist reconciliation evidence\n        // with the intent, before execution. An evidence failure means\n        // the invocation could not be classified, so it is denied\n        // model-visibly instead of admitted blind.\n        let evidence = if denial.is_none() && matches!(call.name.as_str(), "write" | "edit") {\n            match crate::tool::reconciliation_evidence(\n                step_tools.cwd(),\n                &call.name,\n                &call.arguments,\n            )\n            .await\n            {\n                Ok(evidence) => Some(evidence),\n                Err(message) => {\n                    denial = Some(message);\n                    None\n                }\n            }\n        } else {\n            None\n        };\n''',
    '''        let mut denial = match decision {\n            PolicyDecision::Deny(message) => Some(message),\n            PolicyDecision::Allow => None,\n            PolicyDecision::ApprovalRequired => unreachable!("handled above"),\n        };\n        // Reconciliation semantics are tool-owned. Runtime only asks the\n        // exact step registry to prepare whatever evidence this invocation\n        // requires before committing the effect intent.\n        let evidence = if denial.is_none() {\n            match step_tools.reconciliation_for(&call.name, &call.arguments).await {\n                Ok(evidence) => evidence,\n                Err(message) => {\n                    denial = Some(message);\n                    None\n                }\n            }\n        } else {\n            None\n        };\n''',
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''            canonical,\n            evidence,\n            denial,\n''',
    '''            resolved,\n            evidence,\n            denial,\n''',
    count=1,
)

replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''        canonical: Result<crate::tool::CanonicalTarget, String>,\n        evidence: Option<serde_json::Value>,\n''',
    '''        resolved: Result<ResolvedInvocation, String>,\n        evidence: Option<serde_json::Value>,\n''',
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''        // The exact invocation the executor will use is part of the\n        // durable intent (§17.3: never approve one string and execute\n        // a materially different one).\n        let effect = EffectRecord {\n            id: EffectId::generate(),\n            kind: format!("tool:{}", call.name),\n            recovery_class: step_tools.recovery_class(&call.name),\n            effective_input: serde_json::json!({\n                "tool": call.name,\n                "arguments": call.arguments,\n                "call_id": call.call_id,\n                "canonical": canonical.ok(),\n                "reconciliation": evidence,\n            }),\n            attempt: 1,\n        };\n''',
    '''        // The exact typed invocation the executor will use is part of the\n        // durable intent (§17.3: never approve one string and execute a\n        // materially different one). Denied unresolved calls never execute;\n        // NeverReplay is the conservative persisted classification for them.\n        let (canonical, recovery_class) = match resolved {\n            Ok(invocation) => (Some(invocation.canonical), invocation.recovery_class),\n            Err(_) => (None, RecoveryClass::NeverReplay),\n        };\n        let effect = EffectRecord {\n            id: EffectId::generate(),\n            kind: format!("tool:{}", call.name),\n            recovery_class,\n            effective_input: serde_json::json!({\n                "tool": call.name,\n                "arguments": call.arguments,\n                "call_id": call.call_id,\n                "canonical": canonical,\n                "reconciliation": evidence,\n            }),\n            attempt: 1,\n        };\n''',
)

# Approval path uses lane-scoped current capabilities, typed resolution, and
# the same tool-owned evidence preparation as ordinary admission.
replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''        let current_capability = self\n            .tools\n            .snapshot()\n            .capability_snapshot()\n            .identity(&call.name)\n            .map(str::to_owned);\n        let canonical = if active_capability == current_capability {\n            step_tools.canonicalize(&call.name, &call.arguments)\n        } else {\n            Err(format!("capability `{}` is no longer available", call.name))\n        };\n        let mut denial = canonical.as_ref().err().cloned();\n''',
    '''        let current_capability = self\n            .tool_registry_for_operation(operation_id)\n            .expect("approval operation has an owning lane")\n            .capability_snapshot()\n            .identity(&call.name)\n            .map(str::to_owned);\n        let resolved = if active_capability == current_capability {\n            step_tools.resolve_invocation(&call.name, &call.arguments)\n        } else {\n            Err(format!("capability `{}` is no longer available", call.name))\n        };\n        let mut denial = resolved.as_ref().err().cloned();\n''',
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''        let evidence = if denial.is_none() && matches!(call.name.as_str(), "write" | "edit") {\n            match crate::tool::reconciliation_evidence(\n                step_tools.cwd(),\n                &call.name,\n                &call.arguments,\n            )\n            .await\n            {\n                Ok(evidence) => Some(evidence),\n                Err(message) => {\n                    denial = Some(message);\n                    None\n                }\n            }\n        } else {\n            None\n        };\n''',
    '''        let evidence = if denial.is_none() {\n            match step_tools.reconciliation_for(&call.name, &call.arguments).await {\n                Ok(evidence) => evidence,\n                Err(message) => {\n                    denial = Some(message);\n                    None\n                }\n            }\n        } else {\n            None\n        };\n''',
)
# The second commit call is the approval path.
replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''                canonical,\n                evidence,\n                denial,\n''',
    '''                resolved,\n                evidence,\n                denial,\n''',
    count=1,
)

# DESIGN keeps the invariant explicit and marks this boundary complete.
replace(
    "DESIGN.md",
    '''The session runtime must not infer semantics from public tool names such as `write`, `edit`, `bash`, or `spawn_child`.\n''',
    '''The session runtime must not infer semantics from public tool names such as `write`, `edit`, `bash`, or `spawn_child`. Tool-owned typed admission metadata now carries canonicalization, recovery, reconciliation, and policy-route semantics into runtime admission.\n''',
)

# Guard the architectural purpose of the patch before compiling.
runtime = Path("crates/ion-core/src/runtime/mod.rs").read_text()
for forbidden in [
    'matches!(call.name.as_str(), "write" | "edit")',
    '"spawn_child"\n                        | "child_status"',
    'crate::tool::reconciliation_evidence(',
]:
    if forbidden in runtime:
        raise SystemExit(f"runtime still infers tool semantics: {forbidden!r}")
