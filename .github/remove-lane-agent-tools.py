from pathlib import Path


def replace_one(path: str, old: str, new: str, label: str) -> None:
    file = Path(path)
    text = file.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected 1 match, found {count}")
    file.write_text(text.replace(old, new, 1))


# Family is the semantic authority; the unified agent host is the only
# model-facing publisher. Remove the older lane-only namespace rather than
# retaining two public control surfaces with the same tool names/scope.
p = Path("crates/ion-core/src/agent.rs")
text = p.read_text()
text = text.replace("use serde_json::{Value, json};\n", "")
text = text.replace("use crate::tool::{Tool, ToolCatalog, ToolOutcome, ToolSpec};\n", "")

render = '''impl Observation {
    fn render(&self) -> String {
        let status = match &self.status {
            Status::Admitted => "admitted".to_owned(),
            Status::Active {
                operation_id,
                state,
            } => {
                format!("active ({operation_id}, {state:?})")
            }
            Status::Suspended { operation_id } => format!("suspended ({operation_id})"),
            Status::Finished {
                operation_id,
                outcome,
            } => {
                format!("finished ({operation_id}, {outcome:?})")
            }
        };
        match &self.result {
            Some(result) => format!("agent {}: {status}\\n\\n{result}", self.agent_id),
            None => format!("agent {}: {status}", self.agent_id),
        }
    }
}

'''
if text.count(render) != 1:
    raise SystemExit(f"observation renderer: expected 1 match, found {text.count(render)}")
text = text.replace(render, "", 1)

marker = "#[derive(Clone, Copy)]\nenum AgentToolKind {"
pos = text.find(marker)
if pos < 0:
    raise SystemExit("lane-only agent tool block missing")
text = text[:pos].rstrip() + "\n"
if any(token in text for token in ["AgentToolKind", "struct AgentTool", "agent_tools(", "install_agent_tools(", "parse_agent_handle(", "agent_handle_schema("]):
    raise SystemExit("lane-only model-facing agent API remains")
p.write_text(text)

replace_one(
    "crates/ion-core/src/lib.rs",
    '''pub use agent::{
    Error as AgentError, Family as AgentFamily, Observation as AgentObservation,
    Status as AgentStatus, agent_tools, install_agent_tools,
};
''',
    '''pub use agent::{
    Error as AgentError, Family as AgentFamily, Observation as AgentObservation,
    Status as AgentStatus,
};
''',
    "agent public exports",
)

# This test covers structural lane narrowing. It should exercise Family
# admission directly, without installing an obsolete model-facing namespace.
replace_one(
    "crates/ion-core/src/tests/agent_family.rs",
    '''    let family = Arc::new(runtime.agent_family(1).await.expect("family"));
    crate::install_agent_tools(&catalog, Arc::clone(&family));
    let agent = family
''',
    '''    let family = runtime.agent_family(1).await.expect("family");
    let agent = family
''',
    "lane read-only test setup",
)

replace_one(
    "DESIGN.md",
    '''Lane/fresh/fork agents share one model-facing namespace and durable family authority; a hosted-runtime service owns only fresh/fork provider/runtime/catalog residency, with no parallel child/delegate execution architecture. Shared-history and separately hosted admission both publish durable lane capability selections that may narrow but never exceed the control parent, and recovery re-applies that stored selection to the available executor catalog. The current client snapshot still projects `main`; scoped capability lifecycle beyond this admission boundary remains active work.
''',
    '''Lane/fresh/fork agents share one model-facing namespace and durable family authority; the unified agent host is the sole model-facing publisher, while a hosted-runtime service owns only fresh/fork provider/runtime/catalog residency. There is no parallel child/delegate or lane-only agent tool namespace. Shared-history and separately hosted admission both publish durable lane capability selections that may narrow but never exceed the control parent, and recovery re-applies that stored selection to the available executor catalog. The current client snapshot still projects `main`; scoped capability lifecycle beyond this admission boundary remains active work.
''',
    "design implementation checkpoint",
)
