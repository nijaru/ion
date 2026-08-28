from pathlib import Path


def replace(path: str, old: str, new: str, count: int = 1) -> None:
    p = Path(path)
    text = p.read_text()
    if text.count(old) < count:
        raise SystemExit(f"anchor missing in {path}: {old[:140]!r}")
    p.write_text(text.replace(old, new, count))


# Keep Config's API minimal: construction with a custom selection has a real
# production owner through mutation of an inherited config, not a second ctor.
replace(
    "crates/ion-core/src/session/lane.rs",
    '''
    pub(crate) fn with_tools(model_ref: impl Into<String>, tools: ToolSelection) -> Self {
        Self {
            model_ref: model_ref.into(),
            tools,
        }
    }
''',
    "",
)

# These are the only bare model literals in the agent-store test; the session
# record uses `.to_owned()`. Convert the two crate-private admission arguments.
replace(
    "crates/ion-core/src/tests/agent_store.rs",
    '"model-a",',
    'crate::session::lane::Config::new("model-a"),',
    count=2,
)

# Hold the recorded requests while borrowing tool names from the first request.
replace(
    "crates/ion-core/src/tests/agent_family.rs",
    '''    let names = provider.requests()[0]
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
''',
    '''    let requests = provider.requests();
    let names = requests[0]
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
''',
)
