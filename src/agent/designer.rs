use crate::provider::{ChatRequest, ContentBlock, LlmApi, Message, Role};
use anyhow::{Result, anyhow};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::sync::Arc;

/// Regex for extracting JSON objects from model responses (non-greedy).
static JSON_EXTRACTOR: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?s)\{.*?\}").expect("JSON extractor regex must be valid"));

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct PlannedTask {
    pub id: String,
    pub title: String,
    pub description: String,
    pub dependencies: Vec<String>,
    pub status: TaskStatus,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct Plan {
    pub title: String,
    pub tasks: Vec<PlannedTask>,
    pub recommended_fresh_context: bool,
}

impl Plan {
    pub fn current_task(&self) -> Option<&PlannedTask> {
        self.tasks
            .iter()
            .find(|t| t.status == TaskStatus::InProgress || t.status == TaskStatus::Pending)
    }

    pub fn mark_task(&mut self, id: &str, status: TaskStatus) {
        if let Some(task) = self.tasks.iter_mut().find(|t| t.id == id) {
            task.status = status;
        }
    }
}

pub struct Designer {
    provider: Arc<dyn LlmApi>,
}

const DESIGNER_SYSTEM: &str = r#"You are the ion Designer. Your goal is to break down complex user requests into a clear, actionable plan.
Output your plan as a JSON object.

Structure:
{
  "title": "Overall project title",
  "tasks": [
    {
      "id": "task-1",
      "title": "Task title",
      "description": "Detailed implementation steps",
      "dependencies": [],
      "status": "Pending"
    }
  ],
  "recommended_fresh_context": true/false
}

Guidelines:
- "dependencies": IDs of tasks that MUST be finished before this one.
- "recommended_fresh_context": Set to true if the plan is a major refactor or new feature where previous conversation noise might distract the agent.

Only output the JSON object. Do not include any other text."#;

impl Designer {
    pub fn new(provider: Arc<dyn LlmApi>) -> Self {
        Self { provider }
    }

    pub async fn plan(&self, user_msg: &str, model: &str, history: &[Message]) -> Result<Plan> {
        let mut messages = history.to_vec();
        messages.push(Message {
            role: Role::User,
            content: Arc::new(vec![ContentBlock::Text {
                text: format!("Plan the following task: {}", user_msg),
            }]),
        });

        let request = ChatRequest {
            model: model.to_string(),
            messages: Arc::new(messages),
            system: Some(Cow::Borrowed(DESIGNER_SYSTEM)),
            tools: Arc::new(vec![]),
            max_tokens: None,
            temperature: Some(0.0), // Low temperature for consistent JSON
            thinking: None,
        };

        let response = self.provider.complete(request).await?;

        let text = response
            .content
            .iter()
            .filter_map(|b| {
                if let ContentBlock::Text { text } = b {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("");

        // Robust JSON extraction using regex to handle model chatter or multiple blocks
        let json_str = JSON_EXTRACTOR
            .find(&text)
            .ok_or_else(|| anyhow!("No JSON object found in designer response"))?
            .as_str();

        let plan: Plan = serde_json::from_str(json_str)?;
        Ok(plan)
    }
}
