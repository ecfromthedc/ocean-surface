use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AgentRole {
    User,
    Assistant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ToolStatus {
    Running,
    Ok,
    Err,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AgentBlock {
    Text(String),
    Thinking {
        content: String,
        expanded: bool,
    },
    ToolCall {
        call_id: String,
        name: String,
        args_preview: String,
        output: String,
        status: ToolStatus,
        expanded: bool,
    },
    Component {
        component_id: String,
        kind: String,
        props: Value,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentTurn {
    pub turn_id: Option<String>,
    pub role: AgentRole,
    pub blocks: Vec<AgentBlock>,
}

impl AgentTurn {
    #[must_use]
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            turn_id: None,
            role: AgentRole::User,
            blocks: vec![AgentBlock::Text(text.into())],
        }
    }

    #[must_use]
    pub fn assistant(turn_id: impl Into<String>) -> Self {
        Self {
            turn_id: Some(turn_id.into()),
            role: AgentRole::Assistant,
            blocks: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TokenStats {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub tokens_per_second: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ToolCallSummary {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub args_json: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ToolResult {
    pub ok: bool,
    #[serde(default)]
    pub output: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    SessionCreated {
        session_id: String,
        title: String,
        #[serde(default)]
        cwd: String,
    },
    TurnStarted {
        turn_id: String,
        session_id: String,
        #[serde(default)]
        model: Option<String>,
    },
    AssistantTextDelta {
        #[serde(default)]
        session_id: String,
        turn_id: String,
        delta: String,
    },
    ThinkingDelta {
        #[serde(default)]
        session_id: String,
        turn_id: String,
        delta: String,
    },
    ToolCallStarted {
        #[serde(default)]
        session_id: String,
        turn_id: String,
        call: ToolCallSummary,
    },
    ToolCallChunk {
        #[serde(default)]
        session_id: String,
        turn_id: String,
        call_id: String,
        chunk: String,
    },
    ToolCallFinished {
        #[serde(default)]
        session_id: String,
        turn_id: String,
        call_id: String,
        result: ToolResult,
    },
    TurnFinished {
        #[serde(default)]
        session_id: String,
        turn_id: String,
        status: String,
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        wall_ms: Option<u64>,
        #[serde(default)]
        output_tokens: Option<u64>,
        #[serde(default)]
        input_tokens: Option<u64>,
        #[serde(default)]
        cache_read_tokens: Option<u64>,
        #[serde(default)]
        tokens_per_second: Option<f64>,
    },
    ComponentRender {
        session_id: String,
        component_id: String,
        kind: String,
        props: Value,
        #[serde(default)]
        replace: bool,
    },
    ComponentUnmount {
        session_id: String,
        component_id: String,
    },
    /// Catch-all for extension / council events (e.g. Longhouse). Carries the
    /// raw payload and an optional session `scope` (OCEAN-56). A scoped event
    /// belongs to a session; an unscoped one is council-wide (`?all=1` only).
    /// We don't render these yet, but we name the variant so they deserialize
    /// cleanly instead of being mapped to `Other` — then ignore them.
    Extension {
        extension: String,
        #[serde(default)]
        payload: Value,
        #[serde(default)]
        scope: Option<String>,
    },
    #[serde(other)]
    Other,
}

impl AgentEvent {
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        let session_id = match self {
            AgentEvent::SessionCreated { session_id, .. }
            | AgentEvent::TurnStarted { session_id, .. }
            | AgentEvent::AssistantTextDelta { session_id, .. }
            | AgentEvent::ThinkingDelta { session_id, .. }
            | AgentEvent::ToolCallStarted { session_id, .. }
            | AgentEvent::ToolCallChunk { session_id, .. }
            | AgentEvent::ToolCallFinished { session_id, .. }
            | AgentEvent::TurnFinished { session_id, .. }
            | AgentEvent::ComponentRender { session_id, .. }
            | AgentEvent::ComponentUnmount { session_id, .. } => session_id.as_str(),
            // An extension event's scope (when set) is its session id; a
            // council-wide one has no scope and is treated as unscoped.
            AgentEvent::Extension { scope, .. } => scope.as_deref().unwrap_or(""),
            AgentEvent::Other => return None,
        };
        (!session_id.is_empty()).then_some(session_id)
    }
}

#[derive(Clone, Debug)]
pub struct AgentState {
    pub session_id: Option<String>,
    pub session_title: String,
    pub active_turn_id: Option<String>,
    pub model: Option<String>,
    pub streaming: bool,
    pub composer_text: String,
    pub turns: Vec<AgentTurn>,
    pub status: String,
    pub last_turn_tokens: Option<TokenStats>,
    pub session_tokens: TokenStats,
}

impl Default for AgentState {
    fn default() -> Self {
        Self {
            session_id: None,
            session_title: String::new(),
            active_turn_id: None,
            model: None,
            streaming: false,
            composer_text: String::new(),
            turns: Vec::new(),
            status: "disconnected".to_string(),
            last_turn_tokens: None,
            session_tokens: TokenStats::default(),
        }
    }
}

impl AgentState {
    #[must_use]
    pub fn can_submit(&self) -> bool {
        !self.streaming && !self.composer_text.trim().is_empty()
    }

    pub fn insert_composer_text(&mut self, text: &str) {
        self.composer_text.push_str(text);
    }

    pub fn delete_composer_backward(&mut self) {
        self.composer_text.pop();
    }

    #[must_use]
    pub fn take_prompt_for_submit(&mut self) -> Option<String> {
        if !self.can_submit() {
            return None;
        }
        let prompt = self.composer_text.trim().to_string();
        self.composer_text.clear();
        self.turns.push(AgentTurn::user(prompt.clone()));
        self.streaming = true;
        self.status = "submitting".to_string();
        Some(prompt)
    }

    pub fn mark_post_error(&mut self, error: impl Into<String>) {
        self.streaming = false;
        self.status = format!("post error: {}", error.into());
    }

    pub fn toggle_block_expanded(&mut self, turn_index: usize, block_index: usize) -> bool {
        let Some(block) = self
            .turns
            .get_mut(turn_index)
            .and_then(|turn| turn.blocks.get_mut(block_index))
        else {
            return false;
        };

        match block {
            AgentBlock::Thinking { expanded, .. } | AgentBlock::ToolCall { expanded, .. } => {
                *expanded = !*expanded;
                true
            }
            AgentBlock::Text(_) | AgentBlock::Component { .. } => false,
        }
    }

    pub fn apply_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::SessionCreated {
                session_id: _,
                title,
                ..
            } => {
                self.session_title = title;
                self.status = "session created".to_string();
            }
            AgentEvent::TurnStarted {
                turn_id,
                session_id: _,
                model,
            } => {
                self.active_turn_id = Some(turn_id);
                if let Some(model) = model {
                    self.model = Some(model);
                }
                self.streaming = true;
                self.status = "streaming".to_string();
            }
            AgentEvent::AssistantTextDelta { turn_id, delta, .. } => {
                let turn = ensure_assistant_turn(&mut self.turns, &turn_id);
                match turn.blocks.last_mut() {
                    Some(AgentBlock::Text(text)) => text.push_str(&delta),
                    _ => turn.blocks.push(AgentBlock::Text(delta)),
                }
            }
            AgentEvent::ThinkingDelta { turn_id, delta, .. } => {
                let turn = ensure_assistant_turn(&mut self.turns, &turn_id);
                match turn.blocks.last_mut() {
                    Some(AgentBlock::Thinking { content, .. }) => content.push_str(&delta),
                    _ => turn.blocks.push(AgentBlock::Thinking {
                        content: delta,
                        expanded: false,
                    }),
                }
            }
            AgentEvent::ToolCallStarted { turn_id, call, .. } => {
                let turn = ensure_assistant_turn(&mut self.turns, &turn_id);
                let args = serde_json::to_string(&call.args_json).unwrap_or_else(|_| "{}".into());
                let args_preview = args.chars().take(60).collect();
                turn.blocks.push(AgentBlock::ToolCall {
                    call_id: call.id,
                    name: call.name,
                    args_preview,
                    output: String::new(),
                    status: ToolStatus::Running,
                    expanded: false,
                });
            }
            AgentEvent::ToolCallChunk {
                turn_id,
                call_id,
                chunk,
                ..
            } => {
                let turn = ensure_assistant_turn(&mut self.turns, &turn_id);
                if let Some((output, _)) = tool_block_mut(turn, &call_id) {
                    output.push_str(&chunk);
                }
            }
            AgentEvent::ToolCallFinished {
                turn_id,
                call_id,
                result,
                ..
            } => {
                let turn = ensure_assistant_turn(&mut self.turns, &turn_id);
                if let Some((output, status)) = tool_block_mut(turn, &call_id) {
                    if output.is_empty() && !result.output.is_empty() {
                        output.push_str(&result.output);
                    }
                    *status = if result.ok {
                        ToolStatus::Ok
                    } else {
                        ToolStatus::Err
                    };
                }
            }
            AgentEvent::TurnFinished {
                output_tokens,
                input_tokens,
                cache_read_tokens,
                tokens_per_second,
                error,
                ..
            } => {
                self.streaming = false;
                self.active_turn_id = None;
                self.status = error.unwrap_or_else(|| "ready".to_string());
                let stats = TokenStats {
                    input: input_tokens.unwrap_or(0),
                    output: output_tokens.unwrap_or(0),
                    cache_read: cache_read_tokens.unwrap_or(0),
                    tokens_per_second: tokens_per_second.unwrap_or(0.0),
                };
                self.last_turn_tokens = Some(stats);
                self.session_tokens.input += stats.input;
                self.session_tokens.output += stats.output;
                self.session_tokens.cache_read += stats.cache_read;
            }
            AgentEvent::ComponentRender {
                component_id,
                kind,
                props,
                replace,
                ..
            } => {
                if replace && replace_component(&mut self.turns, &component_id, &kind, &props) {
                    return;
                }
                let turn = ensure_assistant_turn(&mut self.turns, "component-render");
                turn.blocks.push(AgentBlock::Component {
                    component_id,
                    kind,
                    props,
                });
            }
            AgentEvent::ComponentUnmount { component_id, .. } => {
                for turn in &mut self.turns {
                    turn.blocks.retain(|block| match block {
                        AgentBlock::Component {
                            component_id: id, ..
                        } => id != &component_id,
                        _ => true,
                    });
                }
                self.turns.retain(|turn| !turn.blocks.is_empty());
            }
            AgentEvent::Extension { .. } => {
                // No renderer for extension/council events in the GPUI shell
                // yet. Accept and ignore them rather than letting an unhandled
                // tag fail to deserialize (OCEAN-62a).
            }
            AgentEvent::Other => {}
        }
    }
}

fn ensure_assistant_turn<'a>(turns: &'a mut Vec<AgentTurn>, turn_id: &str) -> &'a mut AgentTurn {
    let matches_last = turns
        .last()
        .map(|turn| turn.role == AgentRole::Assistant && turn.turn_id.as_deref() == Some(turn_id))
        .unwrap_or(false);
    if !matches_last {
        turns.push(AgentTurn::assistant(turn_id.to_string()));
    }
    turns.last_mut().expect("assistant turn should exist")
}

fn tool_block_mut<'a>(
    turn: &'a mut AgentTurn,
    call_id: &str,
) -> Option<(&'a mut String, &'a mut ToolStatus)> {
    for block in turn.blocks.iter_mut().rev() {
        if let AgentBlock::ToolCall {
            call_id: id,
            output,
            status,
            ..
        } = block
        {
            if id == call_id {
                return Some((output, status));
            }
        }
    }
    None
}

fn replace_component(
    turns: &mut [AgentTurn],
    component_id: &str,
    kind: &str,
    props: &Value,
) -> bool {
    for turn in turns {
        for block in &mut turn.blocks {
            if let AgentBlock::Component {
                component_id: id, ..
            } = block
            {
                if id == component_id {
                    *block = AgentBlock::Component {
                        component_id: component_id.to_string(),
                        kind: kind.to_string(),
                        props: props.clone(),
                    };
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{AgentBlock, AgentEvent, AgentRole, AgentState, ToolCallSummary, ToolStatus};

    #[test]
    fn submit_echoes_user_prompt_and_marks_streaming() {
        let mut state = AgentState::default();
        state.insert_composer_text("  hello ocean  ");

        let prompt = state.take_prompt_for_submit();

        assert_eq!(prompt.as_deref(), Some("hello ocean"));
        assert!(state.streaming);
        assert!(state.composer_text.is_empty());
        assert_eq!(state.turns.len(), 1);
        assert_eq!(state.turns[0].role, AgentRole::User);
    }

    #[test]
    fn reducer_streams_assistant_text_for_existing_session() {
        let mut state = AgentState::default();
        state.session_id = Some("s1".to_string());
        state.apply_event(AgentEvent::SessionCreated {
            session_id: "s1".to_string(),
            title: "Ocean".to_string(),
            cwd: "/tmp".to_string(),
        });
        state.apply_event(AgentEvent::TurnStarted {
            turn_id: "t1".to_string(),
            session_id: "s1".to_string(),
            model: Some("model-a".to_string()),
        });
        state.apply_event(AgentEvent::AssistantTextDelta {
            session_id: "s1".to_string(),
            turn_id: "t1".to_string(),
            delta: "hello".to_string(),
        });
        state.apply_event(AgentEvent::AssistantTextDelta {
            session_id: "s1".to_string(),
            turn_id: "t1".to_string(),
            delta: " world".to_string(),
        });

        assert_eq!(state.session_id.as_deref(), Some("s1"));
        assert_eq!(state.model.as_deref(), Some("model-a"));
        assert_eq!(state.active_turn_id.as_deref(), Some("t1"));
        assert_eq!(
            state.turns.last().and_then(|turn| turn.blocks.last()),
            Some(&AgentBlock::Text("hello world".to_string()))
        );
    }

    #[test]
    fn reducer_tracks_tool_output_and_status() {
        let mut state = AgentState::default();
        state.apply_event(AgentEvent::ToolCallStarted {
            session_id: "s1".to_string(),
            turn_id: "t1".to_string(),
            call: ToolCallSummary {
                id: "c1".to_string(),
                name: "shell".to_string(),
                args_json: json!({"cmd": "pwd"}),
            },
        });
        state.apply_event(AgentEvent::ToolCallChunk {
            session_id: "s1".to_string(),
            turn_id: "t1".to_string(),
            call_id: "c1".to_string(),
            chunk: "/repo".to_string(),
        });
        state.apply_event(AgentEvent::ToolCallFinished {
            session_id: "s1".to_string(),
            turn_id: "t1".to_string(),
            call_id: "c1".to_string(),
            result: super::ToolResult {
                ok: true,
                output: String::new(),
            },
        });

        let AgentBlock::ToolCall { output, status, .. } = &state.turns[0].blocks[0] else {
            panic!("expected tool block");
        };
        assert_eq!(output, "/repo");
        assert_eq!(*status, ToolStatus::Ok);
    }

    #[test]
    fn reducer_finishes_turn_and_records_tokens() {
        let mut state = AgentState::default();
        state.streaming = true;
        state.active_turn_id = Some("t1".to_string());

        state.apply_event(AgentEvent::TurnFinished {
            session_id: "s1".to_string(),
            turn_id: "t1".to_string(),
            status: "completed".to_string(),
            error: None,
            wall_ms: Some(100),
            output_tokens: Some(11),
            input_tokens: Some(7),
            cache_read_tokens: Some(3),
            tokens_per_second: Some(42.0),
        });

        assert!(!state.streaming);
        assert!(state.active_turn_id.is_none());
        assert_eq!(state.last_turn_tokens.expect("tokens").output, 11);
        assert_eq!(state.session_tokens.input, 7);
        assert_eq!(state.session_tokens.cache_read, 3);
    }

    #[test]
    fn toggles_collapsible_blocks_only() {
        let mut state = AgentState::default();
        state.turns.push(super::AgentTurn {
            turn_id: Some("t1".to_string()),
            role: AgentRole::Assistant,
            blocks: vec![
                AgentBlock::Text("hello".to_string()),
                AgentBlock::Thinking {
                    content: "reasoning".to_string(),
                    expanded: false,
                },
                AgentBlock::ToolCall {
                    call_id: "c1".to_string(),
                    name: "shell".to_string(),
                    args_preview: "{}".to_string(),
                    output: "done".to_string(),
                    status: ToolStatus::Ok,
                    expanded: false,
                },
            ],
        });

        assert!(!state.toggle_block_expanded(0, 0));
        assert!(state.toggle_block_expanded(0, 1));
        assert!(state.toggle_block_expanded(0, 2));

        let AgentBlock::Thinking { expanded, .. } = &state.turns[0].blocks[1] else {
            panic!("expected thinking block");
        };
        assert!(*expanded);

        let AgentBlock::ToolCall { expanded, .. } = &state.turns[0].blocks[2] else {
            panic!("expected tool block");
        };
        assert!(*expanded);
    }
}
