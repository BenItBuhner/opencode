//! One provider turn over the OpenAI-compatible Chat protocol
//! (packages/llm/src/protocols/openai-chat.ts) via HTTPS + SSE.
//!
//! The parser is the same small state machine: reasoning deltas open
//! "reasoning-0", content closes reasoning and streams "text-0", tool-call
//! deltas accumulate JSON arguments per index, and the terminal frame yields a
//! finish reason plus usage.

use serde_json::{Map, Value};
use std::io::{BufRead, BufReader};

pub struct Turn {
    /// Content fragments in arrival order.
    pub parts: Vec<Part>,
    pub finish: String,
    pub tokens: Tokens,
    pub error: Option<String>,
}

pub enum Part {
    Reasoning(String),
    Text(String),
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
}

#[derive(Default, Clone)]
pub struct Tokens {
    pub input: i64,
    pub output: i64,
    pub reasoning: i64,
    pub cache_read: i64,
}

/// Streams one `/chat/completions` turn and folds the SSE frames into the
/// settled turn value. Returns Err only for transport-level failures.
pub fn stream(base_url: &str, api_key: &str, body: &Value) -> Result<Turn, String> {
    let response = ureq::post(&format!("{base_url}/chat/completions"))
        .set("Authorization", &format!("Bearer {api_key}"))
        .set("Content-Type", "application/json")
        .set("Accept", "text/event-stream")
        .send_string(&body.to_string());
    let response = match response {
        Ok(response) => response,
        Err(ureq::Error::Status(code, response)) => {
            let detail = response.into_string().unwrap_or_default();
            return Ok(Turn {
                parts: vec![],
                finish: "unknown".into(),
                tokens: Tokens::default(),
                error: Some(format!("Provider returned status {code}: {detail}")),
            });
        }
        Err(error) => return Err(error.to_string()),
    };

    let reader = BufReader::new(response.into_reader());
    let mut reasoning = String::new();
    let mut text = String::new();
    let mut order: Vec<Part> = vec![];
    let mut tools: Vec<(String, String, String)> = vec![];
    let mut tool_index: std::collections::BTreeMap<i64, usize> = std::collections::BTreeMap::new();
    let mut finish = String::new();
    let mut tokens = Tokens::default();
    let mut reasoning_flushed = false;

    for line in reader.lines() {
        let line = line.map_err(|error| error.to_string())?;
        let Some(payload) = line
            .strip_prefix("data: ")
            .or_else(|| line.strip_prefix("data:"))
        else {
            continue;
        };
        let payload = payload.trim();
        if payload == "[DONE]" {
            break;
        }
        let Ok(event) = serde_json::from_str::<Value>(payload) else {
            continue;
        };
        if let Some(usage) = event.get("usage").filter(|usage| usage.is_object()) {
            let prompt = usage
                .get("prompt_tokens")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let completion = usage
                .get("completion_tokens")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let cached = usage
                .get("prompt_tokens_details")
                .and_then(|details| details.get("cached_tokens"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let reasoning_tokens = usage
                .get("completion_tokens_details")
                .and_then(|details| details.get("reasoning_tokens"))
                .and_then(Value::as_i64)
                .unwrap_or(0);
            // Publisher token mapping: non-cached input, visible output.
            tokens = Tokens {
                input: (prompt - cached).max(0),
                output: (completion - reasoning_tokens).max(0),
                reasoning: reasoning_tokens.max(0),
                cache_read: cached.max(0),
            };
        }
        let Some(choice) = event
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
        else {
            continue;
        };
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            finish = map_finish_reason(reason).to_string();
        }
        let Some(delta) = choice.get("delta") else {
            continue;
        };
        if let Some(fragment) = delta.get("reasoning_content").and_then(Value::as_str) {
            if !fragment.is_empty() {
                reasoning.push_str(fragment);
            }
        }
        if let Some(fragment) = delta.get("content").and_then(Value::as_str) {
            if !fragment.is_empty() {
                if !reasoning.is_empty() && !reasoning_flushed {
                    order.push(Part::Reasoning(std::mem::take(&mut reasoning)));
                    reasoning_flushed = true;
                }
                text.push_str(fragment);
            }
        }
        if let Some(deltas) = delta.get("tool_calls").and_then(Value::as_array) {
            if !reasoning.is_empty() && !reasoning_flushed {
                order.push(Part::Reasoning(std::mem::take(&mut reasoning)));
                reasoning_flushed = true;
            }
            for tool in deltas {
                let index = tool.get("index").and_then(Value::as_i64).unwrap_or(0);
                let slot = *tool_index.entry(index).or_insert_with(|| {
                    tools.push((String::new(), String::new(), String::new()));
                    tools.len() - 1
                });
                if let Some(id) = tool.get("id").and_then(Value::as_str) {
                    tools[slot].0.push_str(id);
                }
                if let Some(function) = tool.get("function") {
                    if let Some(name) = function.get("name").and_then(Value::as_str) {
                        tools[slot].1.push_str(name);
                    }
                    if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                        tools[slot].2.push_str(arguments);
                    }
                }
            }
        }
    }

    if !reasoning.is_empty() && !reasoning_flushed {
        order.push(Part::Reasoning(reasoning));
    }
    if !text.is_empty() {
        order.push(Part::Text(text));
    }
    let has_tools = !tools.is_empty();
    for (id, name, arguments) in tools {
        order.push(Part::ToolCall {
            id,
            name,
            arguments,
        });
    }
    // finishEvents: stop with pending tool calls settles as tool-calls.
    if finish == "stop" && has_tools {
        finish = "tool-calls".into();
    }
    if finish.is_empty() {
        finish = "unknown".into();
    }
    Ok(Turn {
        parts: order,
        finish,
        tokens,
        error: None,
    })
}

fn map_finish_reason(reason: &str) -> &'static str {
    match reason {
        "stop" => "stop",
        "length" => "length",
        "content_filter" => "content-filter",
        "function_call" | "tool_calls" => "tool-calls",
        _ => "unknown",
    }
}

/// Parses accumulated tool-call arguments; empty input decodes to {} like
/// ProviderShared.parseToolInput.
pub fn parse_tool_input(arguments: &str) -> Value {
    if arguments.trim().is_empty() {
        return Value::Object(Map::new());
    }
    serde_json::from_str(arguments).unwrap_or_else(|_| Value::Object(Map::new()))
}
