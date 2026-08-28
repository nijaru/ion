from pathlib import Path


def replace_once(path: str, old: str, new: str, label: str) -> None:
    p = Path(path)
    text = p.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected 1 match, found {count}")
    p.write_text(text.replace(old, new))


replace_once(
    "crates/ion-core/src/operation/mod.rs",
    '''    /// A durably accepted model selection. It applies only to model
    /// steps started after this entry; any in-flight step keeps its
    /// frozen [`ModelConfig`].
    ModelChanged {
        model_ref: String,
    },
''',
    "",
    "remove ModelChanged entry",
)

replace_once(
    "crates/ion-core/src/context.rs",
    '''            SessionEntry::ModelChanged { .. } => {
                // Configuration lineage is canonical session state, not
                // a conversational message.
            }
''',
    "",
    "remove context ModelChanged projection",
)

replace_once(
    "crates/ion/src/acp.rs",
    '''            ion_core::SessionEntry::ToolResult { .. }
            | ion_core::SessionEntry::ModelChanged { .. } => {}
''',
    '''            ion_core::SessionEntry::ToolResult { .. } => {}
''',
    "remove ACP ModelChanged projection",
)

replace_once(
    "crates/ion/src/tui/render.rs",
    '''        ion_core::SessionEntry::ModelChanged { model_ref } => {
            out.push(Line::from(format!("• model → {model_ref}")).style(palette.system_note));
        }
''',
    "",
    "remove TUI ModelChanged rendering",
)

replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''    let (leaf_id, config): (Option<String>, String) = connection.query_row(
        "SELECT leaf_id, config FROM lanes WHERE session_id = ?1 AND name = ?2",
        rusqlite::params![session_id.as_uuid().to_string(), lane_name],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
''',
    '''    let leaf_id: Option<String> = connection.query_row(
        "SELECT leaf_id FROM lanes WHERE session_id = ?1 AND name = ?2",
        rusqlite::params![session_id.as_uuid().to_string(), lane_name],
        |row| row.get(0),
    )?;
''',
    "entry insertion no longer reads lane config",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''    let mut config: crate::session::lane::Config = serde_json::from_str(&config)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
    if let SessionEntry::ModelChanged { model_ref } = &node.value {
        config.model_ref.clone_from(model_ref);
    }
    let config = serde_json::to_string(&config)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;
    let changed = connection.execute(
        "UPDATE lanes SET leaf_id = ?3, config = ?4, updated_at = ?5
         WHERE session_id = ?1 AND name = ?2",
        rusqlite::params![
            session_id.as_uuid().to_string(),
            lane_name,
            node.id.as_uuid().to_string(),
            config,
            now_ms(),
        ],
    )?;
''',
    '''    let changed = connection.execute(
        "UPDATE lanes SET leaf_id = ?3, updated_at = ?4
         WHERE session_id = ?1 AND name = ?2",
        rusqlite::params![
            session_id.as_uuid().to_string(),
            lane_name,
            node.id.as_uuid().to_string(),
            now_ms(),
        ],
    )?;
''',
    "entry insertion no longer mutates config",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''    if let Some(model_ref) = entries.iter().rev().find_map(|entry| match &entry.entry {
        SessionEntry::ModelChanged { model_ref } => Some(model_ref),
        _ => None,
    }) && model_ref != &lane.config.model_ref
    {
        return Err(StoreError::Sqlite(
            "main lane model config disagrees with the migration entry".to_owned(),
        ));
    }

''',
    "",
    "remove legacy load model-entry check",
)
replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''        SessionEntry::ModelChanged { .. } => "model_changed",
''',
    "",
    "remove ModelChanged entry kind",
)
replace_once(
    "crates/ion-core/src/tests/compaction.rs",
    '''        SessionEntry::ModelChanged { .. } => "model_changed",
''',
    "",
    "remove compaction test ModelChanged helper",
)
replace_once(
    "crates/ion-core/src/tests/support.rs",
    '''            crate::SessionEntry::ModelChanged { .. } => "model_changed",
''',
    "",
    "remove support ModelChanged helper",
)
