from pathlib import Path
import re

ROOT = Path("crates/ion-core/src")

# Pre-1.0 rename: "parent" on a session is specifically control lineage, not
# history inheritance. Apply this across core/tests first so stale vocabulary
# becomes a compiler error rather than coexisting with the new fields.
for p in ROOT.rglob("*.rs"):
    text = p.read_text()
    text = text.replace("parent_session_id", "control_parent_session_id")
    p.write_text(text)

# Session records carry control and history lineage independently.
p = Path("crates/ion-core/src/store/mod.rs")
text = p.read_text()
old = '''    /// Present for bounded child sessions (§20.3): lineage is durable\n    /// before the child runs.\n    pub control_parent_session_id: Option<SessionId>,\n'''
new = '''    /// Control parent for a separately hosted descendant session. This is\n    /// cancellation/ownership lineage, not conversation-history inheritance.\n    pub control_parent_session_id: Option<SessionId>,\n    /// Explicit history source for a separately hosted fork. `Some(session)`\n    /// with no entry records a fork of an empty source session. Fresh children\n    /// keep both fork fields `None`.\n    pub fork_source_session_id: Option<SessionId>,\n    pub fork_source_entry_id: Option<EntryId>,\n'''
if old not in text:
    raise SystemExit("SessionRecord lineage field anchor missing")
p.write_text(text.replace(old, new, 1))

# Add default fork fields to every SessionRecord constructor except the type
# definition. The runtime constructor is specialized below.
for p in ROOT.rglob("*.rs"):
    if p == Path("crates/ion-core/src/store/mod.rs"):
        continue
    text = p.read_text()
    matches = list(re.finditer(r"SessionRecord\s*\{(?P<body>.*?)\n(?P<indent>\s*)\}", text, re.S))
    for m in reversed(matches):
        body = m.group("body")
        if "control_parent_session_id:" not in body or "fork_source_session_id:" in body:
            continue
        line_match = list(re.finditer(r"(?m)^(\s*)control_parent_session_id:\s*[^\n]+,\s*$", body))
        if not line_match:
            continue
        lm = line_match[-1]
        indent = lm.group(1)
        insertion = f"\n{indent}fork_source_session_id: None,\n{indent}fork_source_entry_id: None,"
        body2 = body[: lm.end()] + insertion + body[lm.end():]
        text = text[: m.start("body")] + body2 + text[m.end("body"):]
    p.write_text(text)

# Schema v21: control lineage and optional fork history source are independent.
p = Path("crates/ion-core/src/store/schema.rs")
text = p.read_text().replace("const SCHEMA_VERSION: i64 = 20;", "const SCHEMA_VERSION: i64 = 21;")
old = '''    title TEXT NOT NULL,\n    control_parent_session_id TEXT\n);\n'''
new = '''    title TEXT NOT NULL,\n    control_parent_session_id TEXT REFERENCES sessions(id),\n    fork_source_session_id TEXT REFERENCES sessions(id),\n    fork_source_entry_id TEXT,\n    FOREIGN KEY (fork_source_session_id, fork_source_entry_id)\n        REFERENCES entries(session_id, id),\n    CHECK (fork_source_entry_id IS NULL OR fork_source_session_id IS NOT NULL)\n);\n'''
if old not in text:
    raise SystemExit("sessions schema anchor missing")
p.write_text(text.replace(old, new, 1))

# Store create/load round-trips all three fields, rejecting corrupt IDs instead
# of silently turning malformed lineage into None.
p = Path("crates/ion-core/src/store/sql.rs")
text = p.read_text()
old = '''        "INSERT INTO sessions (id, created_at, updated_at, cwd, title, control_parent_session_id)\n         VALUES (?1, ?2, ?2, ?3, ?4, ?5)",\n        rusqlite::params![\n            record.id.as_uuid().to_string(),\n            now,\n            record.cwd,\n            record.title,\n            record.control_parent_session_id.map(|id| id.as_uuid().to_string()),\n        ],\n'''
new = '''        "INSERT INTO sessions (\n            id, created_at, updated_at, cwd, title, control_parent_session_id,\n            fork_source_session_id, fork_source_entry_id\n         ) VALUES (?1, ?2, ?2, ?3, ?4, ?5, ?6, ?7)",\n        rusqlite::params![\n            record.id.as_uuid().to_string(),\n            now,\n            record.cwd,\n            record.title,\n            record.control_parent_session_id.map(|id| id.as_uuid().to_string()),\n            record.fork_source_session_id.map(|id| id.as_uuid().to_string()),\n            record.fork_source_entry_id.map(|id| id.as_uuid().to_string()),\n        ],\n'''
if old not in text:
    raise SystemExit("create_session insert anchor missing")
text = text.replace(old, new, 1)
old = '''    let id = session_id.as_uuid().to_string();\n    let (cwd, title, control_parent_session_id): (String, String, Option<String>) = connection\n        .query_row(\n            "SELECT cwd, title, control_parent_session_id FROM sessions WHERE id = ?1",\n            rusqlite::params![id],\n            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),\n        )\n        .map_err(|err| match err {\n            rusqlite::Error::QueryReturnedNoRows => StoreError::NotFound(session_id),\n            other => StoreError::from(other),\n        })?;\n'''
new = '''    let id = session_id.as_uuid().to_string();\n    let (cwd, title, raw_control_parent, raw_fork_session, raw_fork_entry): (\n        String,\n        String,\n        Option<String>,\n        Option<String>,\n        Option<String>,\n    ) = connection\n        .query_row(\n            "SELECT cwd, title, control_parent_session_id, fork_source_session_id,\n                    fork_source_entry_id\n             FROM sessions WHERE id = ?1",\n            rusqlite::params![id],\n            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),\n        )\n        .map_err(|err| match err {\n            rusqlite::Error::QueryReturnedNoRows => StoreError::NotFound(session_id),\n            other => StoreError::from(other),\n        })?;\n    let parse_session_lineage = |raw: Option<String>, label: &str| {\n        raw.map(|value| {\n            SessionId::parse(&value).ok_or_else(|| {\n                StoreError::Sqlite(format!("session has corrupt {label} id"))\n            })\n        })\n        .transpose()\n    };\n    let control_parent_session_id =\n        parse_session_lineage(raw_control_parent, "control parent")?;\n    let fork_source_session_id = parse_session_lineage(raw_fork_session, "fork source session")?;\n    let fork_source_entry_id = raw_fork_entry\n        .map(|value| {\n            EntryId::parse(&value)\n                .ok_or_else(|| StoreError::Sqlite("session has corrupt fork source entry id".to_owned()))\n        })\n        .transpose()?;\n    if fork_source_entry_id.is_some() && fork_source_session_id.is_none() {\n        return Err(StoreError::Sqlite(\n            "session fork source entry has no source session".to_owned(),\n        ));\n    }\n'''
if old not in text:
    raise SystemExit("load session row anchor missing")
text = text.replace(old, new, 1)
old = '''        control_parent_session_id: control_parent_session_id.and_then(|text| SessionId::parse(&text)),\n        fork_source_session_id: None,\n        fork_source_entry_id: None,\n'''
new = '''        control_parent_session_id,\n        fork_source_session_id,\n        fork_source_entry_id,\n'''
if old not in text:
    raise SystemExit("loaded SessionRecord lineage anchor missing")
text = text.replace(old, new, 1)
p.write_text(text)

# Runtime composition carries the two lineage dimensions separately into the
# one durable SessionRecord creation boundary.
p = Path("crates/ion-core/src/runtime/mod.rs")
text = p.read_text()
text = text.replace(
    '''    parent: Option<SessionId>,\n    trusted_resources: Vec<TrustedResource>,\n''',
    '''    parent: Option<SessionId>,\n    fork_source: Option<(SessionId, Option<EntryId>)>,\n    trusted_resources: Vec<TrustedResource>,\n''',
    1,
)
text = text.replace(
    '''            parent: None,\n            trusted_resources: Vec::new(),\n''',
    '''            parent: None,\n            fork_source: None,\n            trusted_resources: Vec::new(),\n''',
    1,
)
text = text.replace(
    '''                    budget: self.budget,\n                    parent: self.parent,\n                },\n''',
    '''                    budget: self.budget,\n                    parent: self.parent,\n                    fork_source: self.fork_source,\n                },\n''',
    1,
)
# Child constructor explicitly receives history lineage instead of inferring it
# from control parentage.
old = '''        budget: RuntimeBudget,\n        parent: SessionId,\n        trusted_resources: Vec<TrustedResource>,\n    ) -> Self {\n'''
new = '''        budget: RuntimeBudget,\n        parent: SessionId,\n        fork_source: Option<(SessionId, Option<EntryId>)>,\n        trusted_resources: Vec<TrustedResource>,\n    ) -> Self {\n'''
if old not in text:
    raise SystemExit("start_child signature anchor missing")
text = text.replace(old, new, 1)
text = text.replace(
    '''        composition.parent = Some(parent);\n        composition.trusted_resources = trusted_resources;\n''',
    '''        composition.parent = Some(parent);\n        composition.fork_source = fork_source;\n        composition.trusted_resources = trusted_resources;\n''',
    1,
)
# Reopened child validates control lineage and retains durable fork lineage in
# runtime composition even though the session row will not be rewritten.
text = text.replace(
    '''        if loaded.session.control_parent_session_id != Some(config.parent) {\n''',
    '''        if loaded.session.control_parent_session_id != Some(config.parent) {\n''',
    1,
)
text = text.replace(
    '''        let mut composition = Composition::new(provider, tools, store);\n        composition.policy = config.policy;\n        composition.budget = config.budget;\n        composition.parent = Some(config.parent);\n        composition.trusted_resources = config.trusted_resources;\n''',
    '''        let fork_source = loaded\n            .session\n            .fork_source_session_id\n            .map(|source| (source, loaded.session.fork_source_entry_id));\n        let mut composition = Composition::new(provider, tools, store);\n        composition.policy = config.policy;\n        composition.budget = config.budget;\n        composition.parent = Some(config.parent);\n        composition.fork_source = fork_source;\n        composition.trusted_resources = config.trusted_resources;\n''',
    1,
)
# Session deps/runtime fields.
text = text.replace(
    '''    /// Durable lineage for bounded child sessions (§20.3).\n    parent: Option<SessionId>,\n}\n''',
    '''    /// Durable control lineage for separately hosted descendants.\n    parent: Option<SessionId>,\n    /// Explicit history lineage; independent from control parentage.\n    fork_source: Option<(SessionId, Option<EntryId>)>,\n}\n''',
    1,
)
text = text.replace(
    '''    control_parent_session_id: Option<SessionId>,\n    commands: mpsc::Receiver<SessionCommand>,\n''',
    '''    control_parent_session_id: Option<SessionId>,\n    fork_source_session_id: Option<SessionId>,\n    fork_source_entry_id: Option<EntryId>,\n    commands: mpsc::Receiver<SessionCommand>,\n''',
    1,
)
text = text.replace(
    '''            budget,\n            parent,\n        } = deps;\n''',
    '''            budget,\n            parent,\n            fork_source,\n        } = deps;\n''',
    1,
)
text = text.replace(
    '''            budget,\n            control_parent_session_id: parent,\n            commands,\n''',
    '''            budget,\n            control_parent_session_id: parent,\n            fork_source_session_id: fork_source.map(|(session, _)| session),\n            fork_source_entry_id: fork_source.and_then(|(_, entry)| entry),\n            commands,\n''',
    1,
)
# Specialize the runtime SessionRecord constructor (auto-defaulted above).
text = text.replace(
    '''                control_parent_session_id: self.control_parent_session_id,\n                fork_source_session_id: None,\n                fork_source_entry_id: None,\n''',
    '''                control_parent_session_id: self.control_parent_session_id,\n                fork_source_session_id: self.fork_source_session_id,\n                fork_source_entry_id: self.fork_source_entry_id,\n''',
    1,
)
p.write_text(text)

# Separate-session child handles also name control lineage explicitly.
p = Path("crates/ion-core/src/delegate.rs")
text = p.read_text()
text = text.replace("parent_session_id", "control_parent_session_id")

# Compute fork history from exactly the parent's main branch and retain the
# source leaf used for that projection.
old = '''async fn fork_context(\n    store: &SessionStore,\n    parent_id: SessionId,\n) -> Result<Option<String>, String> {\n    let loaded = store\n        .load(parent_id)\n        .await\n        .map_err(|err| format!("could not load parent context: {err}"))?;\n    if loaded.entries.is_empty() {\n        return Ok(None);\n    }\n    let first_seq = loaded.entries.first().map_or(1, |record| record.seq);\n    let plan = project(loaded.entries.iter().map(|record| &record.entry), first_seq);\n    Ok(Some(render_fork_context(&plan)))\n}\n'''
new = '''struct ForkContext {\n    rendered: Option<String>,\n    source_entry_id: Option<crate::ids::EntryId>,\n}\n\nasync fn fork_context(\n    store: &SessionStore,\n    parent_id: SessionId,\n) -> Result<ForkContext, String> {\n    let loaded = store\n        .load(parent_id)\n        .await\n        .map_err(|err| format!("could not load parent context: {err}"))?;\n    let main = loaded\n        .lanes\n        .iter()\n        .find(|lane| lane.name == crate::session::lane::MAIN)\n        .ok_or_else(|| "parent session has no main lane".to_owned())?;\n    let source_entry_id = main.state.leaf;\n    let Some(mut cursor) = source_entry_id else {\n        return Ok(ForkContext {\n            rendered: None,\n            source_entry_id: None,\n        });\n    };\n    let index = loaded\n        .entries\n        .iter()\n        .map(|record| (record.id, record))\n        .collect::<HashMap<_, _>>();\n    let mut branch = Vec::new();\n    loop {\n        let record = index.get(&cursor).copied().ok_or_else(|| {\n            format!("parent main branch references missing entry {cursor}")\n        })?;\n        branch.push(record);\n        let Some(parent) = record.parent else {\n            break;\n        };\n        cursor = parent;\n    }\n    branch.reverse();\n    let first_seq = branch.first().map_or(1, |record| record.seq);\n    let plan = project(branch.iter().map(|record| &record.entry), first_seq);\n    Ok(ForkContext {\n        rendered: Some(render_fork_context(&plan)),\n        source_entry_id,\n    })\n}\n'''
if old not in text:
    raise SystemExit("fork_context implementation anchor missing")
text = text.replace(old, new, 1)

# Manager spawn: Fresh has no history source; ForkContext records the exact
# source boundary used to render the child seed.
old = '''        let fork_context = match spec.context_mode {\n            ChildContextMode::Fresh => None,\n            ChildContextMode::ForkContext => fork_context(&self.config.store, self.parent_id)\n                .await\n                .map_err(|err| format!("could not load parent context: {err}"))?,\n        };\n        let prompt = compose_child_prompt(&spec, fork_context.as_deref());\n'''
new = '''        let fork_context = match spec.context_mode {\n            ChildContextMode::Fresh => None,\n            ChildContextMode::ForkContext => Some(\n                fork_context(&self.config.store, self.parent_id)\n                    .await\n                    .map_err(|err| format!("could not load parent context: {err}"))?,\n            ),\n        };\n        let prompt = compose_child_prompt(\n            &spec,\n            fork_context.as_ref().and_then(|fork| fork.rendered.as_deref()),\n        );\n        let fork_source = fork_context\n            .as_ref()\n            .map(|fork| (self.parent_id, fork.source_entry_id));\n'''
if old not in text:
    raise SystemExit("managed child fork block anchor missing")
text = text.replace(old, new, 1)
# First production call gets fork_source before trusted resources.
needle = '''            self.config.child_budget,\n            self.parent_id,\n            self.config.trusted_resources.clone(),\n'''
replacement = '''            self.config.child_budget,\n            self.parent_id,\n            fork_source,\n            self.config.trusted_resources.clone(),\n'''
if needle not in text:
    raise SystemExit("managed child runtime call anchor missing")
text = text.replace(needle, replacement, 1)

# Test fixture path has the same exact topology semantics.
old = '''    let fork_context_result = match spec.context_mode {\n        ChildContextMode::Fresh => Ok(None),\n        ChildContextMode::ForkContext => fork_context(&config.store, parent_id).await,\n    };\n    let fork_context = match fork_context_result {\n        Ok(context) => context,\n        Err(err) => return format!("child failed: {err} [child parent: {parent_id}]"),\n    };\n    let prompt = compose_child_prompt(&spec, fork_context.as_deref());\n'''
new = '''    let fork_context_result = match spec.context_mode {\n        ChildContextMode::Fresh => Ok(None),\n        ChildContextMode::ForkContext => fork_context(&config.store, parent_id).await.map(Some),\n    };\n    let fork_context = match fork_context_result {\n        Ok(context) => context,\n        Err(err) => return format!("child failed: {err} [child parent: {parent_id}]"),\n    };\n    let prompt = compose_child_prompt(\n        &spec,\n        fork_context.as_ref().and_then(|fork| fork.rendered.as_deref()),\n    );\n    let fork_source = fork_context\n        .as_ref()\n        .map(|fork| (parent_id, fork.source_entry_id));\n'''
if old not in text:
    raise SystemExit("fixture child fork block anchor missing")
text = text.replace(old, new, 1)
needle = '''        config.child_budget,\n        parent_id,\n        config.trusted_resources.clone(),\n'''
replacement = '''        config.child_budget,\n        parent_id,\n        fork_source,\n        config.trusted_resources.clone(),\n'''
if needle not in text:
    raise SystemExit("fixture child runtime call anchor missing")
text = text.replace(needle, replacement, 1)
p.write_text(text)

# Existing behavior tests now assert the two durable lineage dimensions.
p = Path("crates/ion-core/src/tests/budget_children.rs")
text = p.read_text()
old = '''        assert_eq!(child_loaded.session.control_parent_session_id, Some(parent_id));\n'''
new = '''        assert_eq!(child_loaded.session.control_parent_session_id, Some(parent_id));\n        assert_eq!(child_loaded.session.fork_source_session_id, None);\n        assert_eq!(child_loaded.session.fork_source_entry_id, None);\n'''
if old not in text:
    raise SystemExit("fresh child lineage assertion anchor missing")
text = text.replace(old, new, 1)
old = '''    let child = store.load(child_id).await.expect("child session");\n    assert_eq!(child.session.initial_model_ref, "child-model");\n'''
new = '''    let child = store.load(child_id).await.expect("child session");\n    let parent_main_leaf = parent\n        .lanes\n        .iter()\n        .find(|lane| lane.name == crate::session::lane::MAIN)\n        .expect("parent main lane")\n        .state\n        .leaf;\n    assert_eq!(child.session.control_parent_session_id, Some(parent_id));\n    assert_eq!(child.session.fork_source_session_id, Some(parent_id));\n    assert_eq!(child.session.fork_source_entry_id, parent_main_leaf);\n    assert_eq!(child.session.initial_model_ref, "child-model");\n'''
if old not in text:
    raise SystemExit("fork child lineage assertion anchor missing")
text = text.replace(old, new, 1)
p.write_text(text)
