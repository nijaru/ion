//! Conversion helpers for Gemini Code Assist API.

use super::types::{
    CodeAssistRequest, GeminiContent, GeminiFunctionCall, GeminiFunctionDeclaration,
    GeminiFunctionResponse, GeminiGenerationConfig, GeminiPart, GeminiRequest, GeminiTool,
    VertexRequest,
};
use crate::provider::types::{ChatRequest, ContentBlock, Role};
use std::collections::HashMap;

impl CodeAssistRequest {
    pub(crate) fn from_chat_request(request: &ChatRequest, model: &str, project_id: &str) -> Self {
        let inner = GeminiRequest::from_chat_request(request);

        let prompt_id = format!("ion-{}", uuid::Uuid::new_v4());

        Self {
            model: model.to_string(),
            project: Some(project_id.to_string()),
            user_prompt_id: Some(prompt_id),
            request: VertexRequest {
                contents: inner.contents,
                system_instruction: inner.system_instruction,
                tools: inner.tools,
                generation_config: inner.generation_config,
            },
        }
    }
}

impl GeminiRequest {
    #[allow(clippy::too_many_lines)]
    pub(crate) fn from_chat_request(request: &ChatRequest) -> Self {
        let mut contents = Vec::new();
        let mut system_instruction = None;

        // Build a map of tool_call_id -> function_name from assistant messages
        let mut tool_call_names: HashMap<String, String> = HashMap::new();
        for msg in request.messages.iter() {
            if msg.role == Role::Assistant {
                for block in msg.content.iter() {
                    if let ContentBlock::ToolCall { id, name, .. } = block {
                        tool_call_names.insert(id.clone(), name.clone());
                    }
                }
            }
        }

        for msg in request.messages.iter() {
            match msg.role {
                Role::System => {
                    let text = msg
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
                        .join("\n");

                    if !text.is_empty() {
                        system_instruction = Some(GeminiContent {
                            role: None,
                            parts: vec![GeminiPart {
                                text: Some(text),
                                function_call: None,
                                function_response: None,
                            }],
                        });
                    }
                }
                Role::User => {
                    let parts: Vec<GeminiPart> = msg
                        .content
                        .iter()
                        .filter_map(|b| {
                            if let ContentBlock::Text { text } = b {
                                Some(GeminiPart {
                                    text: Some(text.clone()),
                                    function_call: None,
                                    function_response: None,
                                })
                            } else {
                                None
                            }
                        })
                        .collect();

                    if !parts.is_empty() {
                        contents.push(GeminiContent {
                            role: Some("user".to_string()),
                            parts,
                        });
                    }
                }
                Role::Assistant => {
                    let mut parts = Vec::new();

                    for block in msg.content.iter() {
                        match block {
                            ContentBlock::Text { text } => {
                                parts.push(GeminiPart {
                                    text: Some(text.clone()),
                                    function_call: None,
                                    function_response: None,
                                });
                            }
                            ContentBlock::ToolCall {
                                name, arguments, ..
                            } => {
                                parts.push(GeminiPart {
                                    text: None,
                                    function_call: Some(GeminiFunctionCall {
                                        name: name.clone(),
                                        args: arguments.clone(),
                                    }),
                                    function_response: None,
                                });
                            }
                            _ => {}
                        }
                    }

                    if !parts.is_empty() {
                        contents.push(GeminiContent {
                            role: Some("model".to_string()),
                            parts,
                        });
                    }
                }
                Role::ToolResult => {
                    for block in msg.content.iter() {
                        if let ContentBlock::ToolResult {
                            tool_call_id,
                            content,
                            ..
                        } = block
                        {
                            let function_name = tool_call_names
                                .get(tool_call_id)
                                .cloned()
                                .unwrap_or_else(|| tool_call_id.clone());

                            contents.push(GeminiContent {
                                role: Some("user".to_string()),
                                parts: vec![GeminiPart {
                                    text: None,
                                    function_call: None,
                                    function_response: Some(GeminiFunctionResponse {
                                        name: function_name,
                                        response: serde_json::json!({ "result": content }),
                                    }),
                                }],
                            });
                        }
                    }
                }
            }
        }

        let tools = if request.tools.is_empty() {
            None
        } else {
            Some(vec![GeminiTool {
                function_declarations: request
                    .tools
                    .iter()
                    .map(|t| GeminiFunctionDeclaration {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        parameters: t.parameters.clone(),
                    })
                    .collect(),
            }])
        };

        let generation_config = if request.temperature.is_some() || request.max_tokens.is_some() {
            Some(GeminiGenerationConfig {
                temperature: request.temperature,
                max_output_tokens: request.max_tokens,
            })
        } else {
            None
        };

        Self {
            contents,
            system_instruction,
            tools,
            generation_config,
        }
    }
}
