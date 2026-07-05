//! Port of the v2 (`/api`) session surface from packages/server +
//! packages/core/src/session.ts:
//!
//!   - Session Info projection (`fromRow` in session/info.ts)
//!   - Cursor-paged session list (handlers/session.ts `session.list`)
//!   - Durable prompt admission (`SessionV2.prompt` -> `SessionInput.admit` ->
//!     `SessionInput.projectAdmitted`), including exact-retry reconciliation
//!     and conflict detection
//!   - Durable history reads (`EventV2.readAggregate` with the SessionDurable
//!     manifest)
//!   - Projected v2 message reads (handlers/message.ts)
//!
//! All JSON is produced in the exact key order the Effect schemas encode so
//! both servers agree byte-for-byte on responses and on durable rows.

use crate::b64;
use crate::identifier;
use rusqlite::{Connection, Row};
use serde_json::{json, Map, Value};

// ---------------------------------------------------------------------------
// Session Info (v2 wire shape)
// ---------------------------------------------------------------------------

pub const COLUMNS: &str = "id, parent_id, project_id, agent, model, cost, tokens_input, \
     tokens_output, tokens_reasoning, tokens_cache_read, tokens_cache_write, time_created, \
     time_updated, time_archived, title, directory, workspace_id, path, revert";

/// Session.Info (packages/schema/src/session.ts) in schema struct order:
/// id, parentID?, projectID, agent?, model?, cost, tokens, time, title,
/// location, subpath?, revert?.
pub fn info_from_row(row: &Row) -> rusqlite::Result<Value> {
    let mut info = Map::new();
    info.insert("id".into(), Value::String(row.get(0)?));
    if let Some(parent) = row.get::<_, Option<String>>(1)? {
        info.insert("parentID".into(), Value::String(parent));
    }
    info.insert("projectID".into(), Value::String(row.get(2)?));
    if let Some(agent) = row.get::<_, Option<String>>(3)? {
        info.insert("agent".into(), Value::String(agent));
    }
    if let Some(model) = row.get::<_, Option<String>>(4)? {
        if let Ok(model) = serde_json::from_str::<Value>(&model) {
            // fromRow applies the "default" variant when the stored ref has none.
            info.insert(
                "model".into(),
                json!({
                    "id": model.get("id").cloned().unwrap_or(Value::Null),
                    "providerID": model.get("providerID").cloned().unwrap_or(Value::Null),
                    "variant": model.get("variant").cloned().unwrap_or(Value::String("default".into())),
                }),
            );
        }
    }
    info.insert("cost".into(), js_number(row.get::<_, f64>(5)?));
    info.insert(
        "tokens".into(),
        json!({
            "input": row.get::<_, i64>(6)?,
            "output": row.get::<_, i64>(7)?,
            "reasoning": row.get::<_, i64>(8)?,
            "cache": { "read": row.get::<_, i64>(9)?, "write": row.get::<_, i64>(10)? },
        }),
    );
    let archived: Option<i64> = row.get(13)?;
    let mut time = Map::new();
    time.insert("created".into(), json!(row.get::<_, i64>(11)?));
    time.insert("updated".into(), json!(row.get::<_, i64>(12)?));
    if let Some(archived) = archived {
        time.insert("archived".into(), json!(archived));
    }
    info.insert("time".into(), Value::Object(time));
    info.insert("title".into(), Value::String(row.get(14)?));
    let mut location = Map::new();
    location.insert("directory".into(), Value::String(row.get(15)?));
    if let Some(workspace) = row.get::<_, Option<String>>(16)? {
        location.insert("workspaceID".into(), Value::String(workspace));
    }
    info.insert("location".into(), Value::Object(location));
    if let Some(path) = row.get::<_, Option<String>>(17)?.filter(|p| !p.is_empty()) {
        info.insert("subpath".into(), Value::String(path));
    }
    if let Some(revert) = row.get::<_, Option<String>>(18)? {
        if let Ok(revert) = serde_json::from_str::<Value>(&revert) {
            info.insert("revert".into(), revert);
        }
    }
    Ok(Value::Object(info))
}

/// JSON.stringify prints whole doubles without a fractional part; serde_json
/// prints f64 0.0 as "0.0", so whole values are emitted as integers.
pub fn js_number(value: f64) -> Value {
    if value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0 {
        return json!(value as i64);
    }
    json!(value)
}

pub fn get(conn: &Connection, session_id: &str) -> rusqlite::Result<Option<Value>> {
    let sql = format!("SELECT {COLUMNS} FROM session WHERE id = ?");
    let mut statement = conn.prepare_cached(&sql)?;
    let mut rows = statement.query_map([session_id], info_from_row)?;
    rows.next().transpose()
}

// ---------------------------------------------------------------------------
// Cursor-paged session list
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct ListQuery {
    pub workspace: Option<String>,
    pub order: Option<String>,
    pub search: Option<String>,
    pub directory: Option<String>,
    pub project: Option<String>,
    pub subpath: Option<String>,
    pub anchor: Option<Anchor>,
}

#[derive(Clone)]
pub struct Anchor {
    pub id: String,
    pub time: i64,
    pub direction: String,
}

pub fn parse_cursor(cursor: &str) -> Option<ListQuery> {
    let decoded = b64::decode(cursor)?;
    let parsed: Value = serde_json::from_slice(&decoded).ok()?;
    let anchor = parsed.get("anchor")?;
    Some(ListQuery {
        workspace: text(&parsed, "workspace"),
        order: text(&parsed, "order"),
        search: text(&parsed, "search"),
        directory: text(&parsed, "directory"),
        project: text(&parsed, "project"),
        subpath: text(&parsed, "subpath"),
        anchor: Some(Anchor {
            id: anchor.get("id")?.as_str()?.to_string(),
            time: anchor.get("time")?.as_i64()?,
            direction: anchor.get("direction")?.as_str()?.to_string(),
        }),
    })
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Cursor payloads mirror `withCursor(...)` field order: workspace, order,
/// search, then the union-specific filter fields, then the anchor.
fn make_cursor(query: &ListQuery, anchor: &Value) -> String {
    let mut out = Map::new();
    if let Some(workspace) = &query.workspace {
        out.insert("workspace".into(), Value::String(workspace.clone()));
    }
    if let Some(order) = &query.order {
        out.insert("order".into(), Value::String(order.clone()));
    }
    if let Some(search) = &query.search {
        out.insert("search".into(), Value::String(search.clone()));
    }
    if let Some(directory) = &query.directory {
        out.insert("directory".into(), Value::String(directory.clone()));
    }
    if let Some(project) = &query.project {
        out.insert("project".into(), Value::String(project.clone()));
        if let Some(subpath) = &query.subpath {
            out.insert("subpath".into(), Value::String(subpath.clone()));
        }
    }
    out.insert("anchor".into(), anchor.clone());
    b64::encode(&Value::Object(out).to_string())
}

/// Port of SessionV2.list + the session.list handler: filter, anchor
/// pagination, and previous/next cursor generation.
pub fn list(conn: &Connection, query: &ListQuery, limit: i64) -> rusqlite::Result<Value> {
    let direction = query
        .anchor
        .as_ref()
        .map(|anchor| anchor.direction.clone())
        .unwrap_or_else(|| "next".into());
    let requested = query.order.clone().unwrap_or_else(|| "desc".into());
    let order = if direction == "previous" {
        if requested == "asc" {
            "desc"
        } else {
            "asc"
        }
    } else {
        requested.as_str()
    };

    let mut conditions: Vec<String> = vec![];
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![];
    if let Some(directory) = &query.directory {
        conditions.push("directory = ?".into());
        params.push(Box::new(directory.clone()));
    }
    if let Some(workspace) = &query.workspace {
        conditions.push("workspace_id = ?".into());
        params.push(Box::new(workspace.clone()));
    }
    if let Some(project) = &query.project {
        conditions.push("project_id = ?".into());
        params.push(Box::new(project.clone()));
    }
    if let Some(search) = &query.search {
        conditions.push("title LIKE ?".into());
        params.push(Box::new(format!("%{search}%")));
    }
    if let Some(anchor) = &query.anchor {
        let compare = if order == "asc" { ">" } else { "<" };
        conditions.push(format!(
            "(time_created {compare} ? OR (time_created = ? AND id {compare} ?))"
        ));
        params.push(Box::new(anchor.time));
        params.push(Box::new(anchor.time));
        params.push(Box::new(anchor.id.clone()));
    }

    let direction_sql = if order == "asc" { "ASC" } else { "DESC" };
    let mut sql = format!("SELECT {COLUMNS} FROM session");
    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    sql.push_str(&format!(
        " ORDER BY time_created {direction_sql}, id {direction_sql} LIMIT ?"
    ));
    params.push(Box::new(limit));

    let mut statement = conn.prepare_cached(&sql)?;
    let mut rows: Vec<Value> = statement
        .query_map(
            rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
            info_from_row,
        )?
        .collect::<rusqlite::Result<_>>()?;
    if direction == "previous" {
        rows.reverse();
    }

    let cursor_for = |info: &Value, direction: &str| {
        let anchor = json!({
            "id": info.get("id").cloned().unwrap_or(Value::Null),
            "time": info.get("time").and_then(|time| time.get("created")).cloned().unwrap_or(Value::Null),
            "direction": direction,
        });
        Value::String(make_cursor(query, &anchor))
    };
    let previous = rows.first().map(|info| cursor_for(info, "previous"));
    let next = rows.last().map(|info| cursor_for(info, "next"));
    Ok(json!({
        "data": rows,
        "cursor": {
            "previous": previous.unwrap_or(Value::Null),
            "next": next.unwrap_or(Value::Null),
        },
    }))
}

// ---------------------------------------------------------------------------
// Prompt resolution (PromptInput -> Prompt, with mime inference)
// ---------------------------------------------------------------------------

pub enum AdmitError {
    NotFound,
    Conflict(String),
    BadRequest(String),
    Storage(String),
}

impl From<rusqlite::Error> for AdmitError {
    fn from(error: rusqlite::Error) -> Self {
        AdmitError::Storage(error.to_string())
    }
}

/// Port of `resolvePrompt`: file attachments gain a `mime` derived from a
/// data-URI prefix or the target path extension. Output keys follow the
/// Prompt schema order: text, files?, agents?; file entries follow uri, mime,
/// name?, description?, source?.
pub fn resolve_prompt(input: &Value) -> Result<Value, AdmitError> {
    let text = input
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| AdmitError::BadRequest("prompt.text is required".into()))?;
    let mut prompt = Map::new();
    prompt.insert("text".into(), Value::String(text.to_string()));
    if let Some(files) = input.get("files").and_then(Value::as_array) {
        let resolved = files
            .iter()
            .map(|file| {
                let uri = file
                    .get("uri")
                    .and_then(Value::as_str)
                    .ok_or_else(|| AdmitError::BadRequest("file.uri is required".into()))?;
                let mut entry = Map::new();
                entry.insert("uri".into(), Value::String(uri.to_string()));
                entry.insert("mime".into(), Value::String(file_mime(file, uri)));
                for key in ["name", "description", "source"] {
                    if let Some(value) = file.get(key) {
                        entry.insert(key.into(), value.clone());
                    }
                }
                Ok(Value::Object(entry))
            })
            .collect::<Result<Vec<_>, AdmitError>>()?;
        prompt.insert("files".into(), Value::Array(resolved));
    }
    if let Some(agents) = input.get("agents") {
        prompt.insert("agents".into(), agents.clone());
    }
    Ok(Value::Object(prompt))
}

fn file_mime(file: &Value, uri: &str) -> String {
    if let Some(rest) = uri.strip_prefix("data:").or_else(|| {
        uri.to_ascii_lowercase()
            .starts_with("data:")
            .then(|| &uri[5..])
    }) {
        if let Some(end) = rest.find([';', ',']) {
            return rest[..end].to_string();
        }
    }
    let target = url_pathname(uri).unwrap_or_else(|| {
        file.get("name")
            .and_then(Value::as_str)
            .unwrap_or(uri)
            .to_string()
    });
    if target.ends_with('/') {
        return "application/x-directory".into();
    }
    mime_type(&target)
}

/// Mirrors URL.canParse + new URL(uri).pathname for absolute URLs.
fn url_pathname(uri: &str) -> Option<String> {
    let scheme_end = uri.find(':')?;
    let scheme = &uri[..scheme_end];
    if scheme.is_empty() || !scheme.chars().next()?.is_ascii_alphabetic() {
        return None;
    }
    if !scheme
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
    {
        return None;
    }
    let rest = &uri[scheme_end + 1..];
    let rest = rest
        .strip_prefix("//")
        .map(|after| after.find('/').map(|slash| &after[slash..]).unwrap_or("/"));
    let path = match rest {
        Some(path) => path,
        // Opaque URLs (e.g. mailto:) have an opaque path; treat everything
        // before ? / # as the pathname like WHATWG URL does.
        None => &uri[scheme_end + 1..],
    };
    let end = path.find(['?', '#']).unwrap_or(path.len());
    Some(path[..end].to_string())
}

/// Extension-based lookup for the runner's read tool (FSUtil.mimeType).
pub fn mime_for_tool(path: &str) -> String {
    mime_type(path)
}

/// Subset of mime-db lookups (FSUtil.mimeType) covering the attachment types
/// clients send; unknown extensions fall back to application/octet-stream.
fn mime_type(path: &str) -> String {
    let extension = path
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, ext)| ext.to_ascii_lowercase());
    match extension.as_deref() {
        Some("md" | "markdown") => "text/markdown",
        Some("txt" | "text" | "log") => "text/plain",
        Some("json" | "map") => "application/json",
        Some("jsonc") => "application/octet-stream",
        Some("json5") => "application/json5",
        Some("html" | "htm") => "text/html",
        Some("css") => "text/css",
        Some("csv") => "text/csv",
        Some("xml") => "text/xml",
        Some("yaml" | "yml") => "text/yaml",
        Some("js" | "mjs") => "text/javascript",
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("bmp") => "image/bmp",
        Some("ico") => "image/vnd.microsoft.icon",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("zip") => "application/zip",
        Some("gz") => "application/gzip",
        Some("ts") => "video/mp2t",
        Some("sh") => "application/x-sh",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    }
    .to_string()
}

// ---------------------------------------------------------------------------
// Durable prompt admission
// ---------------------------------------------------------------------------

const PROMPT_ADMITTED_TYPE: &str = "session.next.prompt.admitted";
const PROMPT_ADMITTED_VERSION: i64 = 1;

/// SessionInput.Admitted wire shape in schema struct order.
fn admitted_json(
    admitted_seq: i64,
    id: &str,
    session_id: &str,
    prompt: &Value,
    delivery: &str,
    time_created: i64,
    promoted_seq: Option<i64>,
) -> Value {
    let mut out = Map::new();
    out.insert("admittedSeq".into(), json!(admitted_seq));
    out.insert("id".into(), Value::String(id.to_string()));
    out.insert("sessionID".into(), Value::String(session_id.to_string()));
    out.insert("prompt".into(), prompt.clone());
    out.insert("delivery".into(), Value::String(delivery.to_string()));
    out.insert("timeCreated".into(), json!(time_created));
    if let Some(promoted) = promoted_seq {
        out.insert("promotedSeq".into(), json!(promoted));
    }
    Value::Object(out)
}

struct StoredInput {
    prompt: String,
    delivery: String,
    admitted_seq: i64,
    promoted_seq: Option<i64>,
    time_created: i64,
    session_id: String,
}

fn find_input(conn: &Connection, id: &str) -> rusqlite::Result<Option<StoredInput>> {
    let mut statement = conn.prepare_cached(
        "SELECT prompt, delivery, admitted_seq, promoted_seq, time_created, session_id \
         FROM session_input WHERE id = ?",
    )?;
    let mut rows = statement.query_map([id], |row| {
        Ok(StoredInput {
            prompt: row.get(0)?,
            delivery: row.get(1)?,
            admitted_seq: row.get(2)?,
            promoted_seq: row.get(3)?,
            time_created: row.get(4)?,
            session_id: row.get(5)?,
        })
    })?;
    rows.next().transpose()
}

/// Reusing a prompt message ID reconciles an exact retry only when Session,
/// prompt, and delivery mode match (SessionInput.equivalent); conflicting
/// reuse fails with PromptConflictError.
fn reconcile(
    stored: &StoredInput,
    session_id: &str,
    prompt: &Value,
    delivery: &str,
    id: &str,
) -> Result<Value, AdmitError> {
    let stored_prompt: Value = serde_json::from_str(&stored.prompt)
        .map_err(|error| AdmitError::Storage(error.to_string()))?;
    // Bun compares JSON.stringify of both schema-encoded prompts; both sides
    // are already normalized to struct key order, so value equality matches.
    let matches =
        stored.session_id == session_id && stored.delivery == delivery && stored_prompt == *prompt;
    if !matches {
        return Err(AdmitError::Conflict(id.to_string()));
    }
    Ok(admitted_json(
        stored.admitted_seq,
        id,
        session_id,
        &stored_prompt,
        &stored.delivery,
        stored.time_created,
        stored.promoted_seq,
    ))
}

/// Port of SessionV2.prompt in admit-only form: durably admit one
/// session_input row and its session.next.prompt.admitted event inside a
/// single immediate transaction. Model execution stays with the process that
/// owns the Session drain; the Rust server does not schedule provider work.
pub fn admit(
    conn: &mut Connection,
    session_id: &str,
    payload: &Value,
) -> Result<Value, AdmitError> {
    let exists: bool = conn
        .query_row("SELECT 1 FROM session WHERE id = ?", [session_id], |_| {
            Ok(true)
        })
        .map(|_| true)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(false),
            other => Err(other),
        })?;
    if !exists {
        return Err(AdmitError::NotFound);
    }

    let prompt = resolve_prompt(
        payload
            .get("prompt")
            .ok_or_else(|| AdmitError::BadRequest("prompt is required".into()))?,
    )?;
    let delivery = match payload.get("delivery").and_then(Value::as_str) {
        None => "steer",
        Some(delivery @ ("steer" | "queue")) => delivery,
        Some(other) => {
            return Err(AdmitError::BadRequest(format!("invalid delivery: {other}")));
        }
    };
    let id = match payload.get("id").and_then(Value::as_str) {
        Some(id) if id.starts_with("msg_") => id.to_string(),
        Some(other) => {
            return Err(AdmitError::BadRequest(format!(
                "invalid message id: {other}"
            )));
        }
        None => format!("msg_{}", identifier::ascending()),
    };

    if let Some(stored) = find_input(conn, &id)? {
        return reconcile(&stored, session_id, &prompt, delivery, &id);
    }
    // A projected message with this ID means the input lifecycle already
    // completed under different content (SessionInput.projectAdmitted's
    // LifecycleConflict path).
    let projected: bool = conn
        .query_row("SELECT 1 FROM session_message WHERE id = ?", [&id], |_| {
            Ok(true)
        })
        .map(|_| true)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(false),
            other => Err(other),
        })?;
    if projected {
        return Err(AdmitError::Conflict(id.clone()));
    }

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before epoch")
        .as_millis() as i64;
    let event_id = format!("evt_{}", identifier::ascending());
    // Event data mirrors the PromptAdmitted schema field order:
    // timestamp, sessionID, messageID, prompt, delivery.
    let mut data = Map::new();
    data.insert("timestamp".into(), json!(timestamp));
    data.insert("sessionID".into(), Value::String(session_id.to_string()));
    data.insert("messageID".into(), Value::String(id.clone()));
    data.insert("prompt".into(), prompt.clone());
    data.insert("delivery".into(), Value::String(delivery.to_string()));

    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let latest: i64 = tx
        .query_row(
            "SELECT seq FROM event_sequence WHERE aggregate_id = ?",
            [session_id],
            |row| row.get(0),
        )
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(-1),
            other => Err(other),
        })?;
    let seq = latest + 1;
    let inserted = tx.execute(
        "INSERT INTO session_input (id, session_id, prompt, delivery, admitted_seq, time_created) \
         VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(id) DO NOTHING",
        rusqlite::params![id, session_id, prompt.to_string(), delivery, seq, timestamp],
    )?;
    if inserted == 0 {
        // Lost an admission race to the other server; reconcile the stored row.
        drop(tx);
        let stored = find_input(conn, &id)?.ok_or(AdmitError::Conflict(id.clone()))?;
        return reconcile(&stored, session_id, &prompt, delivery, &id);
    }
    tx.execute(
        "INSERT INTO event_sequence (aggregate_id, seq) VALUES (?, ?) \
         ON CONFLICT(aggregate_id) DO UPDATE SET seq = excluded.seq",
        rusqlite::params![session_id, seq],
    )?;
    tx.execute(
        "INSERT INTO event (id, aggregate_id, seq, type, data) VALUES (?, ?, ?, ?, ?)",
        rusqlite::params![
            event_id,
            session_id,
            seq,
            format!("{PROMPT_ADMITTED_TYPE}.{PROMPT_ADMITTED_VERSION}"),
            Value::Object(data).to_string()
        ],
    )?;
    tx.commit()?;

    Ok(admitted_json(
        seq, &id, session_id, &prompt, delivery, timestamp, None,
    ))
}

// ---------------------------------------------------------------------------
// Durable history (EventV2.readAggregate with the SessionDurable manifest)
// ---------------------------------------------------------------------------

/// (type, durable version) pairs from SessionEvent.DurableDefinitions.
const DURABLE_MANIFEST: &[(&str, i64)] = &[
    ("session.next.agent.switched", 1),
    ("session.next.model.switched", 1),
    ("session.next.moved", 1),
    ("session.next.prompted", 1),
    ("session.next.prompt.admitted", 1),
    ("session.next.context.updated", 1),
    ("session.next.synthetic", 1),
    ("session.next.shell.started", 1),
    ("session.next.shell.ended", 1),
    ("session.next.step.started", 1),
    ("session.next.step.ended", 2),
    ("session.next.step.failed", 2),
    ("session.next.text.started", 1),
    ("session.next.text.ended", 1),
    ("session.next.tool.input.started", 1),
    ("session.next.tool.input.ended", 1),
    ("session.next.tool.called", 1),
    ("session.next.tool.progress", 1),
    ("session.next.tool.success", 1),
    ("session.next.tool.failed", 1),
    ("session.next.reasoning.started", 1),
    ("session.next.reasoning.ended", 1),
    ("session.next.retried", 1),
    ("session.next.compaction.started", 1),
    ("session.next.compaction.ended", 1),
    ("session.next.revert.staged", 1),
    ("session.next.revert.cleared", 1),
    ("session.next.revert.committed", 1),
];

pub fn history(
    conn: &Connection,
    session_id: &str,
    after: Option<i64>,
    limit: i64,
) -> rusqlite::Result<Value> {
    let versioned: Vec<String> = DURABLE_MANIFEST
        .iter()
        .map(|(kind, version)| format!("{kind}.{version}"))
        .collect();
    let placeholders = versioned.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!(
        "SELECT id, seq, type, data FROM event \
         WHERE aggregate_id = ? AND seq > ? AND type IN ({placeholders}) \
         ORDER BY seq ASC LIMIT ?"
    );
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![
        Box::new(session_id.to_string()),
        Box::new(after.unwrap_or(-1)),
    ];
    for kind in &versioned {
        params.push(Box::new(kind.clone()));
    }
    params.push(Box::new(limit + 1));

    let mut statement = conn.prepare_cached(&sql)?;
    let rows: Vec<(String, i64, String, String)> = statement
        .query_map(
            rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?
        .collect::<rusqlite::Result<_>>()?;

    let has_more = rows.len() as i64 > limit;
    let events: Vec<Value> = rows
        .into_iter()
        .take(limit as usize)
        .filter_map(|(id, seq, versioned_type, data)| {
            let (kind, version) = DURABLE_MANIFEST
                .iter()
                .find(|(kind, version)| format!("{kind}.{version}") == versioned_type)?;
            Some(json!({
                "id": id,
                "type": kind,
                "durable": { "aggregateID": session_id, "seq": seq, "version": version },
                "data": serde_json::from_str::<Value>(&data).ok()?,
            }))
        })
        .collect();
    Ok(json!({ "data": events, "hasMore": has_more }))
}

// ---------------------------------------------------------------------------
// Projected v2 messages
// ---------------------------------------------------------------------------

/// Encoded key order for every SessionMessage.Message variant
/// (packages/schema/src/session-message.ts struct definitions).
fn message_key_order(kind: &str) -> &'static [&'static str] {
    match kind {
        "agent-switched" => &["id", "metadata", "time", "type", "agent"],
        "model-switched" => &["id", "metadata", "time", "type", "model"],
        "user" => &["id", "metadata", "time", "text", "files", "agents", "type"],
        "synthetic" => &["id", "metadata", "time", "sessionID", "text", "type"],
        "system" => &["id", "metadata", "time", "type", "text"],
        "shell" => &[
            "id", "metadata", "time", "type", "callID", "command", "output",
        ],
        "assistant" => &[
            "id", "metadata", "time", "type", "agent", "model", "content", "snapshot", "finish",
            "cost", "tokens", "error",
        ],
        "compaction" => &[
            "type", "reason", "summary", "recent", "id", "metadata", "time",
        ],
        _ => &[],
    }
}

/// Rebuild the schema-encoded message JSON from a session_message row: the
/// stored data column is the encoded message minus id/type, so splicing them
/// back at their struct positions reproduces the Bun wire bytes.
fn message_json(id: &str, kind: &str, data: &str) -> Option<Value> {
    let mut data: Map<String, Value> = serde_json::from_str(&data.replace('\u{0}', "")).ok()?;
    let mut out = Map::new();
    for key in message_key_order(kind) {
        match *key {
            "id" => {
                out.insert("id".into(), Value::String(id.to_string()));
            }
            "type" => {
                out.insert("type".into(), Value::String(kind.to_string()));
            }
            key => {
                if let Some(value) = data.shift_remove(key) {
                    out.insert(key.into(), value);
                }
            }
        }
    }
    // Unknown keys (schema drift) keep their stored order at the tail.
    for (key, value) in data {
        out.insert(key, value);
    }
    if !out.contains_key("id") {
        out.insert("id".into(), Value::String(id.to_string()));
    }
    if !out.contains_key("type") {
        out.insert("type".into(), Value::String(kind.to_string()));
    }
    Some(Value::Object(out))
}

pub fn message(
    conn: &Connection,
    session_id: &str,
    message_id: &str,
) -> rusqlite::Result<Option<Value>> {
    let mut statement = conn.prepare_cached(
        "SELECT id, type, data FROM session_message WHERE id = ? AND session_id = ?",
    )?;
    let mut rows = statement.query_map([message_id, session_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    Ok(rows
        .next()
        .transpose()?
        .and_then(|(id, kind, data)| message_json(&id, &kind, &data)))
}

pub struct MessagesQuery {
    pub limit: i64,
    pub order: String,
    pub cursor_id: Option<String>,
    pub direction: Option<String>,
}

/// Port of SessionV2.messages + the session.messages handler cursor envelope.
pub fn messages(
    conn: &Connection,
    session_id: &str,
    query: &MessagesQuery,
) -> rusqlite::Result<Option<Value>> {
    let direction = query.direction.clone().unwrap_or_else(|| "next".into());
    let order = if direction == "previous" {
        if query.order == "asc" {
            "desc"
        } else {
            "asc"
        }
    } else {
        query.order.as_str()
    };

    let anchor_seq = match &query.cursor_id {
        Some(id) => {
            let seq: Option<i64> = conn
                .query_row(
                    "SELECT seq FROM session_message WHERE session_id = ? AND id = ?",
                    [session_id, id],
                    |row| row.get(0),
                )
                .map(Some)
                .or_else(|error| match error {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })?;
            if seq.is_none() {
                // Bun returns an empty page when the cursor anchor is gone.
                return Ok(Some(empty_messages_page()));
            }
            seq
        }
        None => None,
    };

    let mut sql = "SELECT id, type, data FROM session_message WHERE session_id = ?".to_string();
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(session_id.to_string())];
    if let Some(seq) = anchor_seq {
        sql.push_str(if order == "asc" {
            " AND seq > ?"
        } else {
            " AND seq < ?"
        });
        params.push(Box::new(seq));
    }
    sql.push_str(if order == "asc" {
        " ORDER BY seq ASC LIMIT ?"
    } else {
        " ORDER BY seq DESC LIMIT ?"
    });
    params.push(Box::new(query.limit));

    let mut statement = conn.prepare_cached(&sql)?;
    let mut rows: Vec<Value> = statement
        .query_map(
            rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter_map(|(id, kind, data)| message_json(&id, &kind, &data))
        .collect();
    if direction == "previous" {
        rows.reverse();
    }

    let requested = if direction == "previous" {
        if order == "asc" {
            "desc"
        } else {
            "asc"
        }
    } else {
        order
    };
    let encode = |info: &Value, direction: &str| {
        let raw = json!({
            "id": info.get("id").cloned().unwrap_or(Value::Null),
            "order": requested,
            "direction": direction,
        });
        Value::String(b64::encode(&raw.to_string()))
    };
    let previous = rows.first().map(|info| encode(info, "previous"));
    let next = rows.last().map(|info| encode(info, "next"));
    Ok(Some(json!({
        "data": rows,
        "cursor": {
            "previous": previous.unwrap_or(Value::Null),
            "next": next.unwrap_or(Value::Null),
        },
    })))
}

fn empty_messages_page() -> Value {
    json!({ "data": [], "cursor": { "previous": null, "next": null } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_data_uri_mime() {
        let prompt = resolve_prompt(&json!({
            "text": "t",
            "files": [{ "uri": "data:image/png;base64,AAA", "description": "img" }],
        }))
        .ok()
        .unwrap();
        assert_eq!(
            prompt.to_string(),
            r#"{"text":"t","files":[{"uri":"data:image/png;base64,AAA","mime":"image/png","description":"img"}]}"#
        );
    }

    #[test]
    fn resolves_file_url_mime_from_pathname() {
        let prompt = resolve_prompt(&json!({
            "text": "t",
            "files": [{ "uri": "file:///workspace/README.md", "name": "README.md" }],
        }))
        .ok()
        .unwrap();
        assert_eq!(
            prompt.to_string(),
            r#"{"text":"t","files":[{"uri":"file:///workspace/README.md","mime":"text/markdown","name":"README.md"}]}"#
        );
    }

    #[test]
    fn falls_back_to_name_for_relative_uris() {
        let prompt = resolve_prompt(&json!({
            "text": "t",
            "files": [{ "uri": "not a url", "name": "photo.jpeg" }],
        }))
        .ok()
        .unwrap();
        let mime = prompt["files"][0]["mime"].as_str().unwrap();
        assert_eq!(mime, "image/jpeg");
    }

    #[test]
    fn directories_use_the_directory_mime() {
        assert_eq!(
            file_mime(&json!({}), "file:///workspace/src/"),
            "application/x-directory"
        );
    }

    #[test]
    fn url_pathname_matches_whatwg_examples() {
        assert_eq!(url_pathname("file:///a/b.md").unwrap(), "/a/b.md");
        assert_eq!(url_pathname("https://x.dev/a.png?q=1").unwrap(), "/a.png");
        assert_eq!(url_pathname("https://x.dev").unwrap(), "/");
        assert_eq!(url_pathname("not a url"), None);
        assert_eq!(url_pathname("./relative"), None);
    }

    #[test]
    fn user_message_key_order_round_trips() {
        let rebuilt = message_json(
            "msg_1",
            "user",
            r#"{"time":{"created":5},"text":"hi","files":[]}"#,
        )
        .unwrap();
        assert_eq!(
            rebuilt.to_string(),
            r#"{"id":"msg_1","time":{"created":5},"text":"hi","files":[],"type":"user"}"#
        );
    }

    #[test]
    fn compaction_message_puts_type_first() {
        let rebuilt = message_json(
            "msg_2",
            "compaction",
            r#"{"reason":"auto","summary":"s","recent":"r","time":{"created":9}}"#,
        )
        .unwrap();
        assert_eq!(
            rebuilt.to_string(),
            r#"{"type":"compaction","reason":"auto","summary":"s","recent":"r","id":"msg_2","time":{"created":9}}"#
        );
    }
}
