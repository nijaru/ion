from pathlib import Path


def replace(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one match, found {count}: {old[:100]!r}")
    p.write_text(text.replace(old, new, 1))


replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''            let mut loaded = self.store.load(self.session_id).await?;
            let published = self.tools.published_scopes();
            for lane in &mut loaded.lanes {
                let materialized = lane.config.scopes.materialize(&published);
                let inserted = lane.config.scopes.insert(scope.to_owned());
                if materialized || inserted {
                    self.store
                        .set_lane_config(self.session_id, &lane.name, lane.config.clone())
                        .await?;
                }
            }
            return Ok(());
''',
    '''            let mut loaded = self.store.load(self.session_id).await?;
            let published = self.tools.published_scopes();
            for lane in &mut loaded.lanes {
                let materialized = lane.config.scopes.materialize(&published);
                let inserted = lane.name == crate::session::lane::MAIN
                    && lane.config.scopes.insert(scope.to_owned());
                if materialized || inserted {
                    self.store
                        .set_lane_config(self.session_id, &lane.name, lane.config.clone())
                        .await?;
                }
            }
            return Ok(());
''',
)

replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''        let lane_names = self.lanes.keys().cloned().collect::<Vec<_>>();
        for lane_name in lane_names {
            let mut config = self
                .lane(&lane_name)
                .expect("resident lane remains available")
                .config
                .clone();
            if !config.scopes.insert(scope.clone()) {
                continue;
            }
            self.store
                .set_lane_config(self.session_id, &lane_name, config.clone())
                .await
                .map_err(persistence_command_error)?;
            self.lane_mut(&lane_name)
                .expect("persisted lane remains resident")
                .config = config;
        }
        Ok(())
''',
    '''        let lane_name = crate::session::lane::MAIN;
        let mut config = self
            .lane(lane_name)
            .expect("root structural scope admission requires the main lane")
            .config
            .clone();
        if !config.scopes.insert(scope) {
            return Ok(());
        }
        self.store
            .set_lane_config(self.session_id, lane_name, config.clone())
            .await
            .map_err(persistence_command_error)?;
        self.lane_mut(lane_name)
            .expect("persisted main lane remains resident")
            .config = config;
        Ok(())
''',
)

replace(
    "crates/ion-core/src/agent_host.rs",
    '''    runtime.admit_structural_scope(AGENT_SCOPE).await?;
    catalog.register_structural_scope(AGENT_SCOPE, agent_host_tools(family, hosted));
    Ok(())
''',
    '''    catalog.register_structural_scope(AGENT_SCOPE, agent_host_tools(family, hosted));
    runtime.admit_structural_scope(AGENT_SCOPE).await?;
    Ok(())
''',
)

print("step7 child-scope/order fix applied")
