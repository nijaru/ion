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
    '''\n    pub(crate) fn with_tools(model_ref: impl Into<String>, tools: ToolSelection) -> Self {\n        Self {\n            model_ref: model_ref.into(),\n            tools,\n        }\n    }\n''',
    "",
)

# Store tests use the same total config the crate-private admission primitive now requires.
replace(
    "crates/ion-core/src/tests/agent_store.rs",
    '''            Some(root_entry),\n            "model-a",\n''',
    '''            Some(root_entry),\n            crate::session::lane::Config::new("model-a"),\n''',
    count=2,
)

# Hold the recorded requests while borrowing tool names from the first request.
replace(
    "crates/ion-core/src/tests/agent_family.rs",
    '''    let names = provider.requests()[0]\n        .tools\n        .iter()\n        .map(|tool| tool.name.as_str())\n        .collect::<Vec<_>>();\n''',
    '''    let requests = provider.requests();\n    let names = requests[0]\n        .tools\n        .iter()\n        .map(|tool| tool.name.as_str())\n        .collect::<Vec<_>>();\n''',
)
