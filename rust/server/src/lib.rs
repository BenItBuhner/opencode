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

mod auth;
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
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use r2d2_sqlite::SqliteConnectionManager;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

fn official_agents(worktree: &str, directory: &str, config: &Value) -> Vec<Value> {
    [
        json!({
            "id": "build",
            "description": "The default agent. Executes tools based on configured permissions.",
            "mode": "primary",
            "hidden": false,
            "request": { "headers": {}, "body": {} },
            "permissions": [],
        }),
        json!({
            "id": "plan",
            "description": "Plan mode. Disallows all edit tools.",
            "mode": "primary",
            "hidden": false,
            "color": "warning",
            "request": { "headers": {}, "body": {} },
            "permissions": [],
        }),
        json!({
            "id": "goal",
            "description": "Goal mode. Works against a durable session goal that can be paused, edited, resumed, or completed.",
            "mode": "primary",
            "hidden": false,
            "color": "accent",
            "request": { "headers": {}, "body": {} },
            "permissions": [],
        }),
        json!({
            "id": "general",
            "description": "General-purpose agent for researching complex questions and executing multi-step tasks. Use this agent to execute multiple units of work in parallel.",
            "mode": "subagent",
            "hidden": false,
            "request": { "headers": {}, "body": {} },
            "permissions": [],
        }),
        json!({
            "id": "explore",
            "description": "Fast agent specialized for exploring codebases. Use this when you need to quickly find files by patterns (eg. \"src/components/**/*.tsx\"), search code for keywords (eg. \"API endpoints\"), or answer questions about the codebase (eg. \"how do API endpoints work?\"). When calling this agent, specify the desired thoroughness level: \"quick\" for basic searches, \"medium\" for moderate exploration, or \"very thorough\" for comprehensive analysis across multiple locations and naming conventions.",
            "mode": "subagent",
            "hidden": false,
            "request": { "headers": {}, "body": {} },
            "permissions": [],
        }),
        json!({ "id": "compaction", "mode": "primary", "hidden": true, "request": { "headers": {}, "body": {} }, "permissions": [] }),
        json!({ "id": "title", "mode": "primary", "hidden": true, "request": { "headers": {}, "body": {} }, "permissions": [] }),
        json!({ "id": "summary", "mode": "primary", "hidden": true, "request": { "headers": {}, "body": {} }, "permissions": [] }),
    ]
    .into_iter()
    .map(|mut agent| {
        let id = agent
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if let Some(system) = runner::agent_system(&id) {
            agent["system"] = Value::String(system.into());
        }
        let mut permissions = runner::permission::agent_rules(&id)
            .into_iter()
            .map(|rule| {
                json!({
                    "action": rule.action,
                    "resource": rule.resource,
                    "effect": rule.effect,
                })
            })
            .collect::<Vec<_>>();
        if id == "plan" {
            let plans = format!("{}/.local/share/opencode/plans", app_home());
            let edit = permissions
                .iter()
                .position(|rule| {
                    rule.get("action").and_then(Value::as_str) == Some("edit")
                        && rule.get("resource").and_then(Value::as_str) == Some("*")
                })
                .unwrap_or(permissions.len());
            permissions.insert(
                edit,
                json!({
                    "action": "external_directory",
                    "resource": format!("{plans}/*"),
                    "effect": "allow",
                }),
            );
            permissions.insert(
                edit + 3,
                json!({
                    "action": "edit",
                    "resource": format!("{}/*.md", relative_path(directory, &plans)),
                    "effect": "allow",
                }),
            );
        }
        permissions.extend(configured_tool_permissions(config));
        agent["permissions"] = Value::Array(permissions);
        agent
    })
    .chain(
        markdown_agents(worktree)
            .into_iter()
            .map(|agent| official_markdown_agent(agent, config)),
    )
    .collect()
}

fn app_home() -> String {
    std::env::var("OPENCODE_TEST_HOME")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default()
}

fn relative_path(base: &str, target: &str) -> String {
    let base = std::path::Path::new(base).components().collect::<Vec<_>>();
    let target = std::path::Path::new(target)
        .components()
        .collect::<Vec<_>>();
    let common = base
        .iter()
        .zip(&target)
        .take_while(|(left, right)| left == right)
        .count();
    std::iter::repeat_n("..", base.len().saturating_sub(common))
        .map(str::to_string)
        .chain(
            target[common..]
                .iter()
                .map(|part| part.as_os_str().to_string_lossy().into_owned()),
        )
        .collect::<Vec<_>>()
        .join("/")
}

fn official_markdown_agent(agent: Value, config: &Value) -> Value {
    let mut info = serde_json::Map::new();
    info.insert(
        "id".into(),
        agent
            .get("name")
            .cloned()
            .unwrap_or_else(|| Value::String(String::new())),
    );
    if let Some(description) = agent.get("description").filter(|value| !value.is_null()) {
        info.insert("description".into(), description.clone());
    }
    info.insert(
        "mode".into(),
        agent
            .get("mode")
            .cloned()
            .unwrap_or_else(|| Value::String("all".into())),
    );
    info.insert(
        "hidden".into(),
        agent
            .get("hidden")
            .cloned()
            .filter(|value| !value.is_null())
            .unwrap_or(Value::Bool(false)),
    );
    if let Some(prompt) = agent.get("prompt").filter(|value| !value.is_null()) {
        info.insert("system".into(), prompt.clone());
    }
    if let Some(model) = agent.get("model").and_then(Value::as_str) {
        if let Some((provider_id, id)) = model.split_once('/') {
            info.insert(
                "model".into(),
                json!({ "id": id, "providerID": provider_id }),
            );
        }
    }
    if let Some(color) = agent.get("color").filter(|value| !value.is_null()) {
        info.insert("color".into(), color.clone());
    }
    info.insert("request".into(), json!({ "headers": {}, "body": {} }));
    let mut permissions = configured_tool_permissions(config);
    permissions.extend(
        agent
            .get("tools")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|tools| tools.iter())
            .filter_map(|(action, enabled)| {
                Some(json!({
                    "action": action,
                    "resource": "*",
                    "effect": if enabled.as_bool()? { "allow" } else { "deny" },
                }))
            }),
    );
    info.insert("permissions".into(), Value::Array(permissions));
    Value::Object(info)
}

fn configured_tool_permissions(config: &Value) -> Vec<Value> {
    config
        .get("tools")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|tools| tools.iter())
        .filter_map(|(action, enabled)| {
            Some(json!({
                "action": action,
                "resource": "*",
                "effect": if enabled.as_bool()? { "allow" } else { "deny" },
            }))
        })
        .collect()
}

fn official_command(command: &Value) -> Value {
    let mut info = serde_json::Map::new();
    for field in [
        "name",
        "template",
        "description",
        "agent",
        "model",
        "subtask",
    ] {
        if let Some(value) = command.get(field).filter(|value| !value.is_null()) {
            let value = if field == "model" {
                value
                    .as_str()
                    .and_then(|model| model.split_once('/'))
                    .map(|(provider_id, id)| json!({ "id": id, "providerID": provider_id }))
                    .unwrap_or_else(|| value.clone())
            } else {
                value.clone()
            };
            info.insert(field.into(), value);
        }
    }
    info.entry("template")
        .or_insert_with(|| Value::String(String::new()));
    Value::Object(info)
}

fn official_skill(skill: &Value) -> Value {
    let mut info = serde_json::Map::new();
    for field in ["name", "description", "slash", "location", "content"] {
        if let Some(value) = skill.get(field).filter(|value| !value.is_null()) {
            info.insert(field.into(), value.clone());
        }
    }
    Value::Object(info)
}

fn reference_list(config: &Value, paths: &config::Paths) -> Vec<Value> {
    config
        .get("references")
        .and_then(Value::as_object)
        .map(|references| {
            references
                .iter()
                .filter_map(|(name, reference)| {
                    let description = reference.get("description").cloned();
                    let hidden = reference.get("hidden").cloned();
                    let (path, mut source) = if let Some(path) =
                        reference.get("path").and_then(Value::as_str)
                    {
                        let path = path
                            .strip_prefix("~/")
                            .map(|suffix| format!("{}/{suffix}", paths.home))
                            .unwrap_or_else(|| path.to_string());
                        (path.clone(), json!({ "type": "local", "path": path }))
                    } else {
                        let repository = reference.get("repository")?.as_str()?;
                        (
                            format!("{}/.local/share/opencode/repos/{}", paths.home, repository),
                            json!({ "type": "git", "repository": repository }),
                        )
                    };
                    if let Some(description) = &description {
                        source["description"] = description.clone();
                    }
                    if let Some(branch) = reference.get("branch").filter(|value| !value.is_null()) {
                        source["branch"] = branch.clone();
                    }
                    if let Some(hidden) = &hidden {
                        source["hidden"] = hidden.clone();
                    }
                    let mut info = serde_json::Map::new();
                    info.insert("name".into(), Value::String(name.clone()));
                    info.insert("path".into(), Value::String(path));
                    if let Some(description) = description {
                        info.insert("description".into(), description);
                    }
                    if let Some(hidden) = hidden {
                        info.insert("hidden".into(), hidden);
                    }
                    info.insert("source".into(), source);
                    Some(Value::Object(info))
                })
                .collect()
        })
        .unwrap_or_default()
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
                "model": parsed.frontmatter.get("model").cloned().unwrap_or(Value::Null),
                "color": parsed.frontmatter.get("color").cloned().unwrap_or(Value::Null),
                "tools": parsed.frontmatter.get("tools").cloned().unwrap_or(Value::Null),
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
        frontmatter: parse_frontmatter(frontmatter),
        content: content.to_string(),
    })
}

fn parse_frontmatter(frontmatter: &str) -> serde_json::Map<String, Value> {
    let mut result = serde_json::Map::new();
    let mut section: Option<String> = None;
    for line in frontmatter.lines() {
        let nested = line.starts_with(' ') || line.starts_with('\t');
        let Some((key, value)) = line.trim().split_once(':') else {
            continue;
        };
        let key = key.trim().trim_matches('"').trim_matches('\'');
        let value = value.trim().trim_matches('"').trim_matches('\'');
        if nested {
            if let Some(object) = section
                .as_ref()
                .and_then(|section| result.get_mut(section))
                .and_then(Value::as_object_mut)
            {
                object.insert(key.to_string(), frontmatter_value(value));
            }
            continue;
        }
        if value.is_empty() {
            result.insert(key.to_string(), Value::Object(serde_json::Map::new()));
            section = Some(key.to_string());
            continue;
        }
        result.insert(key.to_string(), frontmatter_value(value));
        section = None;
    }
    result
}

fn frontmatter_value(value: &str) -> Value {
    match value {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => Value::String(value.to_string()),
    }
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

#[derive(Deserialize, Default)]
struct ApiLocationQuery {
    #[serde(rename = "location[directory]")]
    location_directory: Option<String>,
    #[serde(rename = "location[workspace]")]
    location_workspace: Option<String>,
}

fn location_directory(app: &App, headers: &HeaderMap, query: &ApiLocationQuery) -> String {
    query
        .location_directory
        .clone()
        .or_else(|| {
            headers
                .get("x-opencode-directory")
                .and_then(|value| value.to_str().ok())
                .map(decode_uri_component)
        })
        .unwrap_or_else(|| app.directory.clone())
}

fn location_info(app: &App, headers: &HeaderMap, query: &ApiLocationQuery) -> Value {
    let directory = location_directory(app, headers, query);
    let project = if directory == app.directory {
        ResolvedLocationProject {
            id: app.project_id.clone(),
            directory: app.worktree.clone(),
        }
    } else {
        resolve_location_project(&directory)
    };
    let mut info = serde_json::Map::new();
    info.insert("directory".into(), Value::String(directory));
    if let Some(workspace_id) = query.location_workspace.clone().or_else(|| {
        headers
            .get("x-opencode-workspace")
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string)
    }) {
        info.insert("workspaceID".into(), Value::String(workspace_id));
    }
    info.insert(
        "project".into(),
        json!({ "id": project.id, "directory": project.directory }),
    );
    Value::Object(info)
}

fn location_response(
    app: &App,
    headers: &HeaderMap,
    query: &ApiLocationQuery,
    data: Value,
) -> Value {
    json!({ "location": location_info(app, headers, query), "data": data })
}

fn scoped_app(app: &App, headers: &HeaderMap, query: &ApiLocationQuery) -> App {
    let directory = location_directory(app, headers, query);
    let project = resolve_location_project(&directory);
    let path = directory
        .strip_prefix(project.directory.trim_end_matches('/'))
        .map(|rest| rest.trim_start_matches('/').to_string())
        .unwrap_or_default();
    App {
        directory,
        worktree: project.directory,
        project_id: project.id,
        path,
        ..app.clone()
    }
}

fn decode_uri_component(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                output.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(output).unwrap_or_else(|_| input.to_string())
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
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

async fn api_location_get(
    State(app): State<App>,
    headers: HeaderMap,
    Query(query): Query<ApiLocationQuery>,
) -> Json<Value> {
    Json(location_info(&app, &headers, &query))
}

async fn api_agent_list(
    State(app): State<App>,
    headers: HeaderMap,
    Query(query): Query<ApiLocationQuery>,
) -> Json<Value> {
    let scoped = scoped_app(&app, &headers, &query);
    Json(location_response(
        &app,
        &headers,
        &query,
        Value::Array(official_agents(
            &scoped.worktree,
            &scoped.directory,
            &config::instance(&scoped.directory, &scoped.worktree),
        )),
    ))
}

async fn api_command_list(
    State(app): State<App>,
    headers: HeaderMap,
    Query(query): Query<ApiLocationQuery>,
) -> Json<Value> {
    let scoped = scoped_app(&app, &headers, &query);
    let Json(commands) = command_list(State(scoped.clone())).await;
    let commands = commands
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|command| {
                    command.get("source").and_then(Value::as_str) != Some("skill")
                        && command.get("name").and_then(Value::as_str) != Some("goal")
                })
                .map(|command| {
                    let mut command = official_command(command);
                    match command.get("name").and_then(Value::as_str) {
                        Some("init") => {
                            command["template"] = Value::String(
                                include_str!(
                                    "../../../packages/core/src/plugin/command/initialize.txt"
                                )
                                .replace("${path}", &scoped.worktree),
                            );
                        }
                        Some("review") => {
                            command["template"] = Value::String(
                                include_str!(
                                    "../../../packages/core/src/plugin/command/review.txt"
                                )
                                .into(),
                            );
                        }
                        _ => {}
                    }
                    command
                })
                .collect()
        })
        .unwrap_or_default();
    Json(location_response(
        &app,
        &headers,
        &query,
        Value::Array(commands),
    ))
}

async fn api_skill_list(
    State(app): State<App>,
    headers: HeaderMap,
    Query(query): Query<ApiLocationQuery>,
) -> Json<Value> {
    let Json(skills) = skill_list(State(scoped_app(&app, &headers, &query))).await;
    let mut data = vec![json!({
        "name": "customize-opencode",
        "description": "Use ONLY when the user is editing or creating opencode's own configuration: opencode.json, opencode.jsonc, files under .opencode/, or files under ~/.config/opencode/. Also use when creating or fixing opencode agents, subagents, commands, skills, plugins, MCP servers, or permission rules. Do not use for the user's own application code, or for any project that is not configuring opencode itself.",
        "location": "/builtin/customize-opencode.md",
        "content": include_str!("../../../packages/core/src/plugin/skill/customize-opencode.md"),
    })];
    data.extend(
        skills
            .as_array()
            .map(|items| items.iter().map(official_skill).collect::<Vec<_>>())
            .unwrap_or_default(),
    );
    Json(location_response(
        &app,
        &headers,
        &query,
        Value::Array(data),
    ))
}

async fn api_reference_list(
    State(app): State<App>,
    headers: HeaderMap,
    Query(query): Query<ApiLocationQuery>,
) -> Json<Value> {
    let scoped = scoped_app(&app, &headers, &query);
    let config = config::instance(&scoped.directory, &scoped.worktree);
    Json(location_response(
        &app,
        &headers,
        &query,
        Value::Array(reference_list(&config, &app.paths)),
    ))
}

async fn api_model_list(
    State(app): State<App>,
    headers: HeaderMap,
    Query(query): Query<ApiLocationQuery>,
) -> Json<Value> {
    let scoped = scoped_app(&app, &headers, &query);
    Json(location_response(
        &app,
        &headers,
        &query,
        Value::Array(provider::official_models(&config::instance(
            &scoped.directory,
            &scoped.worktree,
        ))),
    ))
}

async fn api_provider_list(
    State(app): State<App>,
    headers: HeaderMap,
    Query(query): Query<ApiLocationQuery>,
) -> Json<Value> {
    let scoped = scoped_app(&app, &headers, &query);
    Json(location_response(
        &app,
        &headers,
        &query,
        Value::Array(provider::official_providers(&config::instance(
            &scoped.directory,
            &scoped.worktree,
        ))),
    ))
}

async fn api_provider_get(
    State(app): State<App>,
    Path(provider_id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<ApiLocationQuery>,
) -> Response {
    let scoped = scoped_app(&app, &headers, &query);
    match provider::official_provider(
        &config::instance(&scoped.directory, &scoped.worktree),
        &provider_id,
    ) {
        Some(provider) => Json(location_response(&app, &headers, &query, provider)).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "_tag": "ProviderNotFoundError",
                "providerID": provider_id,
                "message": format!("Provider not found: {provider_id}"),
            })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct ApiFsListQuery {
    #[serde(flatten)]
    location: ApiLocationQuery,
    path: Option<String>,
}

async fn api_fs_list(
    State(app): State<App>,
    headers: HeaderMap,
    Query(query): Query<ApiFsListQuery>,
) -> Result<Json<Value>, Failure> {
    Ok(Json(location_response(
        &app,
        &headers,
        &query.location,
        Value::Array(
            file::entry_list(
                &location_directory(&app, &headers, &query.location),
                query.path.as_deref(),
            )
            .map_err(|error| Failure(StatusCode::BAD_REQUEST, error.to_string()))?,
        ),
    )))
}

#[derive(Deserialize)]
struct ApiFsFindQuery {
    #[serde(flatten)]
    location: ApiLocationQuery,
    query: String,
    #[serde(rename = "type")]
    kind: Option<String>,
    limit: Option<usize>,
}

async fn api_fs_find(
    State(app): State<App>,
    headers: HeaderMap,
    Query(query): Query<ApiFsFindQuery>,
) -> Result<Json<Value>, Failure> {
    Ok(Json(location_response(
        &app,
        &headers,
        &query.location,
        Value::Array(
            file::entry_find(
                &location_directory(&app, &headers, &query.location),
                &query.query,
                query.kind.as_deref(),
                query.limit.unwrap_or(50),
            )
            .map_err(|error| Failure(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?,
        ),
    )))
}

async fn api_fs_read(
    State(app): State<App>,
    headers: HeaderMap,
    Path(path): Path<String>,
    Query(query): Query<ApiLocationQuery>,
) -> Result<Response, Failure> {
    let (content, mime) = file::read_bytes(
        &location_directory(&app, &headers, &query),
        path.trim_start_matches('/'),
    )
    .map_err(|error| {
        let status = if error.kind() == std::io::ErrorKind::NotFound {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::BAD_REQUEST
        };
        Failure(status, error.to_string())
    })?;
    Ok(([(header::CONTENT_TYPE, mime)], content).into_response())
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

/// Port of ProjectV2.resolve + SessionV2.create's project upsert: the project
/// ID is sha1("git-remote:{host}/{path}") for repos with an origin remote, a
/// cached `.git/opencode` ID, the root commit, or "global" outside git.
fn resolve_or_create_project(
    conn: &rusqlite::Connection,
    directory: &str,
) -> rusqlite::Result<(String, String)> {
    use sha1::{Digest, Sha1};
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(args)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let worktree = git(&["rev-parse", "--show-toplevel"]);
    let (id, worktree, vcs) = match worktree {
        None => ("global".to_string(), "/".to_string(), None),
        Some(worktree) => {
            let remote_id = git(&["remote", "get-url", "origin"])
                .and_then(|origin| normalize_remote(&origin))
                .map(|normalized| {
                    let mut hasher = Sha1::new();
                    hasher.update(format!("git-remote:{normalized}").as_bytes());
                    hasher
                        .finalize()
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>()
                });
            let cached = git(&["rev-parse", "--git-common-dir"]).and_then(|common| {
                let path = std::path::Path::new(directory)
                    .join(common)
                    .join("opencode");
                std::fs::read_to_string(path)
                    .ok()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
            });
            let root = git(&["rev-list", "--max-parents=0", "HEAD"])
                .map(|list| list.lines().next().unwrap_or_default().to_string());
            (
                remote_id
                    .or(cached)
                    .or(root)
                    .unwrap_or_else(|| "global".to_string()),
                worktree,
                Some("git"),
            )
        }
    };
    let timestamp = now();
    conn.execute(
        "INSERT INTO project (id, worktree, vcs, sandboxes, time_created, time_updated) \
         VALUES (?, ?, ?, '[]', ?, ?) ON CONFLICT(id) DO NOTHING",
        rusqlite::params![id, worktree, vcs, timestamp, timestamp],
    )?;
    Ok((id, worktree))
}

/// ProjectV2 URL normalization: lowercase host + path without leading
/// slashes, trailing `.git`, or trailing slashes; scp-like remotes supported.
fn normalize_remote(origin: &str) -> Option<String> {
    let value = origin.trim();
    if value.is_empty() {
        return None;
    }
    let parts = |host: &str, name: &str| {
        let pathname = name
            .trim_start_matches('/')
            .trim_end_matches('/')
            .trim_end_matches(".git")
            .trim_end_matches('/');
        if host.is_empty() || pathname.is_empty() {
            return None;
        }
        Some(format!("{}/{pathname}", host.to_lowercase()))
    };
    if let Some(rest) = value.split_once("://").map(|(_, rest)| rest) {
        if value.starts_with("file:") {
            return None;
        }
        let rest = rest.split_once('@').map(|(_, host)| host).unwrap_or(rest);
        let (host, path) = rest.split_once('/')?;
        return parts(host.split(':').next().unwrap_or(host), path);
    }
    // scp-like: [user@]host:path
    let captures = value.split_once(':')?;
    let host = captures
        .0
        .split_once('@')
        .map(|(_, host)| host)
        .unwrap_or(captures.0);
    parts(host, captures.1)
}

struct ResolvedLocationProject {
    id: String,
    directory: String,
}

fn resolve_location_project(directory: &str) -> ResolvedLocationProject {
    use sha1::{Digest, Sha1};
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(args)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let Some(worktree) = git(&["rev-parse", "--show-toplevel"]) else {
        return ResolvedLocationProject {
            id: "global".into(),
            directory: "/".into(),
        };
    };
    let remote_id = git(&["remote", "get-url", "origin"])
        .and_then(|origin| normalize_remote(&origin))
        .map(|normalized| {
            let mut hasher = Sha1::new();
            hasher.update(format!("git-remote:{normalized}").as_bytes());
            hasher
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        });
    let cached = git(&["rev-parse", "--git-common-dir"]).and_then(|common| {
        std::fs::read_to_string(
            std::path::Path::new(directory)
                .join(common)
                .join("opencode"),
        )
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    });
    let root = git(&["rev-list", "--max-parents=0", "HEAD"])
        .map(|list| list.lines().next().unwrap_or_default().to_string());
    ResolvedLocationProject {
        id: remote_id
            .or(cached)
            .or(root)
            .unwrap_or_else(|| "global".into()),
        directory: worktree,
    }
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
    let (project_id, worktree) = resolve_or_create_project(&conn, &directory)?;
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

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|item| item == name)
        .and_then(|index| args.get(index + 1).cloned())
}

fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|item| item == name)
}

pub struct ServeOptions {
    pub db: String,
    pub directory: String,
    pub port: Option<u16>,
    pub hostname: String,
    pub register: bool,
    pub password: Option<String>,
}

impl ServeOptions {
    pub fn from_args(args: Vec<String>) -> Self {
        let db = arg_value(&args, "--db").unwrap_or_else(|| {
            let home = std::env::var("HOME").expect("HOME not set");
            format!("{home}/.local/share/opencode/opencode-local.db")
        });
        let directory = arg_value(&args, "--directory").unwrap_or_else(|| {
            std::env::current_dir()
                .expect("cwd")
                .to_string_lossy()
                .into_owned()
        });
        ServeOptions {
            db,
            directory,
            port: arg_value(&args, "--port").and_then(|value| value.parse().ok()),
            hostname: arg_value(&args, "--hostname").unwrap_or_else(|| "127.0.0.1".into()),
            register: has_flag(&args, "--register"),
            password: arg_value(&args, "--password"),
        }
    }
}

pub async fn run(options: ServeOptions) -> Result<(), String> {
    let manager = SqliteConnectionManager::file(&options.db).with_init(|conn| {
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
        resolve_project(&conn, &options.directory).expect("project resolution")
    };
    let path = options
        .directory
        .strip_prefix(worktree.trim_end_matches('/'))
        .map(|rest| rest.trim_start_matches('/').to_string())
        .unwrap_or_default();

    let password = if options.register {
        Some(opengoal_daemon::load_or_create_password(
            options.password.as_deref(),
        )?)
    } else {
        options
            .password
            .or_else(|| std::env::var("OPENCODE_SERVER_PASSWORD").ok())
    };

    let (port, listener) = bind_listener(&options.hostname, options.port).await?;
    let url = format!("http://{}:{port}", options.hostname);

    let app = App {
        pool,
        bus: bus::Bus::new(),
        project_id: project_id.clone(),
        directory: options.directory.clone(),
        worktree: worktree.clone(),
        path,
        version: std::env::var("OPENCODE_VERSION")
            .unwrap_or_else(|_| opengoal_daemon::VERSION.into()),
        port,
        paths: config::paths(),
    };

    let auth = auth::ServerAuth {
        username: std::env::var("OPENCODE_SERVER_USERNAME").unwrap_or_else(|_| "opencode".into()),
        password,
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
        .route("/api/location", get(api_location_get))
        .route("/api/agent", get(api_agent_list))
        .route("/api/command", get(api_command_list))
        .route("/api/skill", get(api_skill_list))
        .route("/api/reference", get(api_reference_list))
        .route("/api/model", get(api_model_list))
        .route("/api/provider", get(api_provider_list))
        .route("/api/provider/{provider_id}", get(api_provider_get))
        .route("/api/fs/list", get(api_fs_list))
        .route("/api/fs/find", get(api_fs_find))
        .route("/api/fs/read/{*path}", get(api_fs_read))
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
        .layer(axum::middleware::from_fn_with_state(
            auth.clone(),
            auth::middleware,
        ))
        .with_state(app);

    let registration_id = uuid::Uuid::new_v4().to_string();
    if options.register {
        opengoal_daemon::register(&url, &registration_id)?;
        let keepalive = registration_id.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                if !opengoal_daemon::registration_owner(&keepalive) {
                    std::process::exit(0);
                }
            }
        });
    }

    let listener = listener;
    eprintln!(
        "opengoal-server (rust) listening on {url} project={project_id} directory={}",
        options.directory
    );

    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    let result = axum::serve(listener, router)
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(|error| error.to_string());

    if options.register {
        opengoal_daemon::unregister(&registration_id);
    }
    result
}

async fn bind_listener(
    hostname: &str,
    port: Option<u16>,
) -> Result<(u16, tokio::net::TcpListener), String> {
    if let Some(port) = port {
        let listener = tokio::net::TcpListener::bind((hostname, port))
            .await
            .map_err(|error| error.to_string())?;
        return Ok((port, listener));
    }
    for candidate in 4096..=65535 {
        if let Ok(listener) = tokio::net::TcpListener::bind((hostname, candidate)).await {
            return Ok((candidate, listener));
        }
    }
    Err("failed to bind a port".into())
}

#[cfg(test)]
mod tests {
    use super::{
        bus, chrono_iso, config, location_response, parse_frontmatter, relative_path,
        ApiLocationQuery, App,
    };
    use axum::http::HeaderMap;
    use r2d2_sqlite::SqliteConnectionManager;
    use serde_json::{json, Value};

    #[test]
    fn iso_matches_date_to_iso_string() {
        assert_eq!(chrono_iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(chrono_iso(1_751_659_200_123), "2025-07-04T20:00:00.123Z");
        assert_eq!(chrono_iso(1_783_197_020_091), "2026-07-04T20:30:20.091Z");
    }

    #[test]
    fn frontmatter_preserves_nested_tool_rules() {
        let parsed = parse_frontmatter(
            "mode: primary\nhidden: true\ntools:\n  \"*\": false\n  \"github-pr-search\": true",
        );
        assert_eq!(parsed["mode"], "primary");
        assert_eq!(parsed["hidden"], true);
        assert_eq!(parsed["tools"]["*"], false);
        assert_eq!(parsed["tools"]["github-pr-search"], true);
        assert_eq!(
            relative_path(
                "/workspace/packages/opencode",
                "/home/ubuntu/.local/share/opencode/plans"
            ),
            "../../../home/ubuntu/.local/share/opencode/plans"
        );
    }

    #[test]
    fn location_response_wraps_current_protocol_shape() {
        let app = App {
            pool: r2d2::Pool::builder()
                .max_size(1)
                .build(SqliteConnectionManager::memory())
                .expect("pool"),
            bus: bus::Bus::new(),
            project_id: "project-id".into(),
            directory: "/tmp/project".into(),
            worktree: "/tmp/project".into(),
            path: String::new(),
            version: "test".into(),
            port: 4096,
            paths: config::Paths {
                home: "/tmp/home".into(),
                config: "/tmp/config".into(),
                state: "/tmp/state".into(),
                cache: "/tmp/cache".into(),
            },
        };
        let query = ApiLocationQuery {
            location_directory: None,
            location_workspace: Some("workspace-id".into()),
        };
        let wrapped = location_response(&app, &HeaderMap::new(), &query, json!([{"id": "build"}]));

        assert_eq!(wrapped["data"], json!([{"id": "build"}]));
        assert_eq!(wrapped["location"]["directory"], "/tmp/project");
        assert_eq!(wrapped["location"]["workspaceID"], "workspace-id");
        assert_eq!(
            wrapped["location"]["project"],
            json!({
                "id": "project-id",
                "directory": "/tmp/project",
            })
        );
        assert!(matches!(wrapped, Value::Object(_)));
    }
}
