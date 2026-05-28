//! Client-side conversation model.
//!
//! These types mirror what the TUI uses (`PmTurn` / `PmBlock` in
//! crates/ocean-tui/src/main.rs) so the rendering semantics stay consistent
//! across surfaces. They're shaped from the daemon's `AgentTurnEvent` stream.

use serde::{Deserialize, Serialize};

pub type TurnId = String;
pub type CallId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolStatus {
    Running,
    Ok,
    Err,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Block {
    Text(String),
    Thinking {
        content: String,
        expanded: bool,
    },
    ToolCall {
        call_id: CallId,
        name: String,
        args_preview: String,
        output: String,
        status: ToolStatus,
        expanded: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub turn_id: Option<TurnId>,
    pub role: Role,
    pub blocks: Vec<Block>,
}

impl Turn {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            turn_id: None,
            role: Role::User,
            blocks: vec![Block::Text(text.into())],
        }
    }

    pub fn assistant(turn_id: TurnId) -> Self {
        Self {
            turn_id: Some(turn_id),
            role: Role::Assistant,
            blocks: Vec::new(),
        }
    }

    #[allow(dead_code)] // used by model-swap / status messages in later phases
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            turn_id: None,
            role: Role::Assistant,
            blocks: vec![Block::Text(text.into())],
        }
    }
}
