from pathlib import Path

# Store: add a targeted family-member lookup. The family-wide load remains the
# attach/recovery validation path; ongoing status reads validate only the
# addressed session instead of loading every descendant transcript.
p = Path("crates/ion-core/src/store/mod.rs")
text = p.read_text()
old = '''    LoadAgentFamily {\n        family_session_id: SessionId,\n        reply: oneshot::Sender<Result<Vec<AgentRecord>, StoreError>>,\n    },\n    LatestSession {\n'''
new = '''    LoadAgentFamily {\n        family_session_id: SessionId,\n        reply: oneshot::Sender<Result<Vec<AgentRecord>, StoreError>>,\n    },\n    LoadFamilyAgent {\n        family_session_id: SessionId,\n        agent_id: AgentId,\n        reply: oneshot::Sender<Result<Option<AgentRecord>, StoreError>>,\n    },\n    LatestSession {\n'''
if old not in text:
    raise SystemExit("LoadAgentFamily command anchor missing")
text = text.replace(old, new, 1)
old = '''    pub(crate) async fn load_agent_family(\n        &self,\n        family_session_id: SessionId,\n    ) -> Result<Vec<AgentRecord>, StoreError> {\n        self.request(|reply| StoreCommand::LoadAgentFamily {\n            family_session_id,\n            reply,\n        })\n        .await\n    }\n\n    /// Token usage rows recorded for one session (DESIGN.md §27.2).\n'''
new = '''    pub(crate) async fn load_agent_family(\n        &self,\n        family_session_id: SessionId,\n    ) -> Result<Vec<AgentRecord>, StoreError> {\n        self.request(|reply| StoreCommand::LoadAgentFamily {\n            family_session_id,\n            reply,\n        })\n        .await\n    }\n\n    /// Resolve one semantic address inside a family and validate the session\n    /// it targets. `None` means the identity is not a member of this family.\n    pub(crate) async fn load_family_agent(\n        &self,\n        family_session_id: SessionId,\n        agent_id: AgentId,\n    ) -> Result<Option<AgentRecord>, StoreError> {\n        self.request(|reply| StoreCommand::LoadFamilyAgent {\n            family_session_id,\n            agent_id,\n            reply,\n        })\n        .await\n    }\n\n    /// Token usage rows recorded for one session (DESIGN.md §27.2).\n'''
if old not in text:
    raise SystemExit("load_agent_family method anchor missing")
p.write_text(text.replace(old, new, 1))

p = Path("crates/ion-core/src/store/sql.rs")
text = p.read_text()
old = '''        StoreCommand::LoadAgentFamily {\n            family_session_id,\n            reply,\n        } => {\n            let _ = reply.send(load_agent_family(connection, family_session_id));\n        }\n        StoreCommand::Usage { session_id, reply } => {\n'''
new = '''        StoreCommand::LoadAgentFamily {\n            family_session_id,\n            reply,\n        } => {\n            let _ = reply.send(load_agent_family(connection, family_session_id));\n        }\n        StoreCommand::LoadFamilyAgent {\n            family_session_id,\n            agent_id,\n            reply,\n        } => {\n            let _ = reply.send(load_family_agent(connection, family_session_id, agent_id));\n        }\n        StoreCommand::Usage { session_id, reply } => {\n'''
if old not in text:
    raise SystemExit("LoadAgentFamily handler anchor missing")
text = text.replace(old, new, 1)
anchor = '''fn load_agent_family(\n    connection: &Connection,\n    family_session_id: SessionId,\n) -> Result<Vec<AgentRecord>, StoreError> {\n'''
if anchor not in text:
    raise SystemExit("load_agent_family function anchor missing")
lookup_fn = '''fn load_family_agent(\n    connection: &Connection,\n    family_session_id: SessionId,\n    agent_id: AgentId,\n) -> Result<Option<AgentRecord>, StoreError> {\n    let raw_session: Option<String> = connection\n        .query_row(\n            "SELECT session_id FROM agents WHERE id = ?1 AND family_session_id = ?2",\n            rusqlite::params![\n                agent_id.as_uuid().to_string(),\n                family_session_id.as_uuid().to_string(),\n            ],\n            |row| row.get(0),\n        )\n        .optional()?;\n    let Some(raw_session) = raw_session else {\n        return Ok(None);\n    };\n    let session_id = SessionId::parse(&raw_session).ok_or_else(|| {\n        StoreError::Sqlite(format!("agent {agent_id} has a corrupt target session id"))\n    })?;\n    let loaded = load(connection, session_id)?;\n    let agent = loaded\n        .agents\n        .into_iter()\n        .find(|agent| agent.id == agent_id && agent.family_session_id == family_session_id)\n        .ok_or_else(|| {\n            StoreError::Sqlite(format!(\n                "agent {agent_id} is missing from its addressed session"\n            ))\n        })?;\n    Ok(Some(agent))\n}\n\n'''
text = text.replace(anchor, lookup_fn + anchor, 1)
p.write_text(text)

# Family: validate the complete family on attach, use targeted lookup for
# routing, and make durable observation/result extraction span both shared
# lanes and separately hosted fresh/fork sessions.
p = Path("crates/ion-core/src/agent.rs")
text = p.read_text()
old = '''        let loaded = store.load(session_id).await?;\n        let root = AgentId::root(session_id);\n        let retained = loaded\n'''
new = '''        let loaded = store.load(session_id).await?;\n        let family_agents = store.load_agent_family(session_id).await?;\n        let root = AgentId::root(session_id);\n        if !family_agents.iter().any(|agent| agent.id == root) {\n            return Err(Error::Inconsistent(root));\n        }\n        let retained = loaded\n'''
if old not in text:
    raise SystemExit("Family attach load anchor missing")
text = text.replace(old, new, 1)
old = '''    pub(crate) async fn target(&self, agent_id: AgentId) -> Result<AgentTarget, Error> {\n        let agents = self.store.load_agent_family(self.session_id).await?;\n        let agent = agents\n            .iter()\n            .find(|agent| agent.id == agent_id)\n            .ok_or(Error::UnknownAgent(agent_id))?;\n        match &agent.history {\n'''
new = '''    pub(crate) async fn target(&self, agent_id: AgentId) -> Result<AgentTarget, Error> {\n        let agent = self\n            .store\n            .load_family_agent(self.session_id, agent_id)\n            .await?\n            .ok_or(Error::UnknownAgent(agent_id))?;\n        match &agent.history {\n'''
if old not in text:
    raise SystemExit("Family target anchor missing")
text = text.replace(old, new, 1)
# Reference fields now live on an owned record, not &record.
text = text.replace("if agent.session_id == self.session_id =>", "if agent.session_id == self.session_id =>", 1)
old = '''    /// Observe authoritative durable operation state for one retained agent.\n    /// Reading a terminal/suspended state also releases any stale local permit;\n    /// it never consumes or deletes the durable completion.\n    pub async fn status(&self, agent_id: AgentId) -> Result<Status, Error> {\n        if !self\n            .retained\n            .lock()\n            .expect("agent family poisoned")\n            .contains_key(&agent_id)\n        {\n            return Err(Error::UnknownAgent(agent_id));\n        }\n        let loaded = self.store.load(self.session_id).await?;\n        self.release_nonexecuting(&loaded);\n        status_from_loaded(&loaded, agent_id)\n    }\n\n    /// Observe the latest durable execution and its exact semantic result.\n    pub async fn observe(&self, agent_id: AgentId) -> Result<Observation, Error> {\n        if !self\n            .retained\n            .lock()\n            .expect("agent family poisoned")\n            .contains_key(&agent_id)\n        {\n            return Err(Error::UnknownAgent(agent_id));\n        }\n        let loaded = self.store.load(self.session_id).await?;\n        self.release_nonexecuting(&loaded);\n        let status = status_from_loaded(&loaded, agent_id)?;\n        let result = match status_operation_id(&status) {\n            Some(operation_id) => operation_result(&loaded, agent_id, operation_id)?,\n            None => None,\n        };\n        Ok(Observation {\n            agent_id,\n            status,\n            result,\n        })\n    }\n\n    /// Observe one captured operation even if the agent has since started a\n    /// later run. This is the wait/result boundary used by model-facing tools.\n    pub async fn observe_operation(\n        &self,\n        agent_id: AgentId,\n        operation_id: OperationId,\n    ) -> Result<Observation, Error> {\n        let loaded = self.store.load(self.session_id).await?;\n        self.release_nonexecuting(&loaded);\n        let status = status_for_operation(&loaded, agent_id, operation_id)?;\n        let result = operation_result(&loaded, agent_id, operation_id)?;\n        Ok(Observation {\n            agent_id,\n            status,\n            result,\n        })\n    }\n'''
new = '''    /// Observe authoritative durable operation state for any retained family\n    /// agent. Shared-history permit cleanup remains local to the root runtime;\n    /// separately hosted residency is still owned by the child host for now.\n    pub async fn status(&self, agent_id: AgentId) -> Result<Status, Error> {\n        let loaded = self.load_addressed_session(agent_id).await?;\n        status_from_loaded(&loaded, agent_id)\n    }\n\n    /// Observe the latest durable execution and its exact semantic result for\n    /// either shared-history or separately hosted topology.\n    pub async fn observe(&self, agent_id: AgentId) -> Result<Observation, Error> {\n        let loaded = self.load_addressed_session(agent_id).await?;\n        let status = status_from_loaded(&loaded, agent_id)?;\n        let result = match status_operation_id(&status) {\n            Some(operation_id) => operation_result(&loaded, agent_id, operation_id)?,\n            None => None,\n        };\n        Ok(Observation {\n            agent_id,\n            status,\n            result,\n        })\n    }\n\n    /// Observe one captured operation even if the agent has since started a\n    /// later run. This exact durable result boundary spans all current family\n    /// topologies; waiting/execution residency remains topology-specific.\n    pub async fn observe_operation(\n        &self,\n        agent_id: AgentId,\n        operation_id: OperationId,\n    ) -> Result<Observation, Error> {\n        let loaded = self.load_addressed_session(agent_id).await?;\n        let status = status_for_operation(&loaded, agent_id, operation_id)?;\n        let result = operation_result(&loaded, agent_id, operation_id)?;\n        Ok(Observation {\n            agent_id,\n            status,\n            result,\n        })\n    }\n\n    async fn load_addressed_session(&self, agent_id: AgentId) -> Result<LoadedSession, Error> {\n        let target = self.target(agent_id).await?;\n        let session_id = match target {\n            AgentTarget::SharedHistory { session_id }\n            | AgentTarget::SeparateSession { session_id } => session_id,\n        };\n        let loaded = self.store.load(session_id).await?;\n        if matches!(target, AgentTarget::SharedHistory { .. }) {\n            self.release_nonexecuting(&loaded);\n        }\n        Ok(loaded)\n    }\n'''
if old not in text:
    raise SystemExit("Family observation block anchor missing")
text = text.replace(old, new, 1)
# Family cancel must never route a separately hosted operation id through the
# root SessionHandle now that status spans sessions.
old = '''    pub async fn cancel(&self, agent_id: AgentId) -> Result<(), Error> {\n        let status = self.status(agent_id).await?;\n        let operation_id = match status {\n'''
new = '''    pub async fn cancel(&self, agent_id: AgentId) -> Result<(), Error> {\n        if !matches!(\n            self.target(agent_id).await?,\n            AgentTarget::SharedHistory { .. }\n        ) {\n            return Err(Error::UnknownAgent(agent_id));\n        }\n        let status = self.status(agent_id).await?;\n        let operation_id = match status {\n'''
if old not in text:
    raise SystemExit("Family cancel anchor missing")
text = text.replace(old, new, 1)
p.write_text(text)

# Unified host: status/result rendering comes from Family for every topology.
# ChildManager remains only the event/cancel/resume runtime host for separate
# sessions.
p = Path("crates/ion-core/src/delegate.rs")
text = p.read_text()
old = '''                HostAgentToolKind::Status => {\n                    let agent_id = match parse_host_agent_handle(&arguments) {\n                        Ok(agent_id) => agent_id,\n                        Err(err) => return ToolOutcome::error(err),\n                    };\n                    match self.family.target(agent_id).await {\n                        Ok(crate::agent::AgentTarget::SharedHistory { .. }) => {\n                            match self.family.observe(agent_id).await {\n                                Ok(observation) => {\n                                    ToolOutcome::text(render_family_observation(&observation))\n                                }\n                                Err(err) => ToolOutcome::error(err.to_string()),\n                            }\n                        }\n                        Ok(crate::agent::AgentTarget::SeparateSession { session_id }) => {\n                            let handle = child_handle_for_session(session_id, self.children.parent_id);\n                            match self.children.observe(handle).await {\n                                Ok(observation) => {\n                                    ToolOutcome::text(render_child_as_agent(agent_id, &observation))\n                                }\n                                Err(err) => ToolOutcome::error(err),\n                            }\n                        }\n                        Err(err) => ToolOutcome::error(err.to_string()),\n                    }\n                }\n'''
new = '''                HostAgentToolKind::Status => {\n                    let agent_id = match parse_host_agent_handle(&arguments) {\n                        Ok(agent_id) => agent_id,\n                        Err(err) => return ToolOutcome::error(err),\n                    };\n                    match self.family.observe(agent_id).await {\n                        Ok(observation) => {\n                            ToolOutcome::text(render_family_observation(&observation))\n                        }\n                        Err(err) => ToolOutcome::error(err.to_string()),\n                    }\n                }\n'''
if old not in text:
    raise SystemExit("Host Status topology block missing")
text = text.replace(old, new, 1)
# For separate-session wait, ChildManager only waits; Family re-reads the exact
# operation result after the terminal durable transition.
old = '''                        Ok(crate::agent::AgentTarget::SeparateSession { session_id }) => {\n                            let handle = child_handle_for_session(session_id, self.children.parent_id);\n                            match self.children.wait(handle, cancel, progress.as_ref()).await {\n                                Ok(observation) => {\n                                    ToolOutcome::text(render_child_as_agent(agent_id, &observation))\n                                }\n                                Err(err) => ToolOutcome::error(err),\n                            }\n                        }\n'''
new = '''                        Ok(crate::agent::AgentTarget::SeparateSession { session_id }) => {\n                            let handle = child_handle_for_session(session_id, self.children.parent_id);\n                            match self.children.wait(handle, cancel, progress.as_ref()).await {\n                                Ok(observation) => match observation.operation_id() {\n                                    Some(operation_id) => match self\n                                        .family\n                                        .observe_operation(agent_id, operation_id)\n                                        .await\n                                    {\n                                        Ok(observation) => ToolOutcome::text(\n                                            render_family_observation(&observation),\n                                        ),\n                                        Err(err) => ToolOutcome::error(err.to_string()),\n                                    },\n                                    None => ToolOutcome::text(render_child_as_agent(\n                                        agent_id,\n                                        &observation,\n                                    )),\n                                },\n                                Err(err) => ToolOutcome::error(err),\n                            }\n                        }\n'''
if old not in text:
    raise SystemExit("Host separate Wait block missing")
text = text.replace(old, new, 1)
# Resume still performs runtime reattachment in ChildManager, then Family owns
# the durable externally reported observation.
old = '''                            match self\n                                .children\n                                .resume(handle, cancel, progress.as_ref())\n                                .await\n                            {\n                                Ok(observation) => {\n                                    ToolOutcome::text(render_child_as_agent(agent_id, &observation))\n                                }\n                                Err(err) => ToolOutcome::error(err),\n                            }\n'''
new = '''                            match self\n                                .children\n                                .resume(handle, cancel, progress.as_ref())\n                                .await\n                            {\n                                Ok(_) => match self.family.observe(agent_id).await {\n                                    Ok(observation) => {\n                                        ToolOutcome::text(render_family_observation(&observation))\n                                    }\n                                    Err(err) => ToolOutcome::error(err.to_string()),\n                                },\n                                Err(err) => ToolOutcome::error(err),\n                            }\n'''
if old not in text:
    raise SystemExit("Host Resume observation block missing")
text = text.replace(old, new, 1)
p.write_text(text)

# Regression coverage: Family can read fresh/fork durable results after the
# wait path has released live child residency.
p = Path("crates/ion-core/src/tests/agent_family.rs")
text = p.read_text()
old = '''    assert!(!fresh_wait.output.contains("child "), "{fresh_wait:?}");\n\n    let fork = spawn(json!({"objective": "fork work", "topology": "fork"})).await;\n'''
new = '''    assert!(!fresh_wait.output.contains("child "), "{fresh_wait:?}");\n    let fresh_observation = family\n        .observe(fresh_agent)\n        .await\n        .expect("family observes fresh result");\n    assert!(matches!(\n        fresh_observation.status,\n        crate::agent::Status::Finished { .. }\n    ));\n    assert_eq!(fresh_observation.result.as_deref(), Some("session agent answer"));\n\n    let fork = spawn(json!({"objective": "fork work", "topology": "fork"})).await;\n'''
if old not in text:
    raise SystemExit("fresh observation test anchor missing")
text = text.replace(old, new, 1)
old = '''    assert!(\n        fork_wait.output.contains("session agent answer"),\n        "{fork_wait:?}"\n    );\n\n    children.close().await.expect("close session agents");\n'''
new = '''    assert!(\n        fork_wait.output.contains("session agent answer"),\n        "{fork_wait:?}"\n    );\n    let fork_observation = family\n        .observe(fork_agent)\n        .await\n        .expect("family observes fork result");\n    assert!(matches!(\n        fork_observation.status,\n        crate::agent::Status::Finished { .. }\n    ));\n    assert_eq!(fork_observation.result.as_deref(), Some("session agent answer"));\n\n    children.close().await.expect("close session agents");\n'''
if old not in text:
    raise SystemExit("fork observation test anchor missing")
p.write_text(text.replace(old, new, 1))
