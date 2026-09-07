//! Durable revisions for host-owned authority scopes. Reconciliation uses the
//! store writer's transaction; tombstones prevent removal/re-addition from
//! making an old persisted grant authoritative again.

use std::collections::BTreeMap;

use rusqlite::{Connection, params};

use super::StoreError;

pub(super) fn reconcile(
    connection: &mut Connection,
    workspace: &str,
    desired: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, u64>, StoreError> {
    let transaction = connection.transaction()?;
    let existing = {
        let mut statement = transaction.prepare(
            "SELECT scope, revision, definition FROM host_scope_state WHERE workspace = ?1",
        )?;
        statement
            .query_map([workspace], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    (row.get::<_, i64>(1)?, row.get::<_, Option<String>>(2)?),
                ))
            })?
            .collect::<Result<BTreeMap<_, _>, _>>()?
    };
    let mut configured = BTreeMap::new();
    for (scope, definition) in desired {
        let revision = match existing.get(scope) {
            None => 0,
            Some((revision, previous)) => {
                if previous
                    .as_ref()
                    .is_some_and(|previous| previous != definition)
                {
                    next_revision(*revision)?
                } else {
                    *revision
                }
            }
        };
        transaction.execute(
            "INSERT INTO host_scope_state (workspace, scope, revision, definition)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (workspace, scope) DO UPDATE SET revision = excluded.revision,
                 definition = excluded.definition",
            params![workspace, scope, revision, definition],
        )?;
        configured.insert(scope.clone(), revision as u64);
    }
    for (scope, (revision, definition)) in existing {
        if definition.is_some() && !desired.contains_key(&scope) {
            transaction.execute(
                "UPDATE host_scope_state SET revision = ?3, definition = NULL
                 WHERE workspace = ?1 AND scope = ?2",
                params![workspace, scope, next_revision(revision)?],
            )?;
        }
    }
    transaction.commit()?;
    Ok(configured)
}

fn next_revision(revision: i64) -> Result<i64, StoreError> {
    revision
        .checked_add(1)
        .ok_or_else(|| StoreError::Sqlite("host scope revision exhausted".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SessionStore;

    fn desired(definition: &str) -> BTreeMap<String, String> {
        BTreeMap::from([("mcp:peer".into(), definition.into())])
    }

    #[tokio::test]
    async fn host_scopes_removal_readdition_and_changes_never_revive_revisions() {
        let store = SessionStore::open_in_memory().unwrap();
        let reconcile = |definition| store.reconcile_host_scopes("/workspace".into(), definition);
        assert_eq!(reconcile(desired("active:a")).await.unwrap()["mcp:peer"], 0);
        assert!(reconcile(BTreeMap::new()).await.unwrap().is_empty());
        assert!(reconcile(BTreeMap::new()).await.unwrap().is_empty());
        assert_eq!(reconcile(desired("active:a")).await.unwrap()["mcp:peer"], 1);
        assert_eq!(
            reconcile(desired("inactive:a")).await.unwrap()["mcp:peer"],
            2
        );
        assert_eq!(reconcile(desired("active:b")).await.unwrap()["mcp:peer"], 3);
        assert_eq!(
            store
                .reconcile_host_scopes("/other".into(), desired("active:b"))
                .await
                .unwrap()["mcp:peer"],
            0
        );
        store.close().await.unwrap();
    }

    #[tokio::test]
    async fn host_scopes_restart_preserves_unchanged_definitions_and_tombstones() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let store = SessionStore::open(&path).unwrap();
        store
            .reconcile_host_scopes("/workspace".into(), desired("a"))
            .await
            .unwrap();
        store
            .reconcile_host_scopes("/workspace".into(), desired("b"))
            .await
            .unwrap();
        store.close().await.unwrap();
        let store = SessionStore::open(&path).unwrap();
        assert_eq!(
            store
                .reconcile_host_scopes("/workspace".into(), desired("b"))
                .await
                .unwrap()["mcp:peer"],
            1
        );
        store
            .reconcile_host_scopes("/workspace".into(), BTreeMap::new())
            .await
            .unwrap();
        store.close().await.unwrap();
        let store = SessionStore::open(&path).unwrap();
        assert_eq!(
            store
                .reconcile_host_scopes("/workspace".into(), desired("b"))
                .await
                .unwrap()["mcp:peer"],
            2
        );
        store.close().await.unwrap();
    }

    #[tokio::test]
    async fn host_scopes_failed_write_does_not_change_revision() {
        let store = SessionStore::open_in_memory().unwrap();
        store
            .reconcile_host_scopes("/workspace".into(), desired("a"))
            .await
            .unwrap();
        store.fail_next_write();
        assert_eq!(
            store
                .reconcile_host_scopes("/workspace".into(), desired("b"))
                .await
                .unwrap_err(),
            StoreError::Injected
        );
        assert_eq!(
            store
                .reconcile_host_scopes("/workspace".into(), desired("a"))
                .await
                .unwrap()["mcp:peer"],
            0
        );
        store.close().await.unwrap();
    }

    #[test]
    fn host_scopes_transaction_rolls_back_after_a_partial_update() {
        let mut connection = Connection::open_in_memory().unwrap();
        super::super::schema::create_fresh(&mut connection).unwrap();
        reconcile(&mut connection, "/workspace", &desired("a")).unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER refuse_new_scope BEFORE INSERT ON host_scope_state
            WHEN NEW.scope = 'z' BEGIN SELECT RAISE(ABORT, 'injected second write failure'); END;",
            )
            .unwrap();
        let changed = BTreeMap::from([("mcp:peer".into(), "b".into()), ("z".into(), "c".into())]);
        assert!(reconcile(&mut connection, "/workspace", &changed).is_err());
        assert_eq!(
            reconcile(&mut connection, "/workspace", &desired("a")).unwrap()["mcp:peer"],
            0
        );
    }
}
