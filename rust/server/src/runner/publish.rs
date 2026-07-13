//! Durable session event publication with in-transaction message projection.
//!
//! Port of the EventV2 commit path (packages/core/src/event.ts
//! `commitDurableEvent`) combined with the session projector and
//! SessionMessageUpdater: each durable event bumps the aggregate sequence,
//! stores the versioned event row, and applies its `session_message` /
//! `session_input` projection inside one immediate transaction — exactly how
//! the Bun server persists a provider turn.

use crate::identifier;
use rusqlite::{Connection, Transaction};
use serde_json::{json, Map, Value};

pub struct Publisher {
    pub session_id: String,
}

impl Publisher {
    /// Publish one durable session event and run its projection atomically.
    /// `data` must already be in schema key order. Returns the event sequence.
    pub fn publish(
        &self,
        conn: &mut Connection,
        kind: &str,
        version: i64,
        data: &Value,
    ) -> rusqlite::Result<i64> {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let latest: i64 = tx
            .query_row(
                "SELECT seq FROM event_sequence WHERE aggregate_id = ?",
                [&self.session_id],
                |row| row.get(0),
            )
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(-1),
                other => Err(other),
            })?;
        let seq = latest + 1;
        project(&tx, &self.session_id, kind, seq, data)?;
        tx.execute(
            "INSERT INTO event_sequence (aggregate_id, seq) VALUES (?, ?) \
             ON CONFLICT(aggregate_id) DO UPDATE SET seq = excluded.seq",
            rusqlite::params![self.session_id, seq],
        )?;
        tx.execute(
            "INSERT INTO event (id, aggregate_id, seq, type, data) VALUES (?, ?, ?, ?, ?)",
            rusqlite::params![
                format!("evt_{}", identifier::ascending()),
                self.session_id,
                seq,
                format!("{kind}.{version}"),
                data.to_string()
            ],
        )?;
        tx.commit()?;
        Ok(seq)
    }
}

/// Projection dispatch mirroring packages/core/src/session/projector.ts plus
/// the SessionMessageUpdater recipes for the events the Rust runner emits.
fn project(
    tx: &Transaction,
    session_id: &str,
    kind: &str,
    seq: i64,
    data: &Value,
) -> rusqlite::Result<()> {
    match kind {
        "session.next.prompted" => {
            let message_id = text(data, "messageID");
            let timestamp = data["timestamp"].as_i64().unwrap_or_default();
            tx.execute(
                "UPDATE session_input SET promoted_seq = ? WHERE id = ? AND session_id = ? AND promoted_seq IS NULL",
                rusqlite::params![seq, message_id, session_id],
            )?;
            // User message data mirrors SessionMessage.User struct order minus
            // id/type: metadata?, time, text, files?, agents?.
            let mut message = Map::new();
            message.insert("time".into(), json!({ "created": timestamp }));
            message.insert("text".into(), data["prompt"]["text"].clone());
            if let Some(files) = data["prompt"].get("files") {
                message.insert("files".into(), files.clone());
            }
            if let Some(agents) = data["prompt"].get("agents") {
                message.insert("agents".into(), agents.clone());
            }
            insert_message(
                tx,
                session_id,
                &message_id,
                "user",
                seq,
                timestamp,
                &Value::Object(message),
            )
        }
        "session.next.agent.switched" => {
            let timestamp = data["timestamp"].as_i64().unwrap_or_default();
            tx.execute(
                "UPDATE session SET agent = ?, time_updated = ? WHERE id = ?",
                rusqlite::params![
                    data["agent"].as_str().unwrap_or_default(),
                    timestamp,
                    session_id
                ],
            )?;
            let mut message = Map::new();
            message.insert("time".into(), json!({ "created": timestamp }));
            message.insert("agent".into(), data["agent"].clone());
            insert_message(
                tx,
                session_id,
                &text(data, "messageID"),
                "agent-switched",
                seq,
                timestamp,
                &Value::Object(message),
            )
        }
        "session.next.model.switched" => {
            let timestamp = data["timestamp"].as_i64().unwrap_or_default();
            tx.execute(
                "UPDATE session SET model = ?, time_updated = ? WHERE id = ?",
                rusqlite::params![data["model"].to_string(), timestamp, session_id],
            )?;
            let mut message = Map::new();
            message.insert("time".into(), json!({ "created": timestamp }));
            message.insert("model".into(), data["model"].clone());
            insert_message(
                tx,
                session_id,
                &text(data, "messageID"),
                "model-switched",
                seq,
                timestamp,
                &Value::Object(message),
            )
        }
        "session.next.step.started" => {
            let timestamp = data["timestamp"].as_i64().unwrap_or_default();
            complete_current_assistant(tx, session_id, timestamp)?;
            // Assistant data order: metadata?, time, agent, model, content,
            // snapshot?, finish?, cost?, tokens?, error?.
            let mut message = Map::new();
            message.insert("time".into(), json!({ "created": timestamp }));
            message.insert("agent".into(), data["agent"].clone());
            message.insert("model".into(), data["model"].clone());
            message.insert("content".into(), json!([]));
            if let Some(snapshot) = data.get("snapshot") {
                message.insert("snapshot".into(), json!({ "start": snapshot }));
            }
            insert_message(
                tx,
                session_id,
                &text(data, "assistantMessageID"),
                "assistant",
                seq,
                timestamp,
                &Value::Object(message),
            )
        }
        "session.next.text.started" => update_assistant(tx, session_id, data, |message| {
            push_content(
                message,
                json!({ "type": "text", "id": data["textID"], "text": "" }),
            );
        }),
        "session.next.text.ended" => update_assistant(tx, session_id, data, |message| {
            with_content(message, "text", &text(data, "textID"), |part| {
                part.insert("text".into(), data["text"].clone());
            });
        }),
        "session.next.reasoning.started" => update_assistant(tx, session_id, data, |message| {
            let mut part = Map::new();
            part.insert("type".into(), json!("reasoning"));
            part.insert("id".into(), data["reasoningID"].clone());
            part.insert("text".into(), json!(""));
            if let Some(metadata) = data.get("providerMetadata") {
                part.insert("providerMetadata".into(), metadata.clone());
            }
            part.insert("time".into(), json!({ "created": data["timestamp"] }));
            push_content(message, Value::Object(part));
        }),
        "session.next.reasoning.ended" => update_assistant(tx, session_id, data, |message| {
            with_content(message, "reasoning", &text(data, "reasoningID"), |part| {
                part.insert("text".into(), data["text"].clone());
                let created = part
                    .get("time")
                    .and_then(|time| time.get("created"))
                    .cloned()
                    .unwrap_or_else(|| data["timestamp"].clone());
                part.insert(
                    "time".into(),
                    json!({ "created": created, "completed": data["timestamp"] }),
                );
                if let Some(metadata) = data.get("providerMetadata") {
                    part.insert("providerMetadata".into(), metadata.clone());
                }
            });
        }),
        "session.next.tool.input.started" => update_assistant(tx, session_id, data, |message| {
            // AssistantTool struct order: type, id, name, provider?, state, time.
            push_content(
                message,
                json!({
                    "type": "tool",
                    "id": data["callID"],
                    "name": data["name"],
                    "state": { "status": "pending", "input": "" },
                    "time": { "created": data["timestamp"] },
                }),
            );
        }),
        "session.next.tool.input.ended" => update_assistant(tx, session_id, data, |message| {
            with_content(message, "tool", &text(data, "callID"), |part| {
                if part["state"]["status"] == "pending" {
                    part.insert(
                        "state".into(),
                        json!({ "status": "pending", "input": data["text"] }),
                    );
                }
            });
        }),
        "session.next.tool.called" => update_assistant(tx, session_id, data, |message| {
            with_content(message, "tool", &text(data, "callID"), |part| {
                part.insert("provider".into(), data["provider"].clone());
                let created = part["time"]["created"].clone();
                part.insert(
                    "time".into(),
                    json!({ "created": created, "ran": data["timestamp"] }),
                );
                part.insert(
                    "state".into(),
                    json!({
                        "status": "running",
                        "input": data["input"],
                        "structured": {},
                        "content": [],
                    }),
                );
            });
        }),
        "session.next.tool.success" => update_assistant(tx, session_id, data, |message| {
            with_content(message, "tool", &text(data, "callID"), |part| {
                if part["state"]["status"] != "running" {
                    return;
                }
                let input = part["state"]["input"].clone();
                let mut provider = Map::new();
                provider.insert(
                    "executed".into(),
                    json!(
                        data["provider"]["executed"].as_bool().unwrap_or(false)
                            || part["provider"]["executed"].as_bool().unwrap_or(false)
                    ),
                );
                if let Some(metadata) = part["provider"].get("metadata") {
                    provider.insert("metadata".into(), metadata.clone());
                }
                if let Some(metadata) = data["provider"].get("metadata") {
                    provider.insert("resultMetadata".into(), metadata.clone());
                }
                part.insert("provider".into(), Value::Object(provider));
                let mut time = part["time"].as_object().cloned().unwrap_or_default();
                time.insert("completed".into(), data["timestamp"].clone());
                part.insert("time".into(), Value::Object(time));
                // ToolStateCompleted struct order: status, input, attachments?,
                // content, outputPaths, structured, result?.
                let mut state = Map::new();
                state.insert("status".into(), json!("completed"));
                state.insert("input".into(), input);
                state.insert("content".into(), data["content"].clone());
                state.insert(
                    "outputPaths".into(),
                    data.get("outputPaths")
                        .cloned()
                        .unwrap_or_else(|| json!([])),
                );
                state.insert("structured".into(), data["structured"].clone());
                if let Some(result) = data.get("result") {
                    state.insert("result".into(), result.clone());
                }
                part.insert("state".into(), Value::Object(state));
            });
        }),
        "session.next.tool.failed" => update_assistant(tx, session_id, data, |message| {
            with_content(message, "tool", &text(data, "callID"), |part| {
                let status = part["state"]["status"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                if status != "pending" && status != "running" {
                    return;
                }
                let mut provider = Map::new();
                provider.insert(
                    "executed".into(),
                    json!(
                        data["provider"]["executed"].as_bool().unwrap_or(false)
                            || part["provider"]["executed"].as_bool().unwrap_or(false)
                    ),
                );
                if let Some(metadata) = part["provider"].get("metadata") {
                    provider.insert("metadata".into(), metadata.clone());
                }
                if let Some(metadata) = data["provider"].get("metadata") {
                    provider.insert("resultMetadata".into(), metadata.clone());
                }
                part.insert("provider".into(), Value::Object(provider));
                let mut time = part["time"].as_object().cloned().unwrap_or_default();
                time.insert("completed".into(), data["timestamp"].clone());
                part.insert("time".into(), Value::Object(time));
                let input = if status == "running" {
                    part["state"]["input"].clone()
                } else {
                    json!({})
                };
                let mut state = Map::new();
                state.insert("status".into(), json!("error"));
                state.insert("input".into(), input);
                state.insert(
                    "content".into(),
                    if status == "running" {
                        part["state"]["content"].clone()
                    } else {
                        json!([])
                    },
                );
                state.insert(
                    "structured".into(),
                    if status == "running" {
                        part["state"]["structured"].clone()
                    } else {
                        json!({})
                    },
                );
                state.insert("error".into(), data["error"].clone());
                if let Some(result) = data.get("result") {
                    state.insert("result".into(), result.clone());
                }
                part.insert("state".into(), Value::Object(state));
            });
        }),
        "session.next.step.ended" => update_assistant(tx, session_id, data, |message| {
            let created = message["time"]["created"].clone();
            message.insert(
                "time".into(),
                json!({ "created": created, "completed": data["timestamp"] }),
            );
            message.insert("finish".into(), data["finish"].clone());
            message.insert("cost".into(), data["cost"].clone());
            message.insert("tokens".into(), data["tokens"].clone());
            if data.get("snapshot").is_some() || data.get("files").is_some() {
                let mut snapshot = message
                    .get("snapshot")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                if let Some(end) = data.get("snapshot") {
                    snapshot.insert("end".into(), end.clone());
                }
                if let Some(files) = data.get("files") {
                    snapshot.insert("files".into(), files.clone());
                }
                message.insert("snapshot".into(), Value::Object(snapshot));
            }
        }),
        "session.next.step.failed" => update_assistant(tx, session_id, data, |message| {
            let created = message["time"]["created"].clone();
            message.insert(
                "time".into(),
                json!({ "created": created, "completed": data["timestamp"] }),
            );
            message.insert("finish".into(), json!("error"));
            message.insert("error".into(), data["error"].clone());
        }),
        _ => Ok(()),
    }
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before epoch")
        .as_millis() as i64
}

fn text(data: &Value, key: &str) -> String {
    data.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn insert_message(
    tx: &Transaction,
    session_id: &str,
    message_id: &str,
    kind: &str,
    seq: i64,
    time_created: i64,
    data: &Value,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            message_id,
            session_id,
            kind,
            seq,
            time_created,
            now_millis(),
            data.to_string()
        ],
    )?;
    Ok(())
}

/// Step.Started completes any stale incomplete assistant projection first
/// (SessionMessageUpdater "a newer turn supersedes stale incomplete rows").
fn complete_current_assistant(
    tx: &Transaction,
    session_id: &str,
    timestamp: i64,
) -> rusqlite::Result<()> {
    let latest: Option<(String, String)> = tx
        .query_row(
            "SELECT id, data FROM session_message WHERE session_id = ? AND type = 'assistant' \
             ORDER BY seq DESC LIMIT 1",
            [session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    let Some((id, raw)) = latest else {
        return Ok(());
    };
    let Ok(mut message) = serde_json::from_str::<Map<String, Value>>(&raw) else {
        return Ok(());
    };
    if message
        .get("time")
        .and_then(|time| time.get("completed"))
        .is_some()
    {
        return Ok(());
    }
    let created = message["time"]["created"].clone();
    message.insert(
        "time".into(),
        json!({ "created": created, "completed": timestamp }),
    );
    write_assistant(tx, session_id, &id, &message)
}

fn update_assistant(
    tx: &Transaction,
    session_id: &str,
    data: &Value,
    recipe: impl FnOnce(&mut Map<String, Value>),
) -> rusqlite::Result<()> {
    let message_id = text(data, "assistantMessageID");
    let stored: Option<String> = tx
        .query_row(
            "SELECT data FROM session_message WHERE id = ? AND session_id = ? AND type = 'assistant'",
            [&message_id, session_id],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    let Some(raw) = stored else { return Ok(()) };
    let Ok(mut message) = serde_json::from_str::<Map<String, Value>>(&raw) else {
        return Ok(());
    };
    recipe(&mut message);
    write_assistant(tx, session_id, &message_id, &message)
}

fn write_assistant(
    tx: &Transaction,
    session_id: &str,
    message_id: &str,
    message: &Map<String, Value>,
) -> rusqlite::Result<()> {
    // Rebuild in Assistant struct key order so the stored bytes match Bun's
    // schema encoder output: metadata?, time, agent, model, content,
    // snapshot?, finish?, cost?, tokens?, error?.
    const ORDER: &[&str] = &[
        "metadata", "time", "agent", "model", "content", "snapshot", "finish", "cost", "tokens",
        "error",
    ];
    let mut ordered = Map::new();
    for key in ORDER {
        if let Some(value) = message.get(*key) {
            ordered.insert((*key).into(), value.clone());
        }
    }
    for (key, value) in message {
        if !ordered.contains_key(key) {
            ordered.insert(key.clone(), value.clone());
        }
    }
    let time_created = ordered["time"]["created"].as_i64().unwrap_or_default();
    tx.execute(
        "UPDATE session_message SET data = ?, time_created = ?, time_updated = ? \
         WHERE id = ? AND session_id = ?",
        rusqlite::params![
            Value::Object(ordered).to_string(),
            time_created,
            now_millis(),
            message_id,
            session_id
        ],
    )?;
    Ok(())
}

fn push_content(message: &mut Map<String, Value>, part: Value) {
    if let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) {
        content.push(part);
    }
}

fn with_content(
    message: &mut Map<String, Value>,
    kind: &str,
    id: &str,
    recipe: impl FnOnce(&mut Map<String, Value>),
) {
    let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) else {
        return;
    };
    let Some(part) = content
        .iter_mut()
        .rev()
        .find(|part| part["type"] == kind && part["id"] == id)
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    // Rebuild in per-variant struct key order after the recipe runs.
    recipe(part);
    let order: &[&str] = match kind {
        "text" => &["type", "id", "text"],
        "reasoning" => &["type", "id", "text", "providerMetadata", "time"],
        "tool" => &["type", "id", "name", "provider", "state", "time"],
        _ => &[],
    };
    if order.is_empty() {
        return;
    }
    let snapshot = part.clone();
    part.clear();
    for key in order {
        if let Some(value) = snapshot.get(*key) {
            part.insert((*key).into(), value.clone());
        }
    }
    for (key, value) in snapshot {
        if !part.contains_key(&key) {
            part.insert(key, value);
        }
    }
}
