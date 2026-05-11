use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseItem {
    Message {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        role: String,
        content: Vec<ContentItem>,
    },
    FunctionCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        name: String,
        arguments: String,
        call_id: String,
    },
    FunctionCallOutput {
        call_id: String,
        output: FunctionCallOutputPayload,
    },
    Reasoning {
        #[serde(default)]
        id: String,
        #[serde(default)]
        summary: Vec<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentItem {
    InputText { text: String },
    OutputText { text: String },
    InputImage { image_url: String },
}

impl ContentItem {
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::InputText { text } | Self::OutputText { text } => Some(text),
            Self::InputImage { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionCallOutputPayload {
    pub content: String,
    pub success: Option<bool>,
}

impl Serialize for FunctionCallOutputPayload {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.content)
    }
}

impl<'de> Deserialize<'de> for FunctionCallOutputPayload {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(deserializer)?;
        match v {
            serde_json::Value::String(s) => Ok(Self {
                content: s,
                success: None,
            }),
            serde_json::Value::Object(map) => {
                let content = map
                    .get("content")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                let success = map.get("success").and_then(|v| v.as_bool());
                Ok(Self { content, success })
            }
            other => Err(serde::de::Error::custom(format!(
                "function_call_output.output must be string or object, got {other:?}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ResponsesRequest {
    pub model: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub instructions: String,
    pub input: Vec<ResponseItem>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tools: Vec<serde_json::Value>,
    #[serde(default)]
    pub tool_choice: String,
    #[serde(default)]
    pub parallel_tool_calls: bool,
    #[serde(default)]
    pub stream: bool,
}

impl Default for ResponsesRequest {
    fn default() -> Self {
        Self {
            model: String::new(),
            instructions: String::new(),
            input: Vec::new(),
            tools: Vec::new(),
            tool_choice: "auto".into(),
            parallel_tool_calls: false,
            stream: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResponsesResponse {
    pub id: String,
    pub model: String,
    pub output: Vec<ResponseItem>,
    #[serde(default)]
    pub usage: Option<ResponsesUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResponsesUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_item_round_trips_through_json() {
        let item = ResponseItem::Message {
            id: Some("msg_1".into()),
            role: "user".into(),
            content: vec![ContentItem::InputText {
                text: "hello".into(),
            }],
        };
        let json = serde_json::to_value(&item).expect("serialize");
        assert_eq!(json["type"], "message");
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"][0]["type"], "input_text");
        assert_eq!(json["content"][0]["text"], "hello");
        let parsed: ResponseItem = serde_json::from_value(json).expect("deserialize");
        assert_eq!(parsed, item);
    }

    #[test]
    fn function_call_item_round_trips() {
        let item = ResponseItem::FunctionCall {
            id: None,
            name: "lookup".into(),
            arguments: r#"{"q":"x"}"#.into(),
            call_id: "call_abc".into(),
        };
        let json = serde_json::to_value(&item).expect("serialize");
        assert_eq!(json["type"], "function_call");
        assert_eq!(json["call_id"], "call_abc");
        let parsed: ResponseItem = serde_json::from_value(json).expect("deserialize");
        assert_eq!(parsed, item);
    }

    #[test]
    fn function_call_output_accepts_string_or_object_payload() {
        let raw_string = r#"{"type":"function_call_output","call_id":"c1","output":"raw text"}"#;
        let item: ResponseItem = serde_json::from_str(raw_string).expect("parse string form");
        match item {
            ResponseItem::FunctionCallOutput { call_id, output } => {
                assert_eq!(call_id, "c1");
                assert_eq!(output.content, "raw text");
                assert_eq!(output.success, None);
            }
            other => panic!("expected FunctionCallOutput, got {other:?}"),
        }

        let raw_object = r#"{"type":"function_call_output","call_id":"c2","output":{"content":"structured","success":true}}"#;
        let item: ResponseItem = serde_json::from_str(raw_object).expect("parse object form");
        match item {
            ResponseItem::FunctionCallOutput { call_id, output } => {
                assert_eq!(call_id, "c2");
                assert_eq!(output.content, "structured");
                assert_eq!(output.success, Some(true));
            }
            other => panic!("expected FunctionCallOutput, got {other:?}"),
        }
    }

    #[test]
    fn function_call_output_serializes_as_plain_string() {
        let item = ResponseItem::FunctionCallOutput {
            call_id: "c1".into(),
            output: FunctionCallOutputPayload {
                content: "ok".into(),
                success: Some(true),
            },
        };
        let json = serde_json::to_value(&item).expect("serialize");
        assert_eq!(json["output"], "ok");
    }
}
