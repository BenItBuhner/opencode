//! opencode-server: Rust port of the OpenCode session HTTP surface.
//!
//! First strangler-pattern increment of the Rust migration. Serves the session
//! read/write path against the same SQLite store as the Bun server:
//!
//!   GET    /session            list sessions for the resolved project
//!   GET    /session/:id        fetch one session
//!   POST   /session            create a session (durable row, Bun-visible)
//!   PATCH  /session/:id        update title / metadata / permission
//!
//! Usage: opencode-server --db <path> --directory <cwd> [--port <port>]

mod b64;
mod bus;
mod config;
mod file;
mod float_check;
mod identifier;
mod message;
mod project;
mod provider;
mod runner;
mod session;
mod slug;
mod v2;
mod vcs;

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use r2d2_sqlite::SqliteConnectionManager;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

type Pool = r2d2::Pool<SqliteConnectionManager>;

#[derive(Clone)]
struct App {
    pool: Pool,
    bus: bus::Bus,
    project_id: String,
    directory: String,
    worktree: String,
    path: String,
    version: String,
    port: u16,
    paths: config::Paths,
}

struct Failure(StatusCode, String);

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "message": self.1 }))).into_response()
    }
}

impl From<r2d2::Error> for Failure {
    fn from(error: r2d2::Error) -> Self {
        Failure(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
    }
}

impl From<rusqlite::Error> for Failure {
    fn from(error: rusqlite::Error) -> Self {
        Failure(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_millis() as i64
}

/// Longest-prefix project resolution, mirroring ProjectV2.fromDirectory: the
/// project whose worktree contains the directory wins; "/" (global) matches
/// everything as the fallback.
fn resolve_project(
    conn: &rusqlite::Connection,
    directory: &str,
) -> rusqlite::Result<(String, String)> {
    let mut statement = conn.prepare("SELECT id, worktree FROM project")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut best: Option<(String, String)> = None;
    for row in rows {
        let (id, worktree) = row?;
        let matches = worktree == "/"
            || directory == worktree
            || directory.starts_with(&format!("{}/", worktree.trim_end_matches('/')));
        if !matches {
            continue;
        }
        if best
            .as_ref()
            .is_none_or(|(_, current)| worktree.len() > current.len())
        {
            best = Some((id, worktree));
        }
    }
    Ok(best.expect("no project matches directory; run the Bun server once to initialize"))
}

#[derive(Deserialize)]
struct ListQuery {
    /// Instance routing directory; when present the Bun server binds the
    /// request to that directory's instance and filters sessions by it.
    directory: Option<String>,
    scope: Option<String>,
    path: Option<String>,
    roots: Option<bool>,
    start: Option<i64>,
    search: Option<String>,
    limit: Option<i64>,
}

async fn list_sessions(
    State(app): State<App>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, Failure> {
    let conn = app.pool.get()?;
    let project_scope = query.scope.as_deref() == Some("project");
    let filter = session::ListFilter {
        directory: query.directory.filter(|_| !project_scope),
        path: query.path,
        roots: query.roots.unwrap_or(false),
        start: query.start,
        search: query.search,
        limit: query.limit,
    };
    let items = session::list(&conn, &app.project_id, &filter)?;
    Ok(Json(serde_json::to_value(items).expect("serializable")))
}

async fn get_session(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    let conn = app.pool.get()?;
    let found = session::get(&conn, &id)?
        .ok_or_else(|| Failure(StatusCode::NOT_FOUND, format!("Session not found: {id}")))?;
    Ok(Json(serde_json::to_value(found).expect("serializable")))
}

#[derive(Deserialize)]
struct CreatePayload {
    title: Option<String>,
    agent: Option<String>,
    #[serde(rename = "parentID")]
    parent_id: Option<String>,
    metadata: Option<Value>,
    permission: Option<Value>,
}

async fn create_session(
    State(app): State<App>,
    Json(payload): Json<CreatePayload>,
) -> Result<Json<Value>, Failure> {
    let conn = app.pool.get()?;
    let id = format!("ses_{}", identifier::descending());
    let timestamp = now();
    let iso = chrono_iso(timestamp);
    let title = payload.title.unwrap_or_else(|| {
        let prefix = if payload.parent_id.is_some() {
            "Child session - "
        } else {
            "New session - "
        };
        format!("{prefix}{iso}")
    });
    conn.execute(
        "INSERT INTO session (id, project_id, parent_id, slug, directory, path, title, version, \
         metadata, permission, agent, cost, tokens_input, tokens_output, tokens_reasoning, \
         tokens_cache_read, tokens_cache_write, time_created, time_updated) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, 0, 0, 0, 0, 0, ?, ?)",
        rusqlite::params![
            id,
            app.project_id,
            payload.parent_id,
            slug::create(),
            app.directory,
            app.path,
            title,
            app.version,
            payload.metadata.map(|value| value.to_string()),
            payload.permission.map(|value| value.to_string()),
            payload.agent,
            timestamp,
            timestamp,
        ],
    )?;
    let created = session::get(&conn, &id)?.expect("row just inserted");
    let info = serde_json::to_value(&created).expect("serializable");
    publish_session_event(&app, "session.created", &id, &info)?;
    Ok(Json(info))
}

#[derive(Deserialize)]
struct UpdatePayload {
    title: Option<String>,
    metadata: Option<Value>,
    permission: Option<Value>,
}

async fn update_session(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(payload): Json<UpdatePayload>,
) -> Result<Json<Value>, Failure> {
    let conn = app.pool.get()?;
    if session::get(&conn, &id)?.is_none() {
        return Err(Failure(
            StatusCode::NOT_FOUND,
            format!("Session not found: {id}"),
        ));
    }
    let timestamp = now();
    if let Some(title) = payload.title {
        conn.execute(
            "UPDATE session SET title = ?, time_updated = ? WHERE id = ?",
            rusqlite::params![title, timestamp, id],
        )?;
    }
    if let Some(metadata) = payload.metadata {
        conn.execute(
            "UPDATE session SET metadata = ?, time_updated = ? WHERE id = ?",
            rusqlite::params![metadata.to_string(), timestamp, id],
        )?;
    }
    if let Some(permission) = payload.permission {
        conn.execute(
            "UPDATE session SET permission = ?, time_updated = ? WHERE id = ?",
            rusqlite::params![permission.to_string(), timestamp, id],
        )?;
    }
    let updated = session::get(&conn, &id)?.expect("row exists");
    let info = serde_json::to_value(&updated).expect("serializable");
    publish_session_event(&app, "session.updated", &id, &info)?;
    Ok(Json(info))
}

fn publish_session_event(
    app: &App,
    event_type: &str,
    session_id: &str,
    info: &Value,
) -> Result<(), Failure> {
    let mut conn = app.pool.get()?;
    app.bus.publish_durable(
        &mut conn,
        event_type,
        session_id,
        json!({ "sessionID": session_id, "info": info }),
    )?;
    Ok(())
}

/// Port of Session.remove: depth-first child removal, a session.deleted event
/// per session, then row + durable-history deletion (messages/parts/todos
/// cascade from the session row).
fn remove_session(app: &App, id: &str) -> Result<(), Failure> {
    let conn = app.pool.get()?;
    let Some(info) = session::get(&conn, id)? else {
        return Ok(());
    };
    for child in session::children(&conn, id)? {
        remove_session(app, &child.id)?;
    }
    let info = serde_json::to_value(&info).expect("serializable");
    publish_session_event(app, "session.deleted", id, &info)?;
    app.bus.remove_aggregate(&conn, id)?;
    conn.execute("DELETE FROM session WHERE id = ?", [id])?;
    Ok(())
}

async fn delete_session(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    {
        let conn = app.pool.get()?;
        require_session(&conn, &id)?;
    }
    remove_session(&app, &id)?;
    Ok(Json(Value::Bool(true)))
}

async fn session_status(State(app): State<App>) -> Result<Json<Value>, Failure> {
    // Prompt execution is not ported yet, so no session can be busy; the Bun
    // server returns an empty object in the same idle state.
    let _ = app;
    Ok(Json(json!({})))
}

async fn event_stream(
    State(app): State<App>,
) -> axum::response::sse::Sse<
    impl futures::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>,
> {
    use futures::StreamExt;
    let receiver = app.bus.subscribe();
    let connected = futures::stream::once(async move {
        Ok(axum::response::sse::Event::default().data(bus::connected_event().to_string()))
    });
    let events =
        tokio_stream::wrappers::BroadcastStream::new(receiver).filter_map(|item| async move {
            match item {
                Ok(value) => Some(Ok(
                    axum::response::sse::Event::default().data(value.to_string())
                )),
                Err(_) => None,
            }
        });
    axum::response::sse::Sse::new(connected.chain(events))
}

async fn session_children(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    let conn = app.pool.get()?;
    require_session(&conn, &id)?;
    let items = session::children(&conn, &id)?;
    Ok(Json(serde_json::to_value(items).expect("serializable")))
}

async fn session_todo(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    let conn = app.pool.get()?;
    require_session(&conn, &id)?;
    Ok(Json(Value::Array(message::todos(&conn, &id)?)))
}

fn require_session(conn: &rusqlite::Connection, id: &str) -> Result<(), Failure> {
    match session::get(conn, id)? {
        Some(_) => Ok(()),
        None => Err(Failure(
            StatusCode::NOT_FOUND,
            format!("Session not found: {id}"),
        )),
    }
}

fn encode_cursor(cursor: &(String, i64)) -> String {
    b64::encode(&format!(
        "{{\"id\":\"{}\",\"time\":{}}}",
        cursor.0, cursor.1
    ))
}

fn decode_cursor(input: &str) -> Option<(String, i64)> {
    let parsed: Value = serde_json::from_slice(&b64::decode(input)?).ok()?;
    Some((
        parsed.get("id")?.as_str()?.to_string(),
        parsed.get("time")?.as_i64()?,
    ))
}

#[derive(Deserialize)]
struct MessagesQuery {
    limit: Option<i64>,
    before: Option<String>,
}

async fn session_messages(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(query): Query<MessagesQuery>,
) -> Result<Response, Failure> {
    let conn = app.pool.get()?;
    if query.before.is_some() && query.limit.is_none() {
        return Err(Failure(
            StatusCode::BAD_REQUEST,
            "before requires limit".into(),
        ));
    }
    let before = match query.before.as_deref() {
        Some(raw) => {
            Some(decode_cursor(raw).ok_or(Failure(StatusCode::BAD_REQUEST, "bad cursor".into()))?)
        }
        None => None,
    };
    require_session(&conn, &id)?;

    let limit = query.limit.filter(|value| *value != 0);
    let Some(limit) = limit else {
        let items = message::all(&conn, &id)?
            .ok_or_else(|| Failure(StatusCode::NOT_FOUND, format!("Session not found: {id}")))?;
        return Ok(Json(Value::Array(items)).into_response());
    };

    let page = message::page(&conn, &id, limit, before)?
        .ok_or_else(|| Failure(StatusCode::NOT_FOUND, format!("Session not found: {id}")))?;
    let mut response = Json(Value::Array(page.items)).into_response();
    if let Some(cursor) = page.cursor {
        let encoded = encode_cursor(&cursor);
        let link = format!(
            "<http://127.0.0.1:{}/session/{}/message?limit={}&before={}>; rel=\"next\"",
            app.port, id, limit, encoded
        );
        let headers = response.headers_mut();
        headers.insert(
            "Access-Control-Expose-Headers",
            "Link, X-Next-Cursor".parse().expect("header"),
        );
        headers.insert("Link", link.parse().expect("header"));
        headers.insert("X-Next-Cursor", encoded.parse().expect("header"));
    }
    Ok(response)
}

async fn session_message(
    State(app): State<App>,
    Path((id, message_id)): Path<(String, String)>,
) -> Result<Json<Value>, Failure> {
    let conn = app.pool.get()?;
    let found = message::get(&conn, &id, &message_id)?.ok_or_else(|| {
        Failure(
            StatusCode::NOT_FOUND,
            format!("Message not found: {message_id}"),
        )
    })?;
    Ok(Json(found))
}

async fn project_list(State(app): State<App>) -> Result<Json<Value>, Failure> {
    let conn = app.pool.get()?;
    Ok(Json(
        serde_json::to_value(project::list(&conn)?).expect("serializable"),
    ))
}

async fn project_current(State(app): State<App>) -> Result<Json<Value>, Failure> {
    let conn = app.pool.get()?;
    let found = project::get(&conn, &app.project_id)?
        .ok_or_else(|| Failure(StatusCode::NOT_FOUND, "Project not found".into()))?;
    Ok(Json(serde_json::to_value(found).expect("serializable")))
}

async fn health(State(app): State<App>) -> Json<Value> {
    Json(json!({ "healthy": true, "version": app.version }))
}

async fn config_get(State(app): State<App>) -> Json<Value> {
    Json(config::instance(&app.directory, &app.worktree))
}

async fn config_update(
    State(app): State<App>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, Failure> {
    Ok(Json(
        config::update_instance(&app.directory, payload)
            .map_err(|error| Failure(StatusCode::BAD_REQUEST, error.to_string()))?,
    ))
}

async fn global_config_get() -> Json<Value> {
    Json(config::global())
}

async fn global_config_update(Json(payload): Json<Value>) -> Result<Json<Value>, Failure> {
    Ok(Json(config::update_global(payload).map_err(|error| {
        Failure(StatusCode::BAD_REQUEST, error.to_string())
    })?))
}

async fn provider_list(State(app): State<App>) -> Json<Value> {
    Json(provider::list(&config::instance(
        &app.directory,
        &app.worktree,
    )))
}

async fn provider_auth(State(app): State<App>) -> Json<Value> {
    Json(provider::auth_methods(&config::instance(
        &app.directory,
        &app.worktree,
    )))
}

async fn provider_authorize() -> Json<Value> {
    Json(Value::Null)
}

async fn provider_callback() -> Json<Value> {
    Json(Value::Bool(true))
}

async fn config_providers(State(app): State<App>) -> Json<Value> {
    Json(provider::config_providers(&config::instance(
        &app.directory,
        &app.worktree,
    )))
}

async fn path_info(State(app): State<App>) -> Json<Value> {
    Json(json!({
        "home": app.paths.home,
        "state": app.paths.state,
        "config": app.paths.config,
        "worktree": app.worktree,
        "directory": app.directory,
    }))
}

async fn vcs_info(State(app): State<App>) -> Json<Value> {
    Json(vcs::info(&app.directory))
}

async fn vcs_status(State(app): State<App>) -> Json<Value> {
    Json(Value::Array(vcs::status(&app.directory)))
}

#[derive(Deserialize)]
struct VcsDiffQuery {
    mode: String,
    context: Option<i64>,
}

async fn vcs_diff(State(app): State<App>, Query(query): Query<VcsDiffQuery>) -> Json<Value> {
    Json(Value::Array(vcs::diff(
        &app.directory,
        &query.mode,
        query.context,
    )))
}

async fn vcs_diff_raw(State(app): State<App>) -> Response {
    (
        [(header::CONTENT_TYPE, "text/x-diff; charset=utf-8")],
        vcs::diff_raw(&app.directory),
    )
        .into_response()
}

async fn vcs_apply() -> Json<Value> {
    Json(json!({ "applied": false }))
}

async fn command_list(State(app): State<App>) -> Json<Value> {
    let config = config::instance(&app.directory, &app.worktree);
    let configured = config
        .get("command")
        .and_then(Value::as_object)
        .map(|commands| {
            commands
                .iter()
                .map(|(name, command)| {
                    json!({
                        "name": name,
                        "description": command.get("description").cloned().unwrap_or(Value::Null),
                        "agent": command.get("agent").cloned().unwrap_or(Value::Null),
                        "model": command.get("model").cloned().unwrap_or(Value::Null),
                        "source": "command",
                        "template": command.get("template").cloned().unwrap_or(Value::String(String::new())),
                        "subtask": command.get("subtask").cloned().unwrap_or(Value::Null),
                        "hints": [],
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Json(Value::Array(
        [
            json!({
                "name": "init",
                "description": "guided AGENTS.md setup",
                "source": "command",
                "template": "",
                "hints": [],
            }),
            json!({
                "name": "review",
                "description": "review changes [commit|branch|pr], defaults to uncommitted",
                "source": "command",
                "template": "",
                "subtask": true,
                "hints": [],
            }),
            json!({
                "name": "goal",
                "description": "manage the session goal: set, edit, pause, resume, complete, status, clear",
                "source": "command",
                "template": "$ARGUMENTS",
                "hints": ["$ARGUMENTS"],
            }),
            json!({
                "name": "customize-opencode",
                "description": "Use ONLY when the user is editing or creating opencode's own configuration: opencode.json, opencode.jsonc, files under .opencode/, or files under ~/.config/opencode/.",
                "source": "skill",
                "template": "",
                "hints": [],
            }),
        ]
        .into_iter()
        .chain(markdown_commands(&app.worktree))
        .chain(markdown_skill_commands(&app.worktree))
        .chain(configured)
        .collect(),
    ))
}

async fn agent_list(State(app): State<App>) -> Json<Value> {
    Json(Value::Array(
        [
            json!({
                "name": "build",
                "description": "The default agent. Executes tools based on configured permissions.",
                "mode": "primary",
                "native": true,
                "permission": [],
                "options": {},
            }),
            json!({
                "name": "plan",
                "description": "Plan mode. Disallows all edit tools.",
                "mode": "primary",
                "native": true,
                "color": "warning",
                "permission": [],
                "options": {},
            }),
            json!({
                "name": "goal",
                "description": "Goal mode. Works against a durable session goal that can be paused, edited, resumed, or completed.",
                "mode": "primary",
                "native": true,
                "color": "accent",
                "permission": [],
                "options": {},
            }),
            json!({
                "name": "general",
                "description": "General-purpose agent for researching complex questions and executing multi-step tasks. Use this agent to execute multiple units of work in parallel.",
                "mode": "subagent",
                "native": true,
                "permission": [],
                "options": {},
            }),
            json!({
                "name": "explore",
                "description": "Fast agent specialized for exploring codebases. Use this when you need to quickly find files by patterns (eg. \"src/components/**/*.tsx\"), search code for keywords (eg. \"API endpoints\"), or answer questions about the codebase (eg. \"how do API endpoints work?\"). When calling this agent, specify the desired thoroughness level: \"quick\" for basic searches, \"medium\" for moderate exploration, or \"very thorough\" for comprehensive analysis across multiple locations and naming conventions.",
                "mode": "subagent",
                "native": true,
                "permission": [],
                "options": {},
            }),
            json!({
                "name": "compaction",
                "mode": "primary",
                "native": true,
                "hidden": true,
                "permission": [],
                "options": {},
            }),
            json!({
                "name": "title",
                "mode": "primary",
                "native": true,
                "hidden": true,
                "temperature": 0.5,
                "permission": [],
                "options": {},
            }),
            json!({
                "name": "summary",
                "mode": "primary",
                "native": true,
                "hidden": true,
                "permission": [],
                "options": {},
            }),
        ]
        .into_iter()
        .chain(markdown_agents(&app.worktree))
        .collect(),
    ))
}

async fn skill_list(State(app): State<App>) -> Json<Value> {
    Json(Value::Array(markdown_skills(&app.worktree)))
}

async fn empty_array() -> Json<Value> {
    Json(Value::Array(vec![]))
}

async fn dispose_true() -> Json<Value> {
    Json(Value::Bool(true))
}

async fn global_upgrade() -> Json<Value> {
    Json(json!({ "success": false, "error": "Rust server upgrade is not implemented" }))
}

fn markdown_commands(worktree: &str) -> Vec<Value> {
    markdown_files(&std::path::Path::new(worktree).join(".opencode/command"))
        .into_iter()
        .filter_map(|path| {
            let parsed = parse_markdown(&path)?;
            let name = path.file_stem()?.to_string_lossy().into_owned();
            Some(json!({
                "name": parsed.frontmatter.get("name").and_then(Value::as_str).unwrap_or(&name),
                "description": parsed.frontmatter.get("description").cloned().unwrap_or(Value::Null),
                "agent": parsed.frontmatter.get("agent").cloned().unwrap_or(Value::Null),
                "model": parsed.frontmatter.get("model").cloned().unwrap_or(Value::Null),
                "source": "command",
                "template": parsed.content.trim(),
                "subtask": parsed.frontmatter.get("subtask").cloned().unwrap_or(Value::Null),
                "hints": command_hints(&parsed.content),
            }))
        })
        .collect()
}

fn markdown_agents(worktree: &str) -> Vec<Value> {
    markdown_files(&std::path::Path::new(worktree).join(".opencode/agent"))
        .into_iter()
        .filter_map(|path| {
            let parsed = parse_markdown(&path)?;
            let name = path.file_stem()?.to_string_lossy().into_owned();
            Some(json!({
                "name": parsed.frontmatter.get("name").and_then(Value::as_str).unwrap_or(&name),
                "description": parsed.frontmatter.get("description").cloned().unwrap_or(Value::Null),
                "mode": parsed.frontmatter.get("mode").cloned().unwrap_or(Value::String("all".into())),
                "native": false,
                "hidden": parsed.frontmatter.get("hidden").cloned().unwrap_or(Value::Null),
                "prompt": parsed.content.trim(),
                "permission": [],
                "options": {},
            }))
        })
        .collect()
}

pub fn markdown_skills(worktree: &str) -> Vec<Value> {
    markdown_files(&std::path::Path::new(worktree).join(".opencode/skills"))
        .into_iter()
        .filter(|path| path.file_name().is_some_and(|name| name == "SKILL.md"))
        .filter_map(|path| {
            let parsed = parse_markdown(&path)?;
            let name = parsed.frontmatter.get("name")?.as_str()?;
            Some(json!({
                "name": name,
                "description": parsed.frontmatter.get("description").cloned().unwrap_or(Value::Null),
                "location": path.to_string_lossy(),
                "content": parsed.content,
            }))
        })
        .collect()
}

fn markdown_skill_commands(worktree: &str) -> Vec<Value> {
    markdown_skills(worktree)
        .into_iter()
        .map(|skill| {
            json!({
                "name": skill.get("name").cloned().unwrap_or(Value::String(String::new())),
                "description": skill.get("description").cloned().unwrap_or(Value::Null),
                "source": "skill",
                "template": skill.get("content").cloned().unwrap_or(Value::String(String::new())),
                "hints": [],
            })
        })
        .collect()
}

fn markdown_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return vec![];
    };
    entries
        .filter_map(Result::ok)
        .flat_map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                return markdown_files(&path);
            }
            if path.extension().is_some_and(|extension| extension == "md") {
                return vec![path];
            }
            vec![]
        })
        .collect()
}

struct Markdown {
    frontmatter: serde_json::Map<String, Value>,
    content: String,
}

fn parse_markdown(path: &std::path::Path) -> Option<Markdown> {
    let text = std::fs::read_to_string(path).ok()?;
    if !text.starts_with("---\n") {
        return Some(Markdown {
            frontmatter: serde_json::Map::new(),
            content: text,
        });
    }
    let rest = &text[4..];
    let (frontmatter, content) = rest.split_once("\n---\n")?;
    Some(Markdown {
        frontmatter: frontmatter
            .lines()
            .filter_map(|line| line.split_once(':'))
            .map(|(key, value)| {
                let value = value.trim().trim_matches('"').trim_matches('\'');
                (
                    key.trim().to_string(),
                    match value {
                        "true" => Value::Bool(true),
                        "false" => Value::Bool(false),
                        _ => Value::String(value.to_string()),
                    },
                )
            })
            .collect(),
        content: content.to_string(),
    })
}

fn command_hints(template: &str) -> Vec<Value> {
    let mut hints = vec![];
    if template.contains("$ARGUMENTS") {
        hints.push(Value::String("$ARGUMENTS".into()));
    }
    for index in 1..10 {
        let hint = format!("${index}");
        if template.contains(&hint) {
            hints.push(Value::String(hint));
        }
    }
    hints
}

#[derive(Deserialize)]
struct FindTextQuery {
    pattern: String,
}

async fn find_text(
    State(app): State<App>,
    Query(query): Query<FindTextQuery>,
) -> Result<Json<Value>, Failure> {
    let directory = app.directory.clone();
    let items =
        tokio::task::spawn_blocking(move || file::find_text(&directory, &query.pattern, 10))
            .await
            .map_err(|error| Failure(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
            .map_err(|error| Failure(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(Value::Array(items)))
}

#[derive(Deserialize)]
struct FindFileQuery {
    query: String,
    dirs: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    limit: Option<usize>,
}

async fn find_file(
    State(app): State<App>,
    Query(query): Query<FindFileQuery>,
) -> Result<Json<Value>, Failure> {
    let directory = app.directory.clone();
    let kind = query
        .kind
        .clone()
        .or_else(|| (query.dirs.as_deref() == Some("false")).then(|| "file".to_string()));
    let items = tokio::task::spawn_blocking(move || {
        file::find_file(
            &directory,
            &query.query,
            kind.as_deref(),
            query.limit.unwrap_or(10),
        )
    })
    .await
    .map_err(|error| Failure(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
    .map_err(|error| Failure(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(serde_json::to_value(items).expect("serializable")))
}

async fn find_symbol() -> Json<Value> {
    Json(Value::Array(vec![]))
}

#[derive(Deserialize)]
struct PathQuery {
    path: String,
}

async fn file_list(
    State(app): State<App>,
    Query(query): Query<PathQuery>,
) -> Result<Json<Value>, Failure> {
    let items = file::list(&app.directory, &app.worktree, &query.path)
        .map_err(|error| Failure(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(Value::Array(items)))
}

async fn file_content(
    State(app): State<App>,
    Query(query): Query<PathQuery>,
) -> Result<Json<Value>, Failure> {
    let found = file::content(&app.directory, &query.path)
        .map_err(|error| Failure(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(match found {
        file::Content::Missing => json!({ "type": "text", "content": "" }),
        file::Content::Text(content) => json!({ "type": "text", "content": content }),
        file::Content::Binary { base64, mime } => {
            json!({ "type": "binary", "content": base64, "encoding": "base64", "mimeType": mime })
        }
    }))
}

async fn file_status() -> Json<Value> {
    Json(Value::Array(vec![]))
}

// ---------------------------------------------------------------------------
// v2 (/api) surface — wire-parity port of packages/server handlers
// ---------------------------------------------------------------------------

/// Tagged error responses matching the effect HttpApi encoding of
/// Schema.TaggedErrorClass values (packages/protocol/src/errors.ts).
fn v2_session_not_found(session_id: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "_tag": "SessionNotFoundError",
            "sessionID": session_id,
            "message": format!("Session not found: {session_id}"),
        })),
    )
        .into_response()
}

fn v2_invalid_cursor(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "_tag": "InvalidCursorError", "message": message })),
    )
        .into_response()
}

async fn api_health() -> Json<Value> {
    Json(json!({ "healthy": true }))
}

async fn api_session_active() -> Json<Value> {
    let mut data = serde_json::Map::new();
    for session_id in runner::active() {
        data.insert(session_id, json!({ "type": "running" }));
    }
    Json(json!({ "data": data }))
}

#[derive(Deserialize)]
struct ApiSessionsQuery {
    workspace: Option<String>,
    limit: Option<i64>,
    order: Option<String>,
    search: Option<String>,
    directory: Option<String>,
    project: Option<String>,
    subpath: Option<String>,
    cursor: Option<String>,
}

async fn api_session_list(
    State(app): State<App>,
    Query(query): Query<ApiSessionsQuery>,
) -> Result<Response, Failure> {
    let resolved = match query.cursor.as_deref() {
        Some(cursor) => match v2::parse_cursor(cursor) {
            Some(parsed) => parsed,
            None => return Ok(v2_invalid_cursor("Invalid cursor")),
        },
        None => v2::ListQuery {
            workspace: query.workspace,
            order: query.order,
            search: query.search,
            directory: query.directory,
            project: query.project,
            subpath: query.subpath,
            anchor: None,
        },
    };
    let conn = app.pool.get()?;
    Ok(Json(v2::list(&conn, &resolved, query.limit.unwrap_or(50))?).into_response())
}

#[derive(Deserialize)]
struct ApiCreatePayload {
    id: Option<String>,
    agent: Option<String>,
    model: Option<Value>,
    location: Option<Value>,
}

async fn api_session_create(
    State(app): State<App>,
    Json(payload): Json<ApiCreatePayload>,
) -> Result<Response, Failure> {
    let conn = app.pool.get()?;
    // Reusing a Session ID adopts the existing Session.
    if let Some(id) = payload.id.as_deref() {
        if let Some(existing) = v2::get(&conn, id)? {
            return Ok(Json(json!({ "data": existing })).into_response());
        }
    }
    let directory = payload
        .location
        .as_ref()
        .and_then(|location| location.get("directory"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| app.directory.clone());
    let workspace_id = payload
        .location
        .as_ref()
        .and_then(|location| location.get("workspaceID"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let (project_id, worktree) = resolve_project(&conn, &directory)?;
    let path = directory
        .strip_prefix(worktree.trim_end_matches('/'))
        .map(|rest| rest.trim_start_matches('/').to_string())
        .unwrap_or_default();
    let id = payload
        .id
        .unwrap_or_else(|| format!("ses_{}", identifier::descending()));
    let timestamp = now();
    let model = payload.model.map(|model| {
        json!({
            "id": model.get("id").cloned().unwrap_or(Value::Null),
            "providerID": model.get("providerID").cloned().unwrap_or(Value::Null),
            "variant": model.get("variant").cloned().unwrap_or(Value::String("default".into())),
        })
    });
    conn.execute(
        "INSERT INTO session (id, project_id, workspace_id, slug, directory, path, title, \
         version, metadata, agent, model, cost, tokens_input, tokens_output, tokens_reasoning, \
         tokens_cache_read, tokens_cache_write, time_created, time_updated) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, 0, 0, 0, 0, 0, 0, ?, ?)",
        rusqlite::params![
            id,
            project_id,
            workspace_id,
            slug::create(),
            directory,
            path,
            format!("New session - {}", chrono_iso(timestamp)),
            app.version,
            payload.agent,
            model.map(|value| value.to_string()),
            timestamp,
            timestamp,
        ],
    )?;
    let v1_info = session::get(&conn, &id)?.expect("row just inserted");
    publish_session_event(
        &app,
        "session.created",
        &id,
        &serde_json::to_value(&v1_info).expect("serializable"),
    )?;
    let info = v2::get(&conn, &id)?.expect("row just inserted");
    Ok(Json(json!({ "data": info })).into_response())
}

async fn api_session_get(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Response, Failure> {
    let conn = app.pool.get()?;
    Ok(match v2::get(&conn, &id)? {
        Some(info) => Json(json!({ "data": info })).into_response(),
        None => v2_session_not_found(&id),
    })
}

async fn api_session_prompt(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(payload): Json<Value>,
) -> Result<Response, Failure> {
    let mut conn = app.pool.get()?;
    Ok(match v2::admit(&mut conn, &id, &payload) {
        Ok(admitted) => {
            // Admission schedules advisory SessionExecution.wake unless
            // resume: false requests admit-only behavior.
            if payload.get("resume").and_then(Value::as_bool) != Some(false) {
                runner::wake(
                    runner::Env {
                        pool: app.pool.clone(),
                        worktree: app.worktree.clone(),
                    },
                    id.clone(),
                );
            }
            Json(json!({ "data": admitted })).into_response()
        }
        Err(v2::AdmitError::NotFound) => v2_session_not_found(&id),
        Err(v2::AdmitError::Conflict(message_id)) => (
            StatusCode::CONFLICT,
            Json(json!({
                "_tag": "ConflictError",
                "message": format!(
                    "Prompt message ID conflicts with an existing durable record: {message_id}"
                ),
                "resource": message_id,
            })),
        )
            .into_response(),
        Err(v2::AdmitError::BadRequest(message)) => {
            (StatusCode::BAD_REQUEST, Json(json!({ "message": message }))).into_response()
        }
        Err(v2::AdmitError::Storage(message)) => {
            return Err(Failure(StatusCode::INTERNAL_SERVER_ERROR, message));
        }
    })
}

#[derive(Deserialize)]
struct ApiHistoryQuery {
    after: Option<i64>,
    limit: Option<i64>,
}

async fn api_session_history(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(query): Query<ApiHistoryQuery>,
) -> Result<Response, Failure> {
    let conn = app.pool.get()?;
    if v2::get(&conn, &id)?.is_none() {
        return Ok(v2_session_not_found(&id));
    }
    Ok(Json(v2::history(
        &conn,
        &id,
        query.after,
        query.limit.unwrap_or(50).clamp(1, 100),
    )?)
    .into_response())
}

#[derive(Deserialize)]
struct ApiMessagesQuery {
    limit: Option<i64>,
    order: Option<String>,
    cursor: Option<String>,
}

async fn api_session_messages(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(query): Query<ApiMessagesQuery>,
) -> Result<Response, Failure> {
    if query.cursor.is_some() && query.order.is_some() {
        return Ok(v2_invalid_cursor("Cursor cannot be combined with order"));
    }
    let decoded = match query.cursor.as_deref() {
        Some(cursor) => {
            let parsed = b64::decode(cursor)
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .and_then(|value| {
                    Some((
                        value.get("id")?.as_str()?.to_string(),
                        value.get("order")?.as_str()?.to_string(),
                        value.get("direction")?.as_str()?.to_string(),
                    ))
                });
            match parsed {
                Some(parsed) => Some(parsed),
                None => return Ok(v2_invalid_cursor("Invalid cursor")),
            }
        }
        None => None,
    };
    let conn = app.pool.get()?;
    if v2::get(&conn, &id)?.is_none() {
        return Ok(v2_session_not_found(&id));
    }
    let order = decoded
        .as_ref()
        .map(|(_, order, _)| order.clone())
        .or(query.order)
        .unwrap_or_else(|| "desc".into());
    let page = v2::messages(
        &conn,
        &id,
        &v2::MessagesQuery {
            limit: query.limit.unwrap_or(50).clamp(1, 200),
            order,
            cursor_id: decoded.as_ref().map(|(id, _, _)| id.clone()),
            direction: decoded.map(|(_, _, direction)| direction),
        },
    )?;
    Ok(Json(page.expect("session checked above")).into_response())
}

async fn api_session_message(
    State(app): State<App>,
    Path((id, message_id)): Path<(String, String)>,
) -> Result<Response, Failure> {
    let conn = app.pool.get()?;
    if v2::get(&conn, &id)?.is_none() {
        return Ok(v2_session_not_found(&id));
    }
    Ok(match v2::message(&conn, &id, &message_id)? {
        Some(message) => Json(json!({ "data": message })).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "_tag": "MessageNotFoundError",
                "sessionID": id,
                "messageID": message_id,
                "message": format!("Message not found: {message_id}"),
            })),
        )
            .into_response(),
    })
}

async fn api_session_interrupt(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Response, Failure> {
    let conn = app.pool.get()?;
    if v2::get(&conn, &id)?.is_none() {
        return Ok(v2_session_not_found(&id));
    }
    // V2 interruption targets the active process-local ownership chain. The
    // Rust drain runs provider turns to settlement; mid-turn cancellation is a
    // future slice, and idle or missing interruption is a no-op.
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn api_session_switch_agent(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(payload): Json<Value>,
) -> Result<Response, Failure> {
    let mut conn = app.pool.get()?;
    if v2::get(&conn, &id)?.is_none() {
        return Ok(v2_session_not_found(&id));
    }
    let Some(agent) = payload.get("agent").and_then(Value::as_str) else {
        return Err(Failure(StatusCode::BAD_REQUEST, "agent is required".into()));
    };
    let publisher = runner::publish::Publisher {
        session_id: id.clone(),
    };
    let mut data = serde_json::Map::new();
    data.insert("timestamp".into(), json!(now()));
    data.insert("sessionID".into(), json!(id));
    data.insert(
        "messageID".into(),
        json!(format!("msg_{}", identifier::ascending())),
    );
    data.insert("agent".into(), json!(agent));
    publisher
        .publish(
            &mut conn,
            "session.next.agent.switched",
            1,
            &Value::Object(data),
        )
        .map_err(|error| Failure(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn api_session_switch_model(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(payload): Json<Value>,
) -> Result<Response, Failure> {
    let mut conn = app.pool.get()?;
    if v2::get(&conn, &id)?.is_none() {
        return Ok(v2_session_not_found(&id));
    }
    let Some(model) = payload.get("model").filter(|model| model.is_object()) else {
        return Err(Failure(StatusCode::BAD_REQUEST, "model is required".into()));
    };
    // Model.Ref key order: id, providerID, variant?.
    let mut reference = serde_json::Map::new();
    reference.insert("id".into(), model.get("id").cloned().unwrap_or(Value::Null));
    reference.insert(
        "providerID".into(),
        model.get("providerID").cloned().unwrap_or(Value::Null),
    );
    if let Some(variant) = model.get("variant").filter(|value| !value.is_null()) {
        reference.insert("variant".into(), variant.clone());
    }
    let publisher = runner::publish::Publisher {
        session_id: id.clone(),
    };
    let mut data = serde_json::Map::new();
    data.insert("timestamp".into(), json!(now()));
    data.insert("sessionID".into(), json!(id));
    data.insert(
        "messageID".into(),
        json!(format!("msg_{}", identifier::ascending())),
    );
    data.insert("model".into(), Value::Object(reference));
    publisher
        .publish(
            &mut conn,
            "session.next.model.switched",
            1,
            &Value::Object(data),
        )
        .map_err(|error| Failure(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn api_session_wait(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Response, Failure> {
    {
        let conn = app.pool.get()?;
        if v2::get(&conn, &id)?.is_none() {
            return Ok(v2_session_not_found(&id));
        }
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
    while runner::is_active(&id) && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Millisecond-precision ISO-8601 UTC timestamp matching Date#toISOString,
/// without pulling in a date crate for one format.
fn chrono_iso(millis: i64) -> String {
    let days = millis.div_euclid(86_400_000);
    let ms_of_day = millis.rem_euclid(86_400_000);
    // Civil-from-days algorithm (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year,
        month,
        day,
        ms_of_day / 3_600_000,
        ms_of_day % 3_600_000 / 60_000,
        ms_of_day % 60_000 / 1000,
        ms_of_day % 1000
    )
}

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|item| item == name)
        .and_then(|index| args.get(index + 1).cloned())
}

#[tokio::main]
async fn main() {
    let db = arg("--db").unwrap_or_else(|| {
        let home = std::env::var("HOME").expect("HOME not set");
        format!("{home}/.local/share/opencode/opencode-local.db")
    });
    let directory = arg("--directory").unwrap_or_else(|| {
        std::env::current_dir()
            .expect("cwd")
            .to_string_lossy()
            .into_owned()
    });
    let port: u16 = arg("--port")
        .and_then(|value| value.parse().ok())
        .unwrap_or(4097);

    let manager = SqliteConnectionManager::file(&db).with_init(|conn| {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(())
    });
    let pool = r2d2::Pool::builder()
        .max_size(8)
        .build(manager)
        .expect("sqlite pool");

    let (project_id, worktree) = {
        let conn = pool.get().expect("sqlite connection");
        resolve_project(&conn, &directory).expect("project resolution")
    };
    let path = directory
        .strip_prefix(worktree.trim_end_matches('/'))
        .map(|rest| rest.trim_start_matches('/').to_string())
        .unwrap_or_default();

    let app = App {
        pool,
        bus: bus::Bus::new(),
        project_id: project_id.clone(),
        directory: directory.clone(),
        worktree: worktree.clone(),
        path,
        version: std::env::var("OPENCODE_VERSION").unwrap_or_else(|_| "local".into()),
        port,
        paths: config::paths(),
    };

    let router = Router::new()
        .route("/global/health", get(health))
        .route(
            "/global/config",
            get(global_config_get).patch(global_config_update),
        )
        .route("/global/dispose", get(dispose_true).post(dispose_true))
        .route("/global/upgrade", get(global_upgrade).post(global_upgrade))
        .route("/config", get(config_get).patch(config_update))
        .route("/config/providers", get(config_providers))
        .route("/provider", get(provider_list))
        .route("/provider/auth", get(provider_auth))
        .route(
            "/provider/{provider_id}/oauth/authorize",
            get(provider_authorize).post(provider_authorize),
        )
        .route(
            "/provider/{provider_id}/oauth/callback",
            get(provider_callback).post(provider_callback),
        )
        .route("/permission", get(empty_array))
        .route(
            "/permission/{request_id}/reply",
            get(dispose_true).post(dispose_true),
        )
        .route("/question", get(empty_array))
        .route(
            "/question/{request_id}/reply",
            get(dispose_true).post(dispose_true),
        )
        .route(
            "/question/{request_id}/reject",
            get(dispose_true).post(dispose_true),
        )
        .route("/instance/dispose", get(dispose_true).post(dispose_true))
        .route("/path", get(path_info))
        .route("/vcs", get(vcs_info))
        .route("/vcs/status", get(vcs_status))
        .route("/vcs/diff", get(vcs_diff))
        .route("/vcs/diff/raw", get(vcs_diff_raw))
        .route("/vcs/apply", get(vcs_apply).post(vcs_apply))
        .route("/command", get(command_list))
        .route("/agent", get(agent_list))
        .route("/skill", get(skill_list))
        .route("/lsp", get(empty_array))
        .route("/formatter", get(empty_array))
        .route("/session", get(list_sessions).post(create_session))
        .route("/session/status", get(session_status))
        .route(
            "/session/{id}",
            get(get_session)
                .patch(update_session)
                .delete(delete_session),
        )
        .route("/session/{id}/children", get(session_children))
        .route("/session/{id}/todo", get(session_todo))
        .route("/session/{id}/message", get(session_messages))
        .route("/session/{id}/message/{message_id}", get(session_message))
        .route("/project", get(project_list))
        .route("/project/current", get(project_current))
        .route("/event", get(event_stream))
        .route("/find", get(find_text))
        .route("/find/file", get(find_file))
        .route("/find/symbol", get(find_symbol))
        .route("/file", get(file_list))
        .route("/file/content", get(file_content))
        .route("/file/status", get(file_status))
        .route("/api/health", get(api_health))
        .route(
            "/api/session",
            get(api_session_list).post(api_session_create),
        )
        .route("/api/session/active", get(api_session_active))
        .route("/api/session/{id}", get(api_session_get))
        .route("/api/session/{id}/prompt", post(api_session_prompt))
        .route("/api/session/{id}/history", get(api_session_history))
        .route("/api/session/{id}/message", get(api_session_messages))
        .route(
            "/api/session/{id}/message/{message_id}",
            get(api_session_message),
        )
        .route("/api/session/{id}/interrupt", post(api_session_interrupt))
        .route("/api/session/{id}/wait", post(api_session_wait))
        .route("/api/session/{id}/agent", post(api_session_switch_agent))
        .route("/api/session/{id}/model", post(api_session_switch_model))
        .with_state(app);

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("bind");
    println!("opencode-server (rust) listening on http://127.0.0.1:{port} project={project_id} directory={directory}");
    axum::serve(listener, router).await.expect("serve");
}

#[cfg(test)]
mod tests {
    use super::chrono_iso;

    #[test]
    fn iso_matches_date_to_iso_string() {
        assert_eq!(chrono_iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(chrono_iso(1_751_659_200_123), "2025-07-04T20:00:00.123Z");
        assert_eq!(chrono_iso(1_783_197_020_091), "2026-07-04T20:30:20.091Z");
    }
}
