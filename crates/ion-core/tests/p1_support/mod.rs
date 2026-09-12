mod store;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

pub use store::{
    AdmissionError, AdmissionMode, AgentId, AgentStatus, EffectId, EffectSpec, PrototypeStore,
    Receipt, Recovery, RecoveryDecision, TaskId,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnCheckpoint {
    Ready,
    WaitingTools { effects: Vec<EffectId> },
    Done { results: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BehaviorStep {
    Effects(Vec<EffectSpec>),
    Wait,
    Complete(Vec<String>),
}

pub trait Behavior {
    type Checkpoint: Clone + Serialize + for<'de> Deserialize<'de>;

    fn checkpoint(&self) -> &Self::Checkpoint;
    fn step(&mut self, store: &PrototypeStore) -> rusqlite::Result<BehaviorStep>;
}

pub struct TwoToolTurn {
    checkpoint: TurnCheckpoint,
}

impl TwoToolTurn {
    pub const fn new() -> Self {
        Self {
            checkpoint: TurnCheckpoint::Ready,
        }
    }

    pub const fn restore(checkpoint: TurnCheckpoint) -> Self {
        Self { checkpoint }
    }

    pub fn bind_effects(&mut self, effects: Vec<EffectId>) {
        self.checkpoint = TurnCheckpoint::WaitingTools { effects };
    }
}

impl Behavior for TwoToolTurn {
    type Checkpoint = TurnCheckpoint;

    fn checkpoint(&self) -> &Self::Checkpoint {
        &self.checkpoint
    }

    fn step(&mut self, store: &PrototypeStore) -> rusqlite::Result<BehaviorStep> {
        match &self.checkpoint {
            TurnCheckpoint::Ready => Ok(BehaviorStep::Effects(vec![
                EffectSpec {
                    ordinal: 0,
                    name: "A",
                    recovery: Recovery::ReplaySafe,
                },
                EffectSpec {
                    ordinal: 1,
                    name: "B",
                    recovery: Recovery::ReplaySafe,
                },
            ])),
            TurnCheckpoint::WaitingTools { effects } => {
                let mut results = Vec::with_capacity(effects.len());
                for effect_id in effects {
                    let Some(result) = store.effect_result(*effect_id)? else {
                        return Ok(BehaviorStep::Wait);
                    };
                    results.push(result);
                }
                self.checkpoint = TurnCheckpoint::Done {
                    results: results.clone(),
                };
                Ok(BehaviorStep::Complete(results))
            }
            TurnCheckpoint::Done { results } => Ok(BehaviorStep::Complete(results.clone())),
        }
    }
}

pub struct AsyncTwoToolTurn;

impl AsyncTwoToolTurn {
    pub async fn run(
        first: oneshot::Receiver<String>,
        second: oneshot::Receiver<String>,
    ) -> Vec<String> {
        let (first, second) = tokio::join!(first, second);
        vec![first.expect("first effect"), second.expect("second effect")]
    }
}

pub struct UiState {
    focused: AgentId,
    drafts: HashMap<AgentId, String>,
    pending: HashMap<u64, AgentId>,
    pub replies: Vec<(AgentId, String)>,
}

impl UiState {
    pub fn new(root: AgentId) -> Self {
        Self {
            focused: root,
            drafts: HashMap::from([(root, String::new())]),
            pending: HashMap::new(),
            replies: Vec::new(),
        }
    }

    pub fn focus(&mut self, agent_id: AgentId) {
        self.focused = agent_id;
        self.drafts.entry(agent_id).or_default();
    }

    pub fn set_draft(&mut self, text: impl Into<String>) {
        self.drafts.insert(self.focused, text.into());
    }

    pub fn draft(&self, agent_id: AgentId) -> &str {
        self.drafts.get(&agent_id).map_or("", String::as_str)
    }

    pub fn begin_command(&mut self, request_id: u64, target: AgentId) {
        self.pending.insert(request_id, target);
    }

    pub fn apply_reply(&mut self, request_id: u64, text: impl Into<String>) {
        let target = self
            .pending
            .remove(&request_id)
            .expect("reply must address a pending command");
        self.replies.push((target, text.into()));
    }
}
