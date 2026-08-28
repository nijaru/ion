from pathlib import Path

path = Path('.github/architecture_lane_address_patch.py')
text = path.read_text()
marker = '    "insert entry lane query",\n)'
end = text.index(marker) + len(marker)
start = text.rfind('replace_once(', 0, end)
if start < 0:
    raise SystemExit('could not find broad insert-entry replacement')
replacement = '''replace_once(
    "crates/ion-core/src/store/sql.rs",
    ''' + '"""' + '''    let (leaf_id, config): (Option<String>, String) = connection.query_row(
        "SELECT leaf_id, config FROM lanes WHERE session_id = ?1 AND name = ?2",
        rusqlite::params![session_id.as_uuid().to_string(), crate::session::lane::MAIN],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
''' + '"""' + ''',
    ''' + '"""' + '''    let (leaf_id, config): (Option<String>, String) = connection.query_row(
        "SELECT leaf_id, config FROM lanes WHERE session_id = ?1 AND name = ?2",
        rusqlite::params![session_id.as_uuid().to_string(), lane_name],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
''' + '"""' + ''',
    "insert entry lane query",
)'''
path.write_text(text[:start] + replacement + text[end:])
