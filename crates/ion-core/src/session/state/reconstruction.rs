//! Whole-state validation of a session loaded from durable records.
//!
//! Opening a session loads records written by an earlier process, a damaged
//! database or an older build. Parsing individual records does not re-establish
//! the writer's invariants, so the complete state is checked before a writable
//! owner exists. A failure refuses the session instead of repairing it: guessing a
//! sequence or rewriting evidence would discard the very fact that the store is no
//! longer trustworthy.
//!
//! This is a child module of `state`, so it validates the resident records and
//! private indexes directly instead of through a second accessor surface.

use super::{SessionState, StateError};
use crate::{CommitSeq, ConversationId, LocalSeq, TaskStatus};

impl SessionState {
    /// Reject a reconstructed state that no valid commit sequence could have
    /// produced.
    ///
    /// Opening a session loads records written by an earlier process, a damaged
    /// database or an older build. Parsing individual records does not
    /// re-establish the writer's invariants, so the whole state is checked before
    /// a writable owner exists. A failure refuses the session instead of repairing
    /// it: guessing a sequence or rewriting evidence would discard the very
    /// fact that the store is no longer trustworthy.
    pub(crate) fn validate_reconstruction(&self) -> Result<(), StateError> {
        self.validate_sequence_bounds()?;
        self.validate_conversations()?;
        self.validate_configs()?;
        self.validate_entries()?;
        self.validate_inputs()?;
        self.validate_tasks()
    }

    /// A configuration belongs to a conversation that exists, always carries the
    /// commit that installed it, and is still valid under this build's rules.
    ///
    /// The last check matters because configuration decides what a generation is
    /// allowed to spend and send: a stored record this build would refuse is not
    /// repaired, it makes the session untrustworthy, so opening it is refused.
    fn validate_configs(&self) -> Result<(), StateError> {
        for (conversation_id, config) in &self.configs {
            if self.conversation(*conversation_id).is_none() {
                return Err(StateError::InconsistentReconstruction {
                    rule: "configuration",
                    detail: format!("conversation {conversation_id} does not exist"),
                });
            }
            if !self.config_revisions.contains_key(conversation_id) {
                return Err(StateError::InconsistentReconstruction {
                    rule: "configuration",
                    detail: format!(
                        "conversation {conversation_id} has a configuration with no recorded commit"
                    ),
                });
            }
            if let Err(error) = config.validate() {
                return Err(StateError::InconsistentReconstruction {
                    rule: "configuration",
                    detail: format!(
                        "conversation {conversation_id} has an invalid configuration: {error}"
                    ),
                });
            }
        }
        for conversation_id in self.config_revisions.keys() {
            if !self.configs.contains_key(conversation_id) {
                return Err(StateError::InconsistentReconstruction {
                    rule: "configuration",
                    detail: format!(
                        "conversation {conversation_id} records a configuration commit with no configuration"
                    ),
                });
            }
        }
        Ok(())
    }

    /// Every durable id comes from one monotonic sequence and every commit from
    /// the commit cursor, so neither may exceed what the metadata records.
    fn validate_sequence_bounds(&self) -> Result<(), StateError> {
        let records: usize =
            self.conversations.len() + self.entries.len() + self.inputs.len() + self.tasks.len();
        if records > 0 && self.last_seq.is_none() {
            return Err(StateError::InconsistentReconstruction {
                rule: "sequence bound",
                detail: "records exist but no local sequence was recorded".to_owned(),
            });
        }
        let bound = self.last_seq.map(LocalSeq::get).unwrap_or_default();
        let within = |what: &str, id: i64| -> Result<(), StateError> {
            if id > bound {
                return Err(StateError::InconsistentReconstruction {
                    rule: "sequence bound",
                    detail: format!("{what} {id} exceeds the recorded sequence {bound}"),
                });
            }
            Ok(())
        };
        for conversation in self.conversations.keys() {
            within("conversation", conversation.get())?;
        }
        for entry in self.entries.keys() {
            within("entry", entry.get())?;
        }
        for input in self.inputs.keys() {
            within("input", input.get())?;
        }
        for task in self.tasks.keys() {
            within("task", task.get())?;
        }
        if !self.input_commits.is_empty() && self.last_commit.is_none() {
            return Err(StateError::InconsistentReconstruction {
                rule: "commit bound",
                detail: "committed inputs exist but no commit cursor was recorded".to_owned(),
            });
        }
        let commit_bound = self.last_commit.map(CommitSeq::get).unwrap_or_default();
        for (conversation, commit) in &self.config_revisions {
            if commit.get() > commit_bound {
                return Err(StateError::InconsistentReconstruction {
                    rule: "commit bound",
                    detail: format!(
                        "conversation {conversation} was configured at commit {} beyond the recorded cursor {commit_bound}",
                        commit.get()
                    ),
                });
            }
        }
        for (input, commit) in &self.input_commits {
            if commit.get() > commit_bound {
                return Err(StateError::InconsistentReconstruction {
                    rule: "commit bound",
                    detail: format!(
                        "input {input} was admitted at commit {} beyond the recorded cursor {commit_bound}",
                        commit.get()
                    ),
                });
            }
        }
        Ok(())
    }

    fn validate_conversations(&self) -> Result<(), StateError> {
        let root =
            self.root_conversation
                .ok_or_else(|| StateError::InconsistentReconstruction {
                    rule: "session root",
                    detail: "no root conversation was recorded".to_owned(),
                })?;
        let root_record =
            self.conversation(root)
                .ok_or_else(|| StateError::InconsistentReconstruction {
                    rule: "session root",
                    detail: format!("root conversation {root} is missing"),
                })?;
        if root_record.owner_task.is_some() {
            return Err(StateError::InconsistentReconstruction {
                rule: "session root",
                detail: format!("root conversation {root} is owned by a task"),
            });
        }
        for conversation in self.conversations.values() {
            let id = conversation.id;
            if let Some(parent) = conversation.parent {
                if self.conversation(parent.conversation_id).is_none() {
                    return Err(StateError::InconsistentReconstruction {
                        rule: "history parent",
                        detail: format!(
                            "conversation {id} inherits from missing conversation {}",
                            parent.conversation_id
                        ),
                    });
                }
                if self.entry(parent.at).is_none() {
                    return Err(StateError::InconsistentReconstruction {
                        rule: "history parent",
                        detail: format!(
                            "conversation {id} inherits from missing entry {}",
                            parent.at
                        ),
                    });
                }
            }
            if let Some(owner) = conversation.owner_task {
                let task = self.tasks.get(&owner).ok_or_else(|| {
                    StateError::InconsistentReconstruction {
                        rule: "ownership",
                        detail: format!("conversation {id} is owned by missing task {owner}"),
                    }
                })?;
                if !task.owned_conversations.contains(&id) {
                    return Err(StateError::InconsistentReconstruction {
                        rule: "ownership",
                        detail: format!(
                            "conversation {id} names owner {owner}, which does not own it"
                        ),
                    });
                }
            }
            if let Some(turn) = conversation.foreground_turn {
                let record = self.tasks.get(&turn).ok_or_else(|| {
                    StateError::InconsistentReconstruction {
                        rule: "foreground turn",
                        detail: format!("conversation {id} holds missing turn {turn}"),
                    }
                })?;
                if record.turn != Some(turn) || record.conversation_id != id {
                    return Err(StateError::InconsistentReconstruction {
                        rule: "foreground turn",
                        detail: format!("conversation {id} does not hold the root of turn {turn}"),
                    });
                }
            }
            if conversation.turn_cancelled && conversation.foreground_turn.is_none() {
                return Err(StateError::InconsistentReconstruction {
                    rule: "foreground turn",
                    detail: format!("conversation {id} records a cancelled turn but holds no slot"),
                });
            }
            if conversation.retired && self.conversation_has_live_work(id) {
                return Err(StateError::InconsistentReconstruction {
                    rule: "retirement",
                    detail: format!("retired conversation {id} still has live work"),
                });
            }
        }
        Ok(())
    }

    /// Whether a conversation holds a slot or non-terminal task, which retirement
    /// is defined to exclude.
    fn conversation_has_live_work(&self, conversation_id: ConversationId) -> bool {
        self.conversations
            .get(&conversation_id)
            .is_some_and(|conversation| conversation.foreground_turn.is_some())
            || self.tasks.values().any(|task| {
                task.conversation_id == conversation_id
                    && !matches!(task.status, TaskStatus::Terminal(_))
            })
    }

    fn validate_entries(&self) -> Result<(), StateError> {
        for entry in self.entries.values() {
            if self.conversation(entry.conversation_id).is_none() {
                return Err(StateError::InconsistentReconstruction {
                    rule: "entry conversation",
                    detail: format!(
                        "entry {} names missing conversation {}",
                        entry.id, entry.conversation_id
                    ),
                });
            }
        }
        Ok(())
    }

    fn validate_inputs(&self) -> Result<(), StateError> {
        for input in self.inputs.values() {
            if self.conversation(input.target).is_none() {
                return Err(StateError::InconsistentReconstruction {
                    rule: "input target",
                    detail: format!(
                        "input {} targets missing conversation {}",
                        input.id, input.target
                    ),
                });
            }
            let Some(placement) = input.disposition.placement() else {
                continue;
            };
            let entry = self.entry(placement.entry).ok_or_else(|| {
                StateError::InconsistentReconstruction {
                    rule: "input placement",
                    detail: format!(
                        "input {} is placed at missing entry {}",
                        input.id, placement.entry
                    ),
                }
            })?;
            if entry.conversation_id != input.target {
                return Err(StateError::InconsistentReconstruction {
                    rule: "input placement",
                    detail: format!(
                        "input {} is placed at an entry of another conversation",
                        input.id
                    ),
                });
            }
            let turn = self.tasks.get(&placement.turn).ok_or_else(|| {
                StateError::InconsistentReconstruction {
                    rule: "input placement",
                    detail: format!(
                        "input {} is bound to missing turn {}",
                        input.id, placement.turn
                    ),
                }
            })?;
            if turn.conversation_id != input.target {
                return Err(StateError::InconsistentReconstruction {
                    rule: "input placement",
                    detail: format!(
                        "input {} is bound to a turn of another conversation",
                        input.id
                    ),
                });
            }
        }
        Ok(())
    }

    fn validate_tasks(&self) -> Result<(), StateError> {
        for task in self.tasks.values() {
            let id = task.id;
            if self.conversation(task.conversation_id).is_none() {
                return Err(StateError::InconsistentReconstruction {
                    rule: "task conversation",
                    detail: format!(
                        "task {id} names missing conversation {}",
                        task.conversation_id
                    ),
                });
            }
            for dependency in &task.dependencies {
                if !self.tasks.contains_key(dependency) {
                    return Err(StateError::InconsistentReconstruction {
                        rule: "dependency",
                        detail: format!("task {id} depends on missing task {dependency}"),
                    });
                }
                if dependency.get() >= id.get() {
                    return Err(StateError::InconsistentReconstruction {
                        rule: "dependency",
                        detail: format!("task {id} depends forward on {dependency}"),
                    });
                }
            }
            for owned in &task.owned_conversations {
                let conversation = self.conversation(*owned).ok_or_else(|| {
                    StateError::InconsistentReconstruction {
                        rule: "ownership",
                        detail: format!("task {id} owns missing conversation {owned}"),
                    }
                })?;
                if conversation.owner_task != Some(id) {
                    return Err(StateError::InconsistentReconstruction {
                        rule: "ownership",
                        detail: format!(
                            "task {id} lists conversation {owned}, which names another owner"
                        ),
                    });
                }
            }
            if let Some(turn) = task.turn {
                if turn.get() > id.get() {
                    return Err(StateError::InconsistentReconstruction {
                        rule: "turn membership",
                        detail: format!("task {id} belongs to later turn {turn}"),
                    });
                }
                let root = self.tasks.get(&turn).ok_or_else(|| {
                    StateError::InconsistentReconstruction {
                        rule: "turn membership",
                        detail: format!("task {id} belongs to missing turn {turn}"),
                    }
                })?;
                if root.turn != Some(turn) {
                    return Err(StateError::InconsistentReconstruction {
                        rule: "turn membership",
                        detail: format!("task {id} belongs to {turn}, which is not a turn root"),
                    });
                }
            }
            if let Some(closing) = task.turn_closed_by {
                if task.turn != Some(id) {
                    return Err(StateError::InconsistentReconstruction {
                        rule: "turn receipt",
                        detail: format!(
                            "task {id} records closure but is not its conversation's turn root"
                        ),
                    });
                }
                let member = self.tasks.get(&closing).ok_or_else(|| {
                    StateError::InconsistentReconstruction {
                        rule: "turn receipt",
                        detail: format!("turn {id} was closed by missing task {closing}"),
                    }
                })?;
                if member.turn != Some(id) {
                    return Err(StateError::InconsistentReconstruction {
                        rule: "turn receipt",
                        detail: format!("turn {id} was closed by non-member {closing}"),
                    });
                }
            }
            match task.status {
                TaskStatus::Pending => {
                    if task.generation != 0 || task.invocation.is_some() {
                        return Err(StateError::InconsistentReconstruction {
                            rule: "task lifecycle",
                            detail: format!("pending task {id} records an invocation"),
                        });
                    }
                }
                TaskStatus::Running => {
                    if task.generation == 0 || task.invocation.is_none() {
                        return Err(StateError::InconsistentReconstruction {
                            rule: "task lifecycle",
                            detail: format!("running task {id} records no invocation"),
                        });
                    }
                }
                TaskStatus::Terminal(_) => {
                    if task.generation == 0 {
                        return Err(StateError::InconsistentReconstruction {
                            rule: "task lifecycle",
                            detail: format!("terminal task {id} records no invocation generation"),
                        });
                    }
                    if task.invocation.is_some() {
                        return Err(StateError::InconsistentReconstruction {
                            rule: "task lifecycle",
                            detail: format!("terminal task {id} still records an invocation"),
                        });
                    }
                }
            }
            if let Some(invocation) = task.invocation
                && invocation.generation != task.generation
            {
                return Err(StateError::InconsistentReconstruction {
                    rule: "task lifecycle",
                    detail: format!(
                        "task {id} records generation {} with invocation generation {}",
                        task.generation, invocation.generation
                    ),
                });
            }
        }
        Ok(())
    }
}
