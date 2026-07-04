//! Wire-compatible port of the message/part read path from
//! packages/opencode/src/session/message-v2.ts.
//!
//! Rows store the full message/part JSON in a `data` column; the wire
//! projection spreads `data` and overrides the identity fields, exactly like
//! the TS `info(row)` / `part(row)` helpers.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn spread(data: &str, identity: &[(&str, &str)]) -> Value {
    let mut object: Map<String, Value> = serde_json::from_str(data).unwrap_or_default();
    for (key, value) in identity {
        object.insert((*key).into(), Value::String((*value).into()));
    }
    Value::Object(object)
}

fn message_info(data: &str, id: &str, session_id: &str) -> Value {
    spread(data, &[("id", id), ("sessionID", session_id)])
}

fn part_info(data: &str, id: &str, session_id: &str, message_id: &str) -> Value {
    spread(
        data,
        &[
            ("id", id),
            ("sessionID", session_id),
            ("messageID", message_id),
        ],
    )
}

fn parts_for(conn: &Connection, message_ids: &[String]) -> rusqlite::Result<Map<String, Value>> {
    let mut by_message: Map<String, Value> = Map::new();
    if message_ids.is_empty() {
        return Ok(by_message);
    }
    let placeholders = vec!["?"; message_ids.len()].join(",");
    let sql = format!(
        "SELECT id, message_id, session_id, data FROM part \
         WHERE message_id IN ({placeholders}) ORDER BY message_id, id"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(rusqlite::params_from_iter(message_ids), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    for row in rows {
        let (id, message_id, session_id, data) = row?;
        let value = part_info(&data, &id, &session_id, &message_id);
        by_message
            .entry(message_id.clone())
            .or_insert_with(|| Value::Array(vec![]))
            .as_array_mut()
            .expect("array entry")
            .push(value);
    }
    Ok(by_message)
}

struct MessageRow {
    id: String,
    session_id: String,
    time_created: i64,
    data: String,
}

fn hydrate(conn: &Connection, rows: &[MessageRow]) -> rusqlite::Result<Vec<Value>> {
    let ids: Vec<String> = rows.iter().map(|row| row.id.clone()).collect();
    let mut parts = parts_for(conn, &ids)?;
    Ok(rows
        .iter()
        .map(|row| {
            json!({
                "info": message_info(&row.data, &row.id, &row.session_id),
                "parts": parts.remove(&row.id).unwrap_or_else(|| Value::Array(vec![])),
            })
        })
        .collect())
}

pub struct Page {
    pub items: Vec<Value>,
    pub more: bool,
    pub cursor: Option<(String, i64)>,
}

/// Port of MessageV2.page: newest-first fetch of limit+1 rows (optionally
/// older than a cursor), hydrated with parts and reversed to ascending order.
pub fn page(
    conn: &Connection,
    session_id: &str,
    limit: i64,
    before: Option<(String, i64)>,
) -> rusqlite::Result<Option<Page>> {
    let mut rows: Vec<MessageRow> = match before.as_ref() {
        Some((cursor_id, cursor_time)) => {
            let mut statement = conn.prepare_cached(
                "SELECT id, session_id, time_created, data FROM message \
                 WHERE session_id = ? AND (time_created < ? OR (time_created = ? AND id < ?)) \
                 ORDER BY time_created DESC, id DESC LIMIT ?",
            )?;
            let mapped = statement.query_map(
                rusqlite::params![session_id, cursor_time, cursor_time, cursor_id, limit + 1],
                |row| {
                    Ok(MessageRow {
                        id: row.get(0)?,
                        session_id: row.get(1)?,
                        time_created: row.get(2)?,
                        data: row.get(3)?,
                    })
                },
            )?;
            mapped.collect::<rusqlite::Result<_>>()?
        }
        None => {
            let mut statement = conn.prepare_cached(
                "SELECT id, session_id, time_created, data FROM message \
                 WHERE session_id = ? ORDER BY time_created DESC, id DESC LIMIT ?",
            )?;
            let mapped = statement.query_map(rusqlite::params![session_id, limit + 1], |row| {
                Ok(MessageRow {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    time_created: row.get(2)?,
                    data: row.get(3)?,
                })
            })?;
            mapped.collect::<rusqlite::Result<_>>()?
        }
    };

    if rows.is_empty() {
        let exists: Option<String> = conn
            .query_row("SELECT id FROM session WHERE id = ?", [session_id], |row| {
                row.get(0)
            })
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        if exists.is_none() {
            return Ok(None);
        }
        return Ok(Some(Page {
            items: vec![],
            more: false,
            cursor: None,
        }));
    }

    let more = rows.len() as i64 > limit;
    if more {
        rows.truncate(limit as usize);
    }
    let mut items = hydrate(conn, &rows)?;
    items.reverse();
    let cursor = if more {
        rows.last().map(|tail| (tail.id.clone(), tail.time_created))
    } else {
        None
    };
    Ok(Some(Page {
        items,
        more,
        cursor,
    }))
}

/// Port of Session.messages without a limit: page through everything in
/// batches of 50 and concatenate ascending.
pub fn all(conn: &Connection, session_id: &str) -> rusqlite::Result<Option<Vec<Value>>> {
    let mut result: Vec<Value> = vec![];
    let mut before: Option<(String, i64)> = None;
    loop {
        let Some(page) = page(conn, session_id, 50, before)? else {
            return Ok(None);
        };
        let cursor = page.cursor.clone();
        let mut items = page.items;
        items.append(&mut result);
        result = items;
        if !page.more {
            return Ok(Some(result));
        }
        before = cursor;
    }
}

/// Port of MessageV2.get: one message with its parts.
pub fn get(
    conn: &Connection,
    session_id: &str,
    message_id: &str,
) -> rusqlite::Result<Option<Value>> {
    let row = conn
        .query_row(
            "SELECT id, session_id, time_created, data FROM message WHERE id = ? AND session_id = ?",
            [message_id, session_id],
            |row| {
                Ok(MessageRow {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    time_created: row.get(2)?,
                    data: row.get(3)?,
                })
            },
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    let Some(row) = row else { return Ok(None) };
    Ok(hydrate(conn, std::slice::from_ref(&row))?
        .into_iter()
        .next())
}

/// Port of Todo.get: ascending by position, three-field projection.
pub fn todos(conn: &Connection, session_id: &str) -> rusqlite::Result<Vec<Value>> {
    let mut statement = conn.prepare_cached(
        "SELECT content, status, priority FROM todo WHERE session_id = ? ORDER BY position ASC",
    )?;
    let rows = statement.query_map([session_id], |row| {
        Ok(json!({
            "content": row.get::<_, String>(0)?,
            "status": row.get::<_, String>(1)?,
            "priority": row.get::<_, String>(2)?,
        }))
    })?;
    rows.collect()
}
