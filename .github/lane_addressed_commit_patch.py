from pathlib import Path
import re


def replace_once(path: Path, old: str, new: str, label: str) -> None:
    text = path.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected 1 match, found {count}")
    path.write_text(text.replace(old, new, 1))


def regex_once(path: Path, pattern: str, replacement: str, label: str) -> None:
    text = path.read_text()
    new, count = re.subn(pattern, replacement, text, count=1, flags=re.S)
    if count != 1:
        raise SystemExit(f"{label}: expected 1 match, found {count}")
    path.write_text(new)


runtime = Path("crates/ion-core/src/runtime/mod.rs")

# Resolve lane ownership from authoritative live lane state/residency. This also
# covers reopened Suspended operations that intentionally have no execution
# residency but remain the lane's current_operation until recovery settles them.
replace_once(
    runtime,
    '''    fn lane_resident_id(&self, lane_name: &str) -> Option<OperationId> {
''',
    '''    fn operation_lane_name(&self, operation_id: OperationId) -> Option<&str> {
        self.resident(operation_id)
            .map(|resident| resident.lane_name.as_str())
            .or_else(|| {
                self.lanes.iter().find_map(|(name, lane)| {
                    (lane.state.current_operation == Some(operation_id)).then_some(name.as_str())
                })
            })
    }

    fn lane_resident_id(&self, lane_name: &str) -> Option<OperationId> {
''',
    "operation lane resolver",
)

# The store already derives operation lane from immutable origin. Mirror the
# successful durable transition into the same lane in live state, never main by
# convention.
regex_once(
    runtime,
    r'''    async fn commit_transition\(&mut self, mut request: CommitRequest\) -> Result<\(\), StoreError> \{.*?\n    \}\n\n    fn stage_entry''',
    '''    async fn commit_transition(&mut self, mut request: CommitRequest) -> Result<(), StoreError> {
        let operation_id = request.operation_id;
        let lane_name = self
            .operation_lane_name(operation_id)
            .ok_or_else(|| StoreError::Sqlite(format!(
                "operation {operation_id} has no live lane ownership"
            )))?
            .to_owned();
        let terminal = matches!(
            request.checkpoint.payload.state,
            OperationState::Finished(_)
        );
        let mut parent = self
            .lanes
            .get(&lane_name)
            .expect("operation lane exists while session runtime is live")
            .state
            .leaf;
        for entry in &mut request.entries {
            entry.parent = parent;
            parent = Some(entry.id);
        }
        let entries = request.entries.clone();
        let new_leaf = entries.last().map(|entry| entry.id);
        self.store.commit(request).await?;
        self.install_tree_entries(entries);
        let lane = self
            .lanes
            .get_mut(&lane_name)
            .expect("operation lane exists after durable commit");
        if let Some(new_leaf) = new_leaf {
            lane.state.leaf = Some(new_leaf);
        }
        lane.state.current_operation = if terminal { None } else { Some(operation_id) };
        Ok(())
    }

    fn stage_entry''',
    "lane-addressed commit transition",
)

# ToolStarted presentation is already operation-attributed; keep its live spinner
# state on that operation too.
regex_once(
    runtime,
    r'''    fn emit_tool_started\(\n        &mut self,\n        operation_id: OperationId,\n        call_id: u64,\n        tool: &str,\n        target: Option<String>,\n    \) \{.*?\n    \}\n\n    /// Session close''',
    '''    fn emit_tool_started(
        &mut self,
        operation_id: OperationId,
        call_id: u64,
        tool: &str,
        target: Option<String>,
    ) {
        self.emit(RuntimeEvent::ToolStarted {
            cursor: RuntimeCursor::default(),
            operation_id,
            call_id,
            target: target.clone(),
            tool: tool.to_owned(),
        });
        let live = self
            .live_mut(operation_id)
            .expect("operation residency exists while starting tool effect");
        live.live_tools.retain(|pending| pending.call_id != call_id);
        live.live_tools.push(PendingTool {
            call_id,
            tool: tool.to_owned(),
            target,
        });
    }

    /// Session close''',
    "operation-address tool presentation",
)

# Close is a session lifecycle transition, so it must suspend every resident
# operation before cancelling the shared root. There is still only one in
# production today, but this removes a hidden single-main teardown assumption.
regex_once(
    runtime,
    r'''        let close_gate = self\.effect_gate\.clone\(\);\n        if let Some\(mut staged\) = self\.main_active\(\)\.cloned\(\) \{.*?\n        \}\n        self\.cancel_root\.cancel\(\);''',
    '''        let close_gate = self.effect_gate.clone();
        let operation_ids = self.operations.keys().copied().collect::<Vec<_>>();
        for operation_id in operation_ids {
            let Some(mut staged) = self.active(operation_id).cloned() else {
                continue;
            };
            let cancel = staged.cancel.clone();
            staged
                .machine
                .apply(Transition::Suspend)
                .expect("suspend from an open operation");
            staged.open_effect = None;
            let (request, new_entry_seq) = build_commit_request(
                self.session_id,
                &staged,
                staged.state_seq + 1,
                self.next_entry_seq,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            );
            if let Some(gate) = &close_gate {
                gate.wait(EffectBoundary::CloseSuspendCommit).await;
            }
            match self.commit_transition(request).await {
                Ok(()) => {
                    self.next_entry_seq = new_entry_seq;
                    staged.state_seq += 1;
                    self.install_active(staged);
                }
                Err(err) => {
                    error!(
                        session = %self.session_id,
                        %operation_id,
                        %err,
                        "suspend checkpoint failed; durable operation stays open"
                    );
                }
            }
            cancel.cancel();
        }
        self.cancel_root.cancel();''',
    "suspend every resident on close",
)
