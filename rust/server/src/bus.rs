//! Port of the event publication path: in-memory fan-out for SSE subscribers
//! (matching the Bun instance bus wire shape `{id, type, properties}`) plus
//! the durable event-store write from packages/core/src/event.ts for durable
//! event types (versioned type suffix, per-aggregate sequence).

use crate::identifier;
use rusqlite::Connection;
use serde_json::{json, Value};
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct Bus {
    sender: broadcast::Sender<Value>,
    v2_sender: broadcast::Sender<Value>,
}

/// Durable v1 session events all use version 1 with the sessionID aggregate
/// (packages/schema/src/v1/session.ts `options`).
const DURABLE_VERSION: i64 = 1;

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus {
    pub fn new() -> Self {
        Bus {
            sender: broadcast::channel(1024).0,
            v2_sender: broadcast::channel(1024).0,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.sender.subscribe()
    }

    pub fn subscribe_v2(&self) -> broadcast::Receiver<Value> {
        self.v2_sender.subscribe()
    }

    pub fn publish_v2(&self, event: Value) {
        let _ = self.v2_sender.send(event);
    }

    /// Publish a transient event (not persisted to the event store). Used for
    /// PTY lifecycle events which the TS port emits through `EventV2.publish`
    /// but which are not part of the durable event manifest.
    pub fn publish(&self, event_type: &str, properties: Value) {
        let id = format!("evt_{}", identifier::create(false, now_millis()));
        let _ = self
            .sender
            .send(json!({ "id": id.clone(), "type": event_type, "properties": properties.clone() }));
        let _ = self
            .v2_sender
            .send(json!({ "id": id, "type": event_type, "data": properties }));
    }

    /// Publish a durable session event: write the versioned copy to the
    /// event store inside an immediate transaction, then fan out the wire
    /// payload to SSE subscribers.
    pub fn publish_durable(
        &self,
        conn: &mut Connection,
        event_type: &str,
        aggregate_id: &str,
        properties: Value,
    ) -> rusqlite::Result<()> {
        let id = format!("evt_{}", identifier::create(false, now_millis()));
        let tx = tx_immediate(conn)?;
        let latest: i64 = tx
            .query_row(
                "SELECT seq FROM event_sequence WHERE aggregate_id = ?",
                [aggregate_id],
                |row| row.get(0),
            )
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(-1),
                other => Err(other),
            })?;
        let seq = latest + 1;
        tx.execute(
            "INSERT INTO event_sequence (aggregate_id, seq) VALUES (?, ?) \
             ON CONFLICT(aggregate_id) DO UPDATE SET seq = excluded.seq",
            rusqlite::params![aggregate_id, seq],
        )?;
        tx.execute(
            "INSERT INTO event (id, aggregate_id, seq, type, data) VALUES (?, ?, ?, ?, ?)",
            rusqlite::params![
                id,
                aggregate_id,
                seq,
                format!("{event_type}.{DURABLE_VERSION}"),
                properties.to_string()
            ],
        )?;
        tx.commit()?;

        let _ = self
            .sender
            .send(json!({ "id": id, "type": event_type, "properties": properties }));
        Ok(())
    }

    /// Remove the durable history for an aggregate; the event table cascades
    /// from event_sequence.
    pub fn remove_aggregate(&self, conn: &Connection, aggregate_id: &str) -> rusqlite::Result<()> {
        conn.execute(
            "DELETE FROM event_sequence WHERE aggregate_id = ?",
            [aggregate_id],
        )?;
        Ok(())
    }
}

fn tx_immediate(conn: &mut Connection) -> rusqlite::Result<rusqlite::Transaction<'_>> {
    conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_millis() as u64
}

pub fn connected_event() -> Value {
    json!({
        "id": format!("evt_{}", identifier::create(false, now_millis())),
        "type": "server.connected",
        "properties": {},
    })
}
