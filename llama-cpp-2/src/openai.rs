//! OpenAI Specific Utility methods.
use crate::{status_is_ok, status_to_i32, ChatParseError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ffi::{c_char, CStr, CString};
use std::mem;
use std::ptr::{self, NonNull};
use std::slice;

/// OpenAI-compatible tool types supported by llama.cpp chat templates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ToolType {
    /// A function tool.
    #[default]
    Function,
}

/// An OpenAI-compatible function definition for tool calling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionDefinition {
    /// Tool name exposed to the model.
    pub name: String,
    /// Optional description shown in the prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON schema describing the function arguments.
    pub parameters: Value,
    /// Optional strict-mode flag used by some tool-calling templates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

impl FunctionDefinition {
    /// Create a new function definition from a name and JSON schema.
    #[must_use]
    pub fn new(name: impl Into<String>, parameters: Value) -> Self {
        Self {
            name: name.into(),
            description: None,
            parameters,
            strict: None,
        }
    }

    /// Attach a human-readable description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Toggle strict tool-call argument validation.
    #[must_use]
    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = Some(strict);
        self
    }
}

/// An OpenAI-compatible tool definition for tool-aware chat templates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Tool kind. This is `function` for the currently supported tool flow.
    #[serde(default)]
    pub r#type: ToolType,
    /// Function metadata and parameter schema.
    pub function: FunctionDefinition,
}

impl ToolDefinition {
    /// Create a function tool definition.
    #[must_use]
    pub fn function(function: FunctionDefinition) -> Self {
        Self {
            r#type: ToolType::Function,
            function,
        }
    }
}

/// OpenAI-compatible tool choice modes supported by llama.cpp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoice {
    /// Let the model decide whether to call a tool.
    Auto,
    /// Force at least one tool call.
    Required,
    /// Disable tool calls for this request.
    None,
}

impl ToolChoice {
    /// Return the wire-format string expected by the raw OpenAI-compatible API.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Required => "required",
            Self::None => "none",
        }
    }
}

/// Typed Rust options for the OpenAI-compatible chat template flow.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenAIChatTemplateOptions<'a> {
    /// OpenAI-compatible messages JSON array.
    pub messages_json: &'a str,
    /// Optional typed tool definitions.
    pub tools: Option<&'a [ToolDefinition]>,
    /// Optional typed tool choice mode.
    pub tool_choice: Option<ToolChoice>,
    /// Optional JSON schema value for grammar generation.
    pub json_schema: Option<&'a Value>,
    /// Optional custom grammar string.
    pub grammar: Option<&'a str>,
    /// Optional reasoning format string.
    pub reasoning_format: Option<&'a str>,
    /// Optional chat template kwargs JSON object.
    pub chat_template_kwargs: Option<&'a Value>,
    /// Whether to add the assistant generation prompt.
    pub add_generation_prompt: bool,
    /// Whether to render templates with Jinja.
    pub use_jinja: bool,
    /// Whether to allow parallel tool calls.
    pub parallel_tool_calls: bool,
    /// Whether thinking blocks are enabled.
    pub enable_thinking: bool,
    /// Whether to add BOS.
    pub add_bos: bool,
    /// Whether to add EOS.
    pub add_eos: bool,
    /// Whether to parse tool calls in responses.
    pub parse_tool_calls: bool,
}

impl<'a> OpenAIChatTemplateOptions<'a> {
    /// Create typed OpenAI-compatible options with sensible defaults.
    #[must_use]
    pub fn new(messages_json: &'a str) -> Self {
        Self {
            messages_json,
            tools: None,
            tool_choice: None,
            json_schema: None,
            grammar: None,
            reasoning_format: None,
            chat_template_kwargs: None,
            add_generation_prompt: true,
            use_jinja: true,
            parallel_tool_calls: false,
            enable_thinking: false,
            add_bos: false,
            add_eos: false,
            parse_tool_calls: false,
        }
    }
}

/// Parameters for applying OpenAI-compatible chat templates.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenAIChatTemplateParams<'a> {
    /// OpenAI-compatible messages JSON array.
    pub messages_json: &'a str,
    /// Optional OpenAI-compatible tools JSON array.
    pub tools_json: Option<&'a str>,
    /// Optional tool choice string.
    pub tool_choice: Option<&'a str>,
    /// Optional JSON schema string for tool grammar generation.
    pub json_schema: Option<&'a str>,
    /// Optional custom grammar string.
    pub grammar: Option<&'a str>,
    /// Optional reasoning format string.
    pub reasoning_format: Option<&'a str>,
    /// Optional chat template kwargs JSON object.
    pub chat_template_kwargs: Option<&'a str>,
    /// Whether to add the assistant generation prompt.
    pub add_generation_prompt: bool,
    /// Whether to render templates with Jinja.
    pub use_jinja: bool,
    /// Whether to allow parallel tool calls.
    pub parallel_tool_calls: bool,
    /// Whether thinking blocks are enabled.
    pub enable_thinking: bool,
    /// Whether to add BOS.
    pub add_bos: bool,
    /// Whether to add EOS.
    pub add_eos: bool,
    /// Whether to parse tool calls in responses.
    pub parse_tool_calls: bool,
}

/// OpenAI-compatible function call payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionCallOaicompat {
    /// Function name selected by the model.
    pub name: String,
    /// Function arguments, either as a raw JSON string or a decoded object.
    pub arguments: Value,
}

/// OpenAI-compatible tool call payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallOaicompat {
    /// Tool call id when present.
    #[serde(default)]
    pub id: Option<String>,
    /// Tool kind, typically `function`.
    #[serde(default)]
    pub r#type: Option<ToolType>,
    /// Nested function call payload.
    pub function: FunctionCallOaicompat,
}

/// Typed OpenAI-compatible parsed chat message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessageOaicompat {
    /// Message role.
    pub role: String,
    /// Message content. This may be a string, array of typed content parts, or null.
    #[serde(default)]
    pub content: Option<Value>,
    /// Optional reasoning content emitted by reasoning-capable templates.
    #[serde(default)]
    pub reasoning_content: Option<String>,
    /// Optional tool name for tool-role messages.
    #[serde(default, alias = "tool_name")]
    pub name: Option<String>,
    /// Optional tool call id for tool-role messages.
    #[serde(default)]
    pub tool_call_id: Option<String>,
    /// Optional assistant tool calls.
    #[serde(default)]
    pub tool_calls: Vec<ToolCallOaicompat>,
}

impl ChatMessageOaicompat {
    /// Parse a typed OpenAI-compatible chat message from JSON.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

/// Incremental OpenAI-compatible function call delta.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionCallDeltaOaicompat {
    /// Optional function name fragment.
    #[serde(default)]
    pub name: Option<String>,
    /// Optional arguments fragment.
    #[serde(default)]
    pub arguments: Option<String>,
}

/// Incremental OpenAI-compatible tool call delta.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallDeltaOaicompat {
    /// Tool call index within the assistant message.
    pub index: usize,
    /// Optional tool call id fragment.
    #[serde(default)]
    pub id: Option<String>,
    /// Optional tool kind.
    #[serde(default)]
    pub r#type: Option<ToolType>,
    /// Optional function delta payload.
    #[serde(default)]
    pub function: Option<FunctionCallDeltaOaicompat>,
}

/// Typed OpenAI-compatible streaming delta.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessageDeltaOaicompat {
    /// Optional role fragment.
    #[serde(default)]
    pub role: Option<String>,
    /// Optional content fragment.
    #[serde(default)]
    pub content: Option<String>,
    /// Optional reasoning content fragment.
    #[serde(default)]
    pub reasoning_content: Option<String>,
    /// Optional tool call deltas.
    #[serde(default)]
    pub tool_calls: Vec<ToolCallDeltaOaicompat>,
}

impl ChatMessageDeltaOaicompat {
    /// Parse a typed OpenAI-compatible streaming delta from JSON.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

/// Streaming OpenAI-compatible parser state.
#[derive(Debug)]
pub struct ChatParseStateOaicompat {
    pub(crate) state: NonNull<llama_cpp_sys_2::llama_rs_chat_parse_state_oaicompat>,
}

impl ChatParseStateOaicompat {
    /// Update the parser with additional text and return OpenAI-compatible deltas as JSON strings.
    pub fn update(
        &mut self,
        text_added: &str,
        is_partial: bool,
    ) -> Result<Vec<String>, ChatParseError> {
        let text_cstr = CString::new(text_added)?;
        let mut out_msg: llama_cpp_sys_2::llama_rs_chat_msg_oaicompat = unsafe { mem::zeroed() };
        let mut out_diffs: *mut llama_cpp_sys_2::llama_rs_chat_msg_diff_oaicompat = ptr::null_mut();
        let mut out_diffs_count: usize = 0;
        let rc = unsafe {
            llama_cpp_sys_2::llama_rs_chat_parse_state_update_oaicompat(
                self.state.as_ptr(),
                text_cstr.as_ptr(),
                is_partial,
                &mut out_msg,
                &mut out_diffs,
                &mut out_diffs_count,
            )
        };

        let result = {
            if !status_is_ok(rc) {
                return Err(ChatParseError::FfiError(status_to_i32(rc)));
            }
            if out_diffs_count > 0 && out_diffs.is_null() {
                return Err(ChatParseError::NullResult);
            }
            let diffs = if out_diffs_count == 0 {
                &[]
            } else {
                unsafe { slice::from_raw_parts(out_diffs, out_diffs_count) }
            };
            let mut deltas = Vec::with_capacity(diffs.len());
            for diff in diffs {
                let mut out_json: *mut c_char = ptr::null_mut();
                let rc = unsafe {
                    llama_cpp_sys_2::llama_rs_chat_msg_diff_to_oaicompat_json(diff, &mut out_json)
                };
                if !status_is_ok(rc) {
                    if !out_json.is_null() {
                        unsafe { llama_cpp_sys_2::llama_rs_string_free(out_json) };
                    }
                    return Err(ChatParseError::FfiError(status_to_i32(rc)));
                }
                if out_json.is_null() {
                    return Err(ChatParseError::NullResult);
                }
                let bytes = unsafe { CStr::from_ptr(out_json) }.to_bytes().to_vec();
                unsafe { llama_cpp_sys_2::llama_rs_string_free(out_json) };
                deltas.push(String::from_utf8(bytes)?);
            }
            Ok(deltas)
        };

        unsafe { llama_cpp_sys_2::llama_rs_chat_msg_free_oaicompat(&mut out_msg) };
        unsafe {
            llama_cpp_sys_2::llama_rs_chat_msg_diff_free_oaicompat(out_diffs, out_diffs_count)
        };
        result
    }

    /// Update the parser with additional text and return typed OpenAI-compatible deltas.
    pub fn update_typed(
        &mut self,
        text_added: &str,
        is_partial: bool,
    ) -> Result<Vec<ChatMessageDeltaOaicompat>, ChatParseError> {
        self.update(text_added, is_partial)?
            .into_iter()
            .map(|json| ChatMessageDeltaOaicompat::from_json(&json).map_err(Into::into))
            .collect()
    }
}

impl Drop for ChatParseStateOaicompat {
    fn drop(&mut self) {
        unsafe { llama_cpp_sys_2::llama_rs_chat_parse_state_free_oaicompat(self.state.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ChatMessageDeltaOaicompat, ChatMessageOaicompat, FunctionDefinition,
        OpenAIChatTemplateOptions, ToolChoice, ToolDefinition, ToolType,
    };
    use serde_json::json;

    #[test]
    fn function_tool_serializes_to_openai_shape() {
        let tool = ToolDefinition::function(
            FunctionDefinition::new(
                "get_weather",
                json!({
                    "type": "object",
                    "properties": {
                        "location": { "type": "string" }
                    },
                    "required": ["location"]
                }),
            )
            .with_description("Look up the current weather")
            .with_strict(true),
        );

        let value = serde_json::to_value(&tool).expect("tool definition should serialize");
        assert_eq!(
            value,
            json!({
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Look up the current weather",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "location": { "type": "string" }
                        },
                        "required": ["location"]
                    },
                    "strict": true
                }
            })
        );
    }

    #[test]
    fn tool_type_defaults_to_function() {
        let tool =
            ToolDefinition::function(FunctionDefinition::new("noop", json!({ "type": "object" })));

        assert_eq!(tool.r#type, ToolType::Function);
    }

    #[test]
    fn typed_options_have_reasonable_defaults() {
        let options = OpenAIChatTemplateOptions::new("[]");

        assert_eq!(options.messages_json, "[]");
        assert_eq!(options.tool_choice, None);
        assert!(options.add_generation_prompt);
        assert!(options.use_jinja);
        assert!(!options.parallel_tool_calls);
        assert!(!options.parse_tool_calls);
    }

    #[test]
    fn tool_choice_serializes_to_expected_wire_values() {
        assert_eq!(ToolChoice::Auto.as_str(), "auto");
        assert_eq!(ToolChoice::Required.as_str(), "required");
        assert_eq!(ToolChoice::None.as_str(), "none");
    }

    #[test]
    fn parsed_chat_message_accepts_openai_tool_calls() {
        let parsed = ChatMessageOaicompat::from_json(
            r#"{
                "role":"assistant",
                "content":null,
                "reasoning_content":"thinking",
                "tool_calls":[
                    {
                        "id":"call_123",
                        "type":"function",
                        "function":{
                            "name":"get_weather",
                            "arguments":{"location":"Paris"}
                        }
                    }
                ]
            }"#,
        )
        .expect("typed parsed message should deserialize");

        assert_eq!(parsed.role, "assistant");
        assert_eq!(parsed.reasoning_content.as_deref(), Some("thinking"));
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].function.name, "get_weather");
        assert_eq!(
            parsed.tool_calls[0].function.arguments,
            json!({"location": "Paris"})
        );
    }

    #[test]
    fn parsed_chat_delta_accepts_tool_call_chunks() {
        let parsed = ChatMessageDeltaOaicompat::from_json(
            r#"{
                "content":"hi",
                "tool_calls":[
                    {
                        "index":0,
                        "id":"call_123",
                        "type":"function",
                        "function":{
                            "name":"get_weather",
                            "arguments":"{\"location\":\"Paris\"}"
                        }
                    }
                ]
            }"#,
        )
        .expect("typed parsed delta should deserialize");

        assert_eq!(parsed.content.as_deref(), Some("hi"));
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].index, 0);
        assert_eq!(
            parsed.tool_calls[0]
                .function
                .as_ref()
                .and_then(|function| function.arguments.as_deref()),
            Some("{\"location\":\"Paris\"}")
        );
    }
}
