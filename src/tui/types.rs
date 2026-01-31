//! TUI type definitions: enums, structs, and constants.

use crate::tool::ApprovalResponse;
use async_trait::async_trait;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// Window duration for double-tap cancel/quit detection.
pub(super) const CANCEL_WINDOW: Duration = Duration::from_millis(1500);

/// Number of lines to show in queued message preview.
pub(crate) const QUEUED_PREVIEW_LINES: usize = 5;

/// Thinking budget level for extended reasoning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThinkingLevel {
    /// No extended thinking (default)
    #[default]
    Off,
    /// Standard budget (4k tokens)
    Standard,
    /// Extended budget (16k tokens)
    Extended,
}

impl ThinkingLevel {
    /// Cycle to the next level
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::Standard,
            Self::Standard => Self::Extended,
            Self::Extended => Self::Off,
        }
    }

    /// Get the token budget for this level, None if Off
    #[must_use]
    pub fn budget_tokens(self) -> Option<u32> {
        match self {
            Self::Off => None,
            Self::Standard => Some(4096),
            Self::Extended => Some(16384),
        }
    }

    /// Display label for the status line (empty string when off)
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "",
            Self::Standard => "[think:4k]",
            Self::Extended => "[think:16k]",
        }
    }
}

/// Modal states for the TUI. The default is Input (no mode switching required).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Standard input mode (always active unless a modal is open)
    #[default]
    Input,
    /// Tool approval prompt
    Approval,
    /// Bottom-anchored selector shell (provider/model)
    Selector,
    /// Keybinding help overlay (Ctrl+H)
    HelpOverlay,
}

/// Active page within the selector shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorPage {
    Provider,
    Model,
    Session,
}

/// Simple list selection state (replaces `ratatui::widgets::ListState`).
#[derive(Debug, Clone, Default)]
pub struct SelectionState {
    selected: Option<usize>,
}

impl SelectionState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub fn select(&mut self, index: Option<usize>) {
        self.selected = index;
    }
}

/// Request for tool approval sent from agent to TUI.
pub struct ApprovalRequest {
    pub tool_name: String,
    pub args: serde_json::Value,
    pub response_tx: oneshot::Sender<ApprovalResponse>,
}

/// Summary of a completed task for post-completion display.
#[derive(Clone)]
pub struct TaskSummary {
    pub elapsed: std::time::Duration,
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub was_cancelled: bool,
}

/// Approval handler that sends requests to the TUI for user confirmation.
pub(super) struct TuiApprovalHandler {
    pub request_tx: mpsc::Sender<ApprovalRequest>,
}

#[async_trait]
impl crate::tool::ApprovalHandler for TuiApprovalHandler {
    async fn ask_approval(&self, tool_name: &str, args: &serde_json::Value) -> ApprovalResponse {
        let (tx, rx) = oneshot::channel();
        let request = ApprovalRequest {
            tool_name: tool_name.to_string(),
            args: args.clone(),
            response_tx: tx,
        };

        if self.request_tx.send(request).await.is_err() {
            return ApprovalResponse::No;
        }

        rx.await.unwrap_or(ApprovalResponse::No)
    }
}
