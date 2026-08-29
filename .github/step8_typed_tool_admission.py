from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one exact match, found {count}")
    file.write_text(text.replace(old, new, 1))


path = "crates/ion-core/src/runtime/mod.rs"

replace_once(
    path,
    '''/// The live, in-memory side of the active operation. Cloned whole to
/// stage a transition: a failed durable commit discards the staged clone
/// and never mutates live state (DESIGN.md §26.2).
#[derive(Clone)]
struct ActiveOperation {''',
    '''/// One fully prepared post-policy tool admission. Resolution and
/// reconciliation are bundled so the durable commit boundary cannot receive
/// mismatched canonical/recovery/evidence/denial state.
enum PreparedToolAdmission {
    Execute {
        resolved: ResolvedInvocation,
        reconciliation: Option<serde_json::Value>,
    },
    Deny {
        resolved: Option<ResolvedInvocation>,
        message: String,
    },
}

/// The live, in-memory side of the active operation. Cloned whole to
/// stage a transition: a failed durable commit discards the staged clone
/// and never mutates live state (DESIGN.md §26.2).
#[derive(Clone)]
struct ActiveOperation {''',
)

replace_once(
    path,
    '''    /// Commit a tool effect intent, then spawn the tool effect (or
    /// settle a validation denial through the normal path). Returns
    /// false when persistence failed.
    async fn admit_next_tool(&mut self, operation_id: OperationId) -> bool {''',
    '''    fn resolve_tool_for_admission(
        &self,
        operation_id: OperationId,
        call: &ToolCall,
    ) -> (ToolRegistry, Result<ResolvedInvocation, String>) {
        let step_tools = self
            .active(operation_id)
            .map(|active| active.tool_registry.clone())
            .expect("tool admission needs the current step tool registry");
        let active_capability = self
            .active(operation_id)
            .and_then(|active| active.capability_snapshot.identity(&call.name));
        let current_registry = self
            .tool_registry_for_operation(operation_id)
            .expect("tool admission operation has an owning lane");
        let current_snapshot = current_registry.capability_snapshot();
        let current_capability = current_snapshot.identity(&call.name);
        let resolved = if active_capability == current_capability {
            step_tools.resolve_invocation(&call.name, &call.arguments)
        } else {
            Err(format!("capability `{}` is no longer available", call.name))
        };
        (step_tools, resolved)
    }

    fn tool_budget_denial(&self, operation_id: OperationId) -> Option<String> {
        self.budget.max_tool_calls.and_then(|max| {
            (self
                .live(operation_id)
                .expect("tool admission operation residency exists")
                .operation_tool_calls
                >= max)
                .then(|| "operation tool-call budget exhausted".to_owned())
        })
    }

    async fn prepare_tool_admission(
        call: &ToolCall,
        step_tools: &ToolRegistry,
        resolved: Result<ResolvedInvocation, String>,
        denial: Option<String>,
    ) -> PreparedToolAdmission {
        match resolved {
            Ok(resolved) => {
                if let Some(message) = denial {
                    return PreparedToolAdmission::Deny {
                        resolved: Some(resolved),
                        message,
                    };
                }
                match step_tools
                    .reconciliation_for(&call.name, &call.arguments)
                    .await
                {
                    Ok(reconciliation) => PreparedToolAdmission::Execute {
                        resolved,
                        reconciliation,
                    },
                    Err(message) => PreparedToolAdmission::Deny {
                        resolved: Some(resolved),
                        message,
                    },
                }
            }
            Err(message) => PreparedToolAdmission::Deny {
                resolved: None,
                message: denial.unwrap_or(message),
            },
        }
    }

    /// Commit a tool effect intent, then spawn the tool effect (or
    /// settle a validation denial through the normal path). Returns
    /// false when persistence failed.
    async fn admit_next_tool(&mut self, operation_id: OperationId) -> bool {''',
)

replace_once(
    path,
    '''        let active_capability = self
            .active(operation_id)
            .and_then(|active| active.capability_snapshot.identity(&call.name))
            .map(str::to_owned);
        let current_capability = self
            .tool_registry_for_operation(operation_id)
            .expect("tool admission operation has an owning lane")
            .capability_snapshot()
            .identity(&call.name)
            .map(str::to_owned);
        let step_tools = self
            .active(operation_id)
            .map(|active| active.tool_registry.clone())
            .expect("admit needs the current step tool registry");
        let resolved = if active_capability == current_capability {
            step_tools.resolve_invocation(&call.name, &call.arguments)
        } else {
            Err(format!("capability `{}` is no longer available", call.name))
        };
        let decision = match &resolved {
            Ok(invocation) if invocation.policy_route == PolicyRoute::Structural => {
                PolicyDecision::Allow
            }
            Ok(invocation) => self.policy.decide(&call.name, &invocation.canonical),
            // Resolution/validation failure is model-visible denial, not a
            // harness failure: the model produced an unusable input.
            Err(message) => PolicyDecision::Deny(message.clone()),
        };
        // Tool-call budget (§20.5): spent budget denies further calls
        // model-visibly; the model can still finish its turn.
        let over_tool_budget = self.budget.max_tool_calls.is_some_and(|max| {
            self.live_mut(operation_id)
                .expect("main operation residency exists")
                .operation_tool_calls
                >= max
        });
        let decision = if over_tool_budget {
            PolicyDecision::Deny("operation tool-call budget exhausted".to_owned())
        } else {
            decision
        };''',
    '''        let (step_tools, resolved) = self.resolve_tool_for_admission(operation_id, &call);
        let decision = if let Some(message) = self.tool_budget_denial(operation_id) {
            // Budget denial wins before an approval can park the operation.
            PolicyDecision::Deny(message)
        } else {
            match &resolved {
                Ok(invocation) if invocation.policy_route == PolicyRoute::Structural => {
                    PolicyDecision::Allow
                }
                Ok(invocation) => self.policy.decide(&call.name, &invocation.canonical),
                // Resolution/validation failure is model-visible denial, not a
                // harness failure: the model produced an unusable input.
                Err(message) => PolicyDecision::Deny(message.clone()),
            }
        };''',
)

replace_once(
    path,
    '''        let mut denial = match decision {
            PolicyDecision::Deny(message) => Some(message),
            PolicyDecision::Allow => None,
            PolicyDecision::ApprovalRequired => unreachable!("handled above"),
        };
        // Reconciliation semantics are tool-owned. Runtime only asks the
        // exact step registry to prepare whatever evidence this invocation
        // requires before committing the effect intent.
        let evidence = if denial.is_none() {
            match step_tools
                .reconciliation_for(&call.name, &call.arguments)
                .await
            {
                Ok(evidence) => evidence,
                Err(message) => {
                    denial = Some(message);
                    None
                }
            }
        } else {
            None
        };
        self.commit_tool_admission(
            operation_id,
            Transition::AdmitNextTool,
            "admit next tool from ToolsPlanned",
            resolved,
            evidence,
            denial,
        )
        .await''',
    '''        let denial = match decision {
            PolicyDecision::Deny(message) => Some(message),
            PolicyDecision::Allow => None,
            PolicyDecision::ApprovalRequired => unreachable!("handled above"),
        };
        let prepared = Self::prepare_tool_admission(&call, &step_tools, resolved, denial).await;
        self.commit_tool_admission(
            operation_id,
            Transition::AdmitNextTool,
            "admit next tool from ToolsPlanned",
            prepared,
        )
        .await''',
)

replace_once(
    path,
    '''        resolved: Result<ResolvedInvocation, String>,
        evidence: Option<serde_json::Value>,
        denial: Option<String>,
    ) -> bool {''',
    '''        prepared: PreparedToolAdmission,
    ) -> bool {''',
)

replace_once(
    path,
    '''        // The exact typed invocation the executor will use is part of the
        // durable intent (§17.3: never approve one string and execute a
        // materially different one). Denied unresolved calls never execute;
        // NeverReplay is the conservative persisted classification for them.
        let (canonical, recovery_class) = match resolved {
            Ok(invocation) => (Some(invocation.canonical), invocation.recovery_class),
            Err(_) => (None, RecoveryClass::NeverReplay),
        };
        let effect = EffectRecord::new(
            EffectId::generate(),
            DurableEffect::Tool(ToolInvocation {
                tool: call.name.clone(),
                arguments: call.arguments.clone(),
                call_id: call.call_id,
                canonical,
                reconciliation: evidence.clone(),
            }),
            recovery_class,
            1,
        );''',
    '''        // The exact typed invocation the executor will use is part of the
        // durable intent (§17.3: never approve one string and execute a
        // materially different one). The prepared enum prevents execution
        // evidence from coexisting with a denial or an unresolved call.
        let (canonical, recovery_class, reconciliation, denial) = match prepared {
            PreparedToolAdmission::Execute {
                resolved,
                reconciliation,
            } => (
                Some(resolved.canonical),
                resolved.recovery_class,
                reconciliation,
                None,
            ),
            PreparedToolAdmission::Deny { resolved, message } => {
                let (canonical, recovery_class) = resolved.map_or(
                    (None, RecoveryClass::NeverReplay),
                    |resolved| (Some(resolved.canonical), resolved.recovery_class),
                );
                (canonical, recovery_class, None, Some(message))
            }
        };
        let effect = EffectRecord::new(
            EffectId::generate(),
            DurableEffect::Tool(ToolInvocation {
                tool: call.name.clone(),
                arguments: call.arguments.clone(),
                call_id: call.call_id,
                canonical,
                reconciliation: reconciliation.clone(),
            }),
            recovery_class,
            1,
        );''',
)

replace_once(
    path,
    '''            self.spawn_tool_effect(effect_id, call, evidence, step_tools);''',
    '''            self.spawn_tool_effect(effect_id, call, reconciliation, step_tools);''',
)

replace_once(
    path,
    '''        let step_tools = staged.tool_registry.clone();
        let active_capability = staged
            .capability_snapshot
            .identity(&call.name)
            .map(str::to_owned);
        let current_capability = self
            .tool_registry_for_operation(operation_id)
            .expect("approval operation has an owning lane")
            .capability_snapshot()
            .identity(&call.name)
            .map(str::to_owned);
        let resolved = if active_capability == current_capability {
            step_tools.resolve_invocation(&call.name, &call.arguments)
        } else {
            Err(format!("capability `{}` is no longer available", call.name))
        };
        let mut denial = resolved.as_ref().err().cloned();
        if self.budget.max_tool_calls.is_some_and(|max| {
            self.live_mut(operation_id)
                .expect("main operation residency exists")
                .operation_tool_calls
                >= max
        }) {
            denial = Some("operation tool-call budget exhausted".to_owned());
        }
        let evidence = if denial.is_none() {
            match step_tools
                .reconciliation_for(&call.name, &call.arguments)
                .await
            {
                Ok(evidence) => evidence,
                Err(message) => {
                    denial = Some(message);
                    None
                }
            }
        } else {
            None
        };
        self.install_active(staged);
        let committed = self
            .commit_tool_admission(
                operation_id,
                Transition::ApproveCall,
                "approve a parked call",
                resolved,
                evidence,
                denial,
            )
            .await;''',
    '''        let (step_tools, resolved) = self.resolve_tool_for_admission(operation_id, &call);
        let denial = self.tool_budget_denial(operation_id);
        let prepared = Self::prepare_tool_admission(&call, &step_tools, resolved, denial).await;
        self.install_active(staged);
        let committed = self
            .commit_tool_admission(
                operation_id,
                Transition::ApproveCall,
                "approve a parked call",
                prepared,
            )
            .await;''',
)

replace_once(
    "DESIGN.md",
    '''8. Finish typed tool/effect admission boundaries and expand `ion-eval` around recovery and multi-agent invariants. Runtime effect writers and recovery consume the typed durable vocabulary through `EffectRecord`; SQLite's compact kind/JSON encoding is confined to that translation boundary. Remaining work is typed admission/evaluation coverage rather than parallel JSON interpretation.''',
    '''8. Finish typed tool/effect admission boundaries and expand evaluation around recovery and multi-agent invariants. Runtime effect writers and recovery consume the typed durable vocabulary through `EffectRecord`; ordinary and approved tool calls share one typed post-policy admission preparation, so canonical/recovery/reconciliation/denial state reaches the durable commit boundary as one coherent value. SQLite's compact kind/JSON encoding remains confined to the effect translation boundary.''',
)

print("Step 8 typed tool admission migration applied")
