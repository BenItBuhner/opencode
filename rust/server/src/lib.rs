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
pub mod bus;
mod config;
mod file;
mod float_check;
mod identifier;
mod message;
mod permission_v2;
mod project;
mod provider;
pub mod pty;
mod question_v2;
mod runner;
mod session;
mod slug;
mod v2;
mod vcs;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use r2d2_sqlite::SqliteConnectionManager;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::convert::Infallible;
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
    pty: pty::Registry,
    pty_tickets: pty::TicketRegistry,
    permissions: permission_v2::Registry,
    questions: question_v2::Registry,
}

#[derive(Debug)]
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
    runner::background::cancel_task_tree(id);
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

async fn experimental_session_background(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    {
        let conn = app.pool.get()?;
        require_session(&conn, &id)?;
    }
    let promoted = runner::background::list()
        .into_iter()
        .filter(|job| {
            job.kind == "task"
                && job.status == runner::background::Status::Running
                && job.metadata.get("parentSessionId").and_then(Value::as_str) == Some(id.as_str())
                && !runner::background::is_promoted(job)
        })
        .filter_map(|job| runner::background::promote(&job.id))
        .count();
    Ok(Json(Value::Bool(promoted > 0)))
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
        order_agent(agent)
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
    order_agent(Value::Object(info))
}

fn order_agent(agent: Value) -> Value {
    let Value::Object(mut source) = agent else {
        return agent;
    };
    let mut ordered = serde_json::Map::new();
    for key in [
        "id",
        "model",
        "request",
        "system",
        "description",
        "mode",
        "hidden",
        "color",
        "permissions",
    ] {
        if let Some(value) = source.shift_remove(key) {
            ordered.insert(key.into(), value);
        }
    }
    ordered.extend(source);
    Value::Object(ordered)
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

#[derive(Debug)]
struct V2InvalidRequest {
    message: String,
    field: &'static str,
}

fn v2_invalid_request(error: V2InvalidRequest) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "_tag": "InvalidRequestError",
            "message": error.message,
            "field": error.field,
        })),
    )
        .into_response()
}

fn invalid_request(message: impl Into<String>, field: &'static str) -> V2InvalidRequest {
    V2InvalidRequest {
        message: message.into(),
        field,
    }
}

fn validate_positive_limit(
    limit: Option<i64>,
    default: i64,
    max: i64,
    field: &'static str,
) -> Result<i64, V2InvalidRequest> {
    let value = limit.unwrap_or(default);
    if !(1..=max).contains(&value) {
        return Err(invalid_request(
            format!("{field} must be an integer between 1 and {max}"),
            field,
        ));
    }
    Ok(value)
}

fn validate_non_negative(
    value: Option<i64>,
    field: &'static str,
) -> Result<Option<i64>, V2InvalidRequest> {
    if value.is_some_and(|value| value < 0) {
        return Err(invalid_request(
            format!("{field} must be a non-negative integer"),
            field,
        ));
    }
    Ok(value)
}

fn validate_order(order: Option<&str>) -> Result<(), V2InvalidRequest> {
    if v2::valid_order(order) {
        return Ok(());
    }
    Err(invalid_request("order must be asc or desc", "order"))
}

fn v2_sse_response<S>(stream: S) -> Response
where
    S: futures::Stream<Item = Result<String, Infallible>> + Send + 'static,
{
    let mut response = Body::from_stream(stream).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        "text/event-stream".parse().expect("header"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        "no-cache, no-transform".parse().expect("header"),
    );
    headers.insert("X-Accel-Buffering", "no".parse().expect("header"));
    headers.insert("X-Content-Type-Options", "nosniff".parse().expect("header"));
    response
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

async fn api_event_stream(State(app): State<App>) -> Response {
    use futures::StreamExt;

    let receiver = app.bus.subscribe_v2();
    let connected =
        futures::stream::once(async move { Ok(v2::sse_message_bytes(&v2::connected_event())) });
    let events =
        tokio_stream::wrappers::BroadcastStream::new(receiver).filter_map(|item| async move {
            match item {
                Ok(value) => Some(Ok(v2::sse_message_bytes(&value))),
                Err(_) => None,
            }
        });
    let heartbeat =
        tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(Duration::from_secs(15)))
            .skip(1)
            .map(|_| Ok(": heartbeat\n\n".to_string()));
    v2_sse_response(futures::stream::select(connected.chain(events), heartbeat))
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
    if let Err(error) = validate_order(query.order.as_deref()) {
        return Ok(v2_invalid_request(error));
    }
    let limit = match validate_positive_limit(query.limit, 50, i64::MAX, "limit") {
        Ok(limit) => limit,
        Err(error) => return Ok(v2_invalid_request(error)),
    };
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
    Ok(Json(v2::list(&conn, &resolved, limit)?).into_response())
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
            if let Some(seq) = admitted.get("admittedSeq").and_then(Value::as_i64) {
                publish_v2_event_at(&app, &id, seq)?;
            }
            // Admission schedules advisory SessionExecution.wake unless
            // resume: false requests admit-only behavior.
            if payload.get("resume").and_then(Value::as_bool) != Some(false) {
                runner::wake(
                    runner::Env {
                        pool: app.pool.clone(),
                        worktree: app.worktree.clone(),
                        project_id: app.project_id.clone(),
                        bus: app.bus.clone(),
                        permissions: app.permissions.clone(),
                        questions: app.questions.clone(),
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

fn publish_v2_event_at(app: &App, session_id: &str, seq: i64) -> Result<(), Failure> {
    let conn = app.pool.get()?;
    if let Some(event) = v2::event_at(&conn, session_id, seq)? {
        app.bus.publish_v2(event);
    }
    Ok(())
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
    let after = match validate_non_negative(query.after, "after") {
        Ok(after) => after,
        Err(error) => return Ok(v2_invalid_request(error)),
    };
    let limit = match validate_positive_limit(query.limit, 50, 100, "limit") {
        Ok(limit) => limit,
        Err(error) => return Ok(v2_invalid_request(error)),
    };
    let conn = app.pool.get()?;
    if v2::get(&conn, &id)?.is_none() {
        return Ok(v2_session_not_found(&id));
    }
    Ok(Json(v2::history(&conn, &id, after, limit)?).into_response())
}

async fn api_session_context(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Response, Failure> {
    let conn = app.pool.get()?;
    if v2::get(&conn, &id)?.is_none() {
        return Ok(v2_session_not_found(&id));
    }
    Ok(Json(json!({ "data": v2::context(&conn, &id)? })).into_response())
}

#[derive(Deserialize)]
struct ApiSessionEventQuery {
    after: Option<i64>,
}

struct V2SessionEventTail {
    pool: Pool,
    session_id: String,
    after: i64,
    pending: VecDeque<Value>,
}

async fn api_session_events(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(query): Query<ApiSessionEventQuery>,
) -> Result<Response, Failure> {
    let after = match validate_non_negative(query.after, "after") {
        Ok(after) => after.unwrap_or(-1),
        Err(error) => return Ok(v2_invalid_request(error)),
    };
    {
        let conn = app.pool.get()?;
        if v2::get(&conn, &id)?.is_none() {
            return Ok(v2_session_not_found(&id));
        }
    }
    let stream = futures::stream::unfold(
        V2SessionEventTail {
            pool: app.pool.clone(),
            session_id: id,
            after,
            pending: VecDeque::new(),
        },
        |mut state| async move {
            loop {
                if let Some(event) = state.pending.pop_front() {
                    if let Some(seq) = event
                        .get("durable")
                        .and_then(|durable| durable.get("seq"))
                        .and_then(Value::as_i64)
                    {
                        state.after = seq;
                    }
                    return Some((Ok(v2::sse_message_bytes(&event)), state));
                }
                if let Some(events) = state
                    .pool
                    .get()
                    .ok()
                    .and_then(|conn| {
                        v2::durable_events_after(&conn, &state.session_id, state.after).ok()
                    })
                    .filter(|events| !events.is_empty())
                {
                    state.pending = events.into();
                    continue;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        },
    );
    Ok(v2_sse_response(stream))
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
    if let Err(error) = validate_order(query.order.as_deref()) {
        return Ok(v2_invalid_request(error));
    }
    let limit = match validate_positive_limit(query.limit, 50, 200, "limit") {
        Ok(limit) => limit,
        Err(error) => return Ok(v2_invalid_request(error)),
    };
    if query.cursor.is_some() && query.order.is_some() {
        return Ok(v2_invalid_cursor("Cursor cannot be combined with order"));
    }
    let decoded = match query.cursor.as_deref() {
        Some(cursor) => {
            let parsed = b64::decode(cursor)
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .and_then(|value| {
                    let id = value.get("id")?.as_str()?.to_string();
                    let order = value.get("order")?.as_str()?.to_string();
                    let direction = value.get("direction")?.as_str()?.to_string();
                    (id.starts_with("msg_")
                        && matches!(order.as_str(), "asc" | "desc")
                        && matches!(direction.as_str(), "previous" | "next"))
                    .then_some((id, order, direction))
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
            limit,
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
    // V2 interruption targets the active process-local ownership chain; idle
    // or missing interruption is a no-op.
    runner::interrupt(&id);
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
    let seq = publisher
        .publish(
            &mut conn,
            "session.next.agent.switched",
            1,
            &Value::Object(data),
        )
        .map_err(|error| Failure(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    publish_v2_event_at(&app, &id, seq)?;
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
    let seq = publisher
        .publish(
            &mut conn,
            "session.next.model.switched",
            1,
            &Value::Object(data),
        )
        .map_err(|error| Failure(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    publish_v2_event_at(&app, &id, seq)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ---------------------------------------------------------------------------
// /api/pty routes — WebSocket protocol matches packages/core/src/pty/protocol.ts
// ---------------------------------------------------------------------------

fn pty_not_found(id: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "_tag": "PtyNotFoundError",
            "ptyID": id,
            "message": format!("PTY session not found: {id}"),
        })),
    )
        .into_response()
}

fn pty_forbidden(message: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "_tag": "ForbiddenError",
            "message": message,
        })),
    )
        .into_response()
}

fn pty_ticket_scope(
    app: &App,
    headers: &HeaderMap,
    query: &ApiLocationQuery,
    id: &str,
) -> pty::Scope {
    pty::Scope {
        pty_id: id.to_string(),
        directory: Some(location_directory(app, headers, query)),
        workspace_id: query.location_workspace.clone().or_else(|| {
            headers
                .get("x-opencode-workspace")
                .and_then(|value| value.to_str().ok())
                .map(ToString::to_string)
        }),
    }
}

async fn api_pty_list(
    State(app): State<App>,
    headers: HeaderMap,
    Query(query): Query<ApiLocationQuery>,
) -> Json<Value> {
    Json(location_response(
        &app,
        &headers,
        &query,
        Value::Array(
            app.pty
                .list()
                .into_iter()
                .map(|info| serde_json::to_value(info).expect("serializable"))
                .collect(),
        ),
    ))
}

#[derive(Deserialize)]
struct ApiPtyCreatePayload {
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    env: Option<std::collections::HashMap<String, String>>,
}

async fn api_pty_create(
    State(app): State<App>,
    headers: HeaderMap,
    Query(query): Query<ApiLocationQuery>,
    Json(payload): Json<ApiPtyCreatePayload>,
) -> Response {
    let cwd = payload
        .cwd
        .clone()
        .unwrap_or_else(|| location_directory(&app, &headers, &query));
    match app.pty.create(pty::CreateInput {
        command: payload.command,
        args: payload.args,
        cwd: Some(cwd),
        title: payload.title,
        env: payload.env,
    }) {
        Ok(info) => Json(location_response(
            &app,
            &headers,
            &query,
            serde_json::to_value(info).expect("serializable"),
        ))
        .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "message": error.to_string() })),
        )
            .into_response(),
    }
}

async fn api_pty_get(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<ApiLocationQuery>,
) -> Response {
    match app.pty.get(&id) {
        Ok(info) => Json(location_response(
            &app,
            &headers,
            &query,
            serde_json::to_value(info).expect("serializable"),
        ))
        .into_response(),
        Err(_) => pty_not_found(&id),
    }
}

async fn api_pty_update(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<ApiLocationQuery>,
    Json(payload): Json<pty::UpdateInput>,
) -> Response {
    match app.pty.update(&id, payload) {
        Ok(info) => Json(location_response(
            &app,
            &headers,
            &query,
            serde_json::to_value(info).expect("serializable"),
        ))
        .into_response(),
        Err(pty::Error::NotFound(_)) => pty_not_found(&id),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "message": error.to_string() })),
        )
            .into_response(),
    }
}

async fn api_pty_remove(State(app): State<App>, Path(id): Path<String>) -> Response {
    match app.pty.remove(&id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(pty::Error::NotFound(_)) => pty_not_found(&id),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "message": error.to_string() })),
        )
            .into_response(),
    }
}

async fn api_pty_connect_token(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<ApiLocationQuery>,
) -> Response {
    // The custom header forces a CORS preflight, matching the TS route: cross-
    // origin browser pages cannot mint tickets without the server's origin
    // policy accepting them.
    let header_ok = headers
        .get("x-opencode-ticket")
        .and_then(|value| value.to_str().ok())
        == Some("1");
    if !header_ok {
        return pty_forbidden("Invalid PTY connect token request");
    }
    if app.pty.get(&id).is_err() {
        return pty_not_found(&id);
    }
    let token = app
        .pty_tickets
        .issue(pty_ticket_scope(&app, &headers, &query, &id));
    Json(location_response(
        &app,
        &headers,
        &query,
        serde_json::to_value(token).expect("serializable"),
    ))
    .into_response()
}

#[derive(Deserialize)]
struct ApiPtyConnectQuery {
    #[serde(flatten)]
    location: ApiLocationQuery,
    #[serde(default)]
    ticket: Option<String>,
    #[serde(default)]
    cursor: Option<String>,
}

async fn api_pty_connect(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<ApiPtyConnectQuery>,
    ws: axum::extract::ws::WebSocketUpgrade,
) -> Response {
    if app.pty.get(&id).is_err() {
        return pty_not_found(&id);
    }
    if let Some(ticket) = query.ticket.as_deref() {
        let scope = pty_ticket_scope(&app, &headers, &query.location, &id);
        if !app.pty_tickets.consume(ticket, &scope) {
            return pty_forbidden("Invalid or expired PTY ticket");
        }
    }
    let cursor = query
        .cursor
        .as_deref()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value >= -1);
    let registry = app.pty.clone();
    ws.on_upgrade(move |socket| {
        pty::router::handle_socket(
            pty::router::State_ {
                registry,
                tickets: app.pty_tickets.clone(),
            },
            id,
            cursor,
            socket,
        )
    })
}

// ---------------------------------------------------------------------------
// /api/permission + /api/session/{id}/permission — v2 permission surface
// ---------------------------------------------------------------------------

fn permission_not_found(id: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "_tag": "PermissionNotFoundError",
            "requestID": id,
            "message": format!("Permission request not found: {id}"),
        })),
    )
        .into_response()
}

fn question_not_found(id: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "_tag": "QuestionNotFoundError",
            "requestID": id,
            "message": format!("Question request not found: {id}"),
        })),
    )
        .into_response()
}

async fn api_permission_request_list(
    State(app): State<App>,
    headers: HeaderMap,
    Query(query): Query<ApiLocationQuery>,
) -> Response {
    let items: Vec<Value> = app
        .permissions
        .list()
        .iter()
        .map(permission_v2::request_json)
        .collect();
    Json(location_response(
        &app,
        &headers,
        &query,
        Value::Array(items),
    ))
    .into_response()
}

#[derive(Deserialize)]
struct ApiPermissionSavedQuery {
    #[serde(rename = "projectID")]
    project_id: Option<String>,
}

async fn api_permission_saved_list(
    State(app): State<App>,
    Query(query): Query<ApiPermissionSavedQuery>,
) -> Result<Response, Failure> {
    let conn = app.pool.get()?;
    let project = query.project_id.unwrap_or_else(|| app.project_id.clone());
    let rows = permission_v2::saved_list(&conn, Some(&project))?;
    let data: Vec<Value> = rows.iter().map(permission_v2::saved_json).collect();
    Ok(Json(json!({ "data": data })).into_response())
}

async fn api_permission_saved_remove(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Response, Failure> {
    let conn = app.pool.get()?;
    permission_v2::saved_remove(&conn, &id)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn api_session_permission_list(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Response, Failure> {
    let conn = app.pool.get()?;
    if v2::get(&conn, &id)?.is_none() {
        return Ok(v2_session_not_found(&id));
    }
    let data: Vec<Value> = app
        .permissions
        .for_session(&id)
        .iter()
        .map(permission_v2::request_json)
        .collect();
    Ok(Json(json!({ "data": data })).into_response())
}

async fn api_session_permission_get(
    State(app): State<App>,
    Path((id, request_id)): Path<(String, String)>,
) -> Result<Response, Failure> {
    let conn = app.pool.get()?;
    if v2::get(&conn, &id)?.is_none() {
        return Ok(v2_session_not_found(&id));
    }
    let Some(request) = app.permissions.get(&request_id) else {
        return Ok(permission_not_found(&request_id));
    };
    if request.session_id != id {
        return Ok(permission_not_found(&request_id));
    }
    Ok(Json(json!({ "data": permission_v2::request_json(&request) })).into_response())
}

#[derive(Deserialize)]
struct ApiPermissionCreatePayload {
    id: Option<String>,
    action: String,
    #[serde(default)]
    resources: Vec<String>,
    #[serde(default)]
    save: Option<Vec<String>>,
    #[serde(default)]
    metadata: Option<Value>,
    #[serde(default)]
    source: Option<Value>,
    #[serde(default)]
    agent: Option<String>,
}

async fn api_session_permission_create(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(payload): Json<ApiPermissionCreatePayload>,
) -> Result<Response, Failure> {
    let conn = app.pool.get()?;
    if v2::get(&conn, &id)?.is_none() {
        return Ok(v2_session_not_found(&id));
    }
    let outcome = permission_v2::ask(
        &permission_v2::Env {
            bus: &app.bus,
            conn: &conn,
            project_id: &app.project_id,
            registry: &app.permissions,
        },
        &permission_v2::AssertInput {
            id: payload.id,
            session_id: id.clone(),
            action: payload.action,
            resources: payload.resources,
            save: payload.save,
            metadata: payload.metadata,
            source: payload.source,
            agent: payload.agent,
        },
    )
    .map_err(|error| match error {
        permission_v2::ApiError::NotFound { request_id } => Failure(
            StatusCode::CONFLICT,
            format!("Duplicate pending permission ID: {request_id}"),
        ),
    })?;
    Ok(
        Json(json!({ "data": permission_v2::ask_result_json(&outcome.id, outcome.effect) }))
            .into_response(),
    )
}

#[derive(Deserialize)]
struct ApiPermissionReplyPayload {
    reply: String,
    #[serde(default)]
    message: Option<String>,
}

async fn api_session_permission_reply(
    State(app): State<App>,
    Path((id, request_id)): Path<(String, String)>,
    Json(payload): Json<ApiPermissionReplyPayload>,
) -> Result<Response, Failure> {
    let conn = app.pool.get()?;
    if v2::get(&conn, &id)?.is_none() {
        return Ok(v2_session_not_found(&id));
    }
    let Some(request) = app.permissions.get(&request_id) else {
        return Ok(permission_not_found(&request_id));
    };
    if request.session_id != id {
        return Ok(permission_not_found(&request_id));
    }
    let reply = match payload.reply.as_str() {
        "once" => permission_v2::Reply::Once,
        "always" => permission_v2::Reply::Always,
        "reject" => permission_v2::Reply::Reject(payload.message),
        other => {
            return Err(Failure(
                StatusCode::BAD_REQUEST,
                format!("invalid reply: {other}"),
            ));
        }
    };
    match permission_v2::reply(
        &permission_v2::Env {
            bus: &app.bus,
            conn: &conn,
            project_id: &app.project_id,
            registry: &app.permissions,
        },
        &request_id,
        reply,
    ) {
        Ok(()) => Ok(StatusCode::NO_CONTENT.into_response()),
        Err(permission_v2::ApiError::NotFound { .. }) => Ok(permission_not_found(&request_id)),
    }
}

async fn api_question_request_list(
    State(app): State<App>,
    headers: HeaderMap,
    Query(query): Query<ApiLocationQuery>,
) -> Response {
    let items: Vec<Value> = app
        .questions
        .list()
        .iter()
        .map(question_v2::request_json)
        .collect();
    Json(location_response(
        &app,
        &headers,
        &query,
        Value::Array(items),
    ))
    .into_response()
}

async fn api_session_question_list(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Response, Failure> {
    let conn = app.pool.get()?;
    if v2::get(&conn, &id)?.is_none() {
        return Ok(v2_session_not_found(&id));
    }
    let data: Vec<Value> = app
        .questions
        .for_session(&id)
        .iter()
        .map(question_v2::request_json)
        .collect();
    Ok(Json(json!({ "data": data })).into_response())
}

#[derive(Deserialize)]
struct ApiQuestionReplyPayload {
    answers: Vec<Vec<String>>,
}

async fn api_session_question_reply(
    State(app): State<App>,
    Path((id, request_id)): Path<(String, String)>,
    Json(payload): Json<ApiQuestionReplyPayload>,
) -> Result<Response, Failure> {
    let conn = app.pool.get()?;
    if v2::get(&conn, &id)?.is_none() {
        return Ok(v2_session_not_found(&id));
    }
    let Some(request) = app.questions.get(&request_id) else {
        return Ok(question_not_found(&request_id));
    };
    if request.session_id != id {
        return Ok(question_not_found(&request_id));
    }
    match question_v2::reply(
        &question_v2::Env {
            bus: &app.bus,
            registry: &app.questions,
        },
        &request_id,
        payload.answers,
    ) {
        Ok(()) => Ok(StatusCode::NO_CONTENT.into_response()),
        Err(question_v2::ApiError::NotFound { .. }) => Ok(question_not_found(&request_id)),
    }
}

async fn api_session_question_reject(
    State(app): State<App>,
    Path((id, request_id)): Path<(String, String)>,
) -> Result<Response, Failure> {
    let conn = app.pool.get()?;
    if v2::get(&conn, &id)?.is_none() {
        return Ok(v2_session_not_found(&id));
    }
    let Some(request) = app.questions.get(&request_id) else {
        return Ok(question_not_found(&request_id));
    };
    if request.session_id != id {
        return Ok(question_not_found(&request_id));
    }
    match question_v2::reject(
        &question_v2::Env {
            bus: &app.bus,
            registry: &app.questions,
        },
        &request_id,
    ) {
        Ok(()) => Ok(StatusCode::NO_CONTENT.into_response()),
        Err(question_v2::ApiError::NotFound { .. }) => Ok(question_not_found(&request_id)),
    }
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

    let bus = bus::Bus::new();
    let app = App {
        pool,
        bus: bus.clone(),
        project_id: project_id.clone(),
        directory: options.directory.clone(),
        worktree: worktree.clone(),
        path,
        version: std::env::var("OPENCODE_VERSION")
            .unwrap_or_else(|_| opengoal_daemon::VERSION.into()),
        port,
        paths: config::paths(),
        pty: pty::Registry::new(bus),
        pty_tickets: pty::TicketRegistry::default(),
        permissions: permission_v2::Registry::new(),
        questions: question_v2::Registry::new(),
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
        .route(
            "/experimental/session/{id}/background",
            post(experimental_session_background),
        )
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
        .route("/api/event", get(api_event_stream))
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
        .route("/api/session/{id}/context", get(api_session_context))
        .route("/api/session/{id}/history", get(api_session_history))
        .route("/api/session/{id}/event", get(api_session_events))
        .route("/api/session/{id}/message", get(api_session_messages))
        .route(
            "/api/session/{id}/message/{message_id}",
            get(api_session_message),
        )
        .route("/api/session/{id}/interrupt", post(api_session_interrupt))
        .route("/api/session/{id}/wait", post(api_session_wait))
        .route("/api/session/{id}/agent", post(api_session_switch_agent))
        .route("/api/session/{id}/model", post(api_session_switch_model))
        .route("/api/permission/request", get(api_permission_request_list))
        .route("/api/permission/saved", get(api_permission_saved_list))
        .route(
            "/api/permission/saved/{id}",
            axum::routing::delete(api_permission_saved_remove),
        )
        .route(
            "/api/session/{id}/permission",
            get(api_session_permission_list).post(api_session_permission_create),
        )
        .route(
            "/api/session/{id}/permission/{request_id}",
            get(api_session_permission_get),
        )
        .route(
            "/api/session/{id}/permission/{request_id}/reply",
            post(api_session_permission_reply),
        )
        .route("/api/question/request", get(api_question_request_list))
        .route("/api/session/{id}/question", get(api_session_question_list))
        .route(
            "/api/session/{id}/question/{request_id}/reply",
            post(api_session_question_reply),
        )
        .route(
            "/api/session/{id}/question/{request_id}/reject",
            post(api_session_question_reject),
        )
        .route("/api/pty", get(api_pty_list).post(api_pty_create))
        .route(
            "/api/pty/{id}",
            get(api_pty_get).put(api_pty_update).delete(api_pty_remove),
        )
        .route("/api/pty/{id}/connect-token", post(api_pty_connect_token))
        .route("/api/pty/{id}/connect", get(api_pty_connect))
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
        api_permission_request_list, api_permission_saved_list, api_permission_saved_remove,
        api_question_request_list, api_session_permission_create, api_session_permission_get,
        api_session_permission_reply, api_session_question_list, api_session_question_reject,
        api_session_question_reply, bus, chrono_iso, config, location_response, parse_frontmatter,
        permission_v2, question_v2, relative_path, validate_non_negative, validate_order,
        validate_positive_limit, ApiLocationQuery, ApiPermissionCreatePayload,
        ApiPermissionReplyPayload, ApiPermissionSavedQuery, ApiQuestionReplyPayload, App,
    };
    use axum::extract::{Path, Query, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::Json;
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
        let bus = bus::Bus::new();
        let app = App {
            pool: r2d2::Pool::builder()
                .max_size(1)
                .build(SqliteConnectionManager::memory())
                .expect("pool"),
            bus: bus.clone(),
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
            pty: crate::pty::Registry::new(bus),
            pty_tickets: crate::pty::TicketRegistry::default(),
            permissions: crate::permission_v2::Registry::new(),
            questions: crate::question_v2::Registry::new(),
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

    #[test]
    fn v2_query_validation_matches_protocol_bounds() {
        assert!(validate_non_negative(Some(0), "after").is_ok());
        assert!(validate_non_negative(Some(-1), "after").is_err());
        assert_eq!(validate_positive_limit(None, 50, 100, "limit").unwrap(), 50);
        assert!(validate_positive_limit(Some(0), 50, 100, "limit").is_err());
        assert!(validate_positive_limit(Some(101), 50, 100, "limit").is_err());
        assert!(validate_order(Some("asc")).is_ok());
        assert!(validate_order(Some("sideways")).is_err());
    }

    // ---------------------------------------------------------------------
    // Permission + Question API v2 HTTP parity tests
    // ---------------------------------------------------------------------

    /// Minimum session schema mirroring the columns referenced by
    /// `v2::COLUMNS`. Enough for `v2::get()` used by the permission/question
    /// session-scoped handlers to succeed.
    fn v2_permission_schema(conn: &rusqlite::Connection) {
        conn.execute_batch(
            "CREATE TABLE session (
                id text PRIMARY KEY,
                parent_id text,
                project_id text NOT NULL,
                agent text,
                model text,
                cost real NOT NULL DEFAULT 0,
                tokens_input integer NOT NULL DEFAULT 0,
                tokens_output integer NOT NULL DEFAULT 0,
                tokens_reasoning integer NOT NULL DEFAULT 0,
                tokens_cache_read integer NOT NULL DEFAULT 0,
                tokens_cache_write integer NOT NULL DEFAULT 0,
                time_created integer NOT NULL DEFAULT 0,
                time_updated integer NOT NULL DEFAULT 0,
                time_archived integer,
                title text NOT NULL DEFAULT '',
                directory text NOT NULL DEFAULT '/tmp',
                workspace_id text,
                path text,
                revert text,
                permission text
             );
             CREATE TABLE permission (
                id text PRIMARY KEY,
                project_id text NOT NULL,
                action text NOT NULL,
                resource text NOT NULL,
                time_created integer NOT NULL,
                time_updated integer NOT NULL,
                UNIQUE(project_id, action, resource)
             );",
        )
        .unwrap();
    }

    fn insert_session(conn: &rusqlite::Connection, id: &str) {
        conn.execute(
            "INSERT INTO session (id, project_id) VALUES (?, 'prj_x')",
            [id],
        )
        .unwrap();
    }

    fn test_app() -> App {
        let bus = bus::Bus::new();
        let pool = r2d2::Pool::builder()
            .max_size(1)
            .build(SqliteConnectionManager::memory())
            .expect("pool");
        v2_permission_schema(&pool.get().unwrap());
        App {
            pool,
            bus: bus.clone(),
            project_id: "prj_x".into(),
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
            pty: crate::pty::Registry::new(bus),
            pty_tickets: crate::pty::TicketRegistry::default(),
            permissions: permission_v2::Registry::new(),
            questions: question_v2::Registry::new(),
        }
    }

    async fn read_json(response: axum::response::Response) -> (StatusCode, Value) {
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("body");
        let value = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).expect("json")
        };
        (status, value)
    }

    fn location_query() -> Query<ApiLocationQuery> {
        Query(ApiLocationQuery {
            location_directory: None,
            location_workspace: None,
        })
    }

    #[tokio::test]
    async fn permission_request_list_wraps_location_response_with_pending_items() {
        let app = test_app();
        insert_session(&app.pool.get().unwrap(), "ses_permlist000000000000000000");
        // Nothing pending yet.
        let (status, value) = read_json(
            api_permission_request_list(State(app.clone()), HeaderMap::new(), location_query())
                .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["data"], json!([]));

        // Insert one pending via the internal ask path.
        let (status, created) = read_json(
            api_session_permission_create(
                State(app.clone()),
                Path("ses_permlist000000000000000000".into()),
                Json(ApiPermissionCreatePayload {
                    id: Some("per_askedaaaaaaaaaaaaaaaaaa".into()),
                    action: "read".into(),
                    resources: vec![".env".into()],
                    save: Some(vec![".env".into()]),
                    metadata: None,
                    source: None,
                    agent: Some("build".into()),
                }),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(created["data"]["effect"], "ask");
        assert_eq!(created["data"]["id"], "per_askedaaaaaaaaaaaaaaaaaa");

        let (status, listed) = read_json(
            api_permission_request_list(State(app.clone()), HeaderMap::new(), location_query())
                .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listed["data"][0]["id"], "per_askedaaaaaaaaaaaaaaaaaa");
        assert_eq!(
            listed["data"][0]["sessionID"],
            "ses_permlist000000000000000000"
        );
        assert_eq!(listed["data"][0]["action"], "read");
    }

    #[tokio::test]
    async fn session_permission_create_returns_session_not_found_for_missing_session() {
        let app = test_app();
        let (status, value) = read_json(
            api_session_permission_create(
                State(app),
                Path("ses_missingaaaaaaaaaaaaaaaaa".into()),
                Json(ApiPermissionCreatePayload {
                    id: None,
                    action: "read".into(),
                    resources: vec![".env".into()],
                    save: None,
                    metadata: None,
                    source: None,
                    agent: Some("build".into()),
                }),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(value["_tag"], "SessionNotFoundError");
        assert_eq!(value["sessionID"], "ses_missingaaaaaaaaaaaaaaaaa");
    }

    #[tokio::test]
    async fn session_permission_get_enforces_session_ownership_and_returns_tagged_error() {
        let app = test_app();
        insert_session(&app.pool.get().unwrap(), "ses_ownerownerownerownerownera");
        insert_session(&app.pool.get().unwrap(), "ses_othersothersothersothersa");
        let _ = api_session_permission_create(
            State(app.clone()),
            Path("ses_ownerownerownerownerownera".into()),
            Json(ApiPermissionCreatePayload {
                id: Some("per_ownedaaaaaaaaaaaaaaaaaa".into()),
                action: "read".into(),
                resources: vec![".env".into()],
                save: None,
                metadata: None,
                source: None,
                agent: Some("build".into()),
            }),
        )
        .await
        .unwrap();

        // Owner can retrieve.
        let (status, value) = read_json(
            api_session_permission_get(
                State(app.clone()),
                Path((
                    "ses_ownerownerownerownerownera".into(),
                    "per_ownedaaaaaaaaaaaaaaaaaa".into(),
                )),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["data"]["id"], "per_ownedaaaaaaaaaaaaaaaaaa");

        // Non-owner sees NotFound tagged the same way.
        let (status, value) = read_json(
            api_session_permission_get(
                State(app),
                Path((
                    "ses_othersothersothersothersa".into(),
                    "per_ownedaaaaaaaaaaaaaaaaaa".into(),
                )),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(value["_tag"], "PermissionNotFoundError");
        assert_eq!(value["requestID"], "per_ownedaaaaaaaaaaaaaaaaaa");
    }

    #[tokio::test]
    async fn session_permission_reply_once_settles_pending_and_supports_always_saving() {
        let app = test_app();
        insert_session(&app.pool.get().unwrap(), "ses_replyaaaaaaaaaaaaaaaaaaa");
        // Create -> reply once (204).
        let _ = api_session_permission_create(
            State(app.clone()),
            Path("ses_replyaaaaaaaaaaaaaaaaaaa".into()),
            Json(ApiPermissionCreatePayload {
                id: Some("per_reponceaaaaaaaaaaaaaaaa".into()),
                action: "read".into(),
                resources: vec![".env".into()],
                save: None,
                metadata: None,
                source: None,
                agent: Some("build".into()),
            }),
        )
        .await
        .unwrap();
        let (status, _) = read_json(
            api_session_permission_reply(
                State(app.clone()),
                Path((
                    "ses_replyaaaaaaaaaaaaaaaaaaa".into(),
                    "per_reponceaaaaaaaaaaaaaaaa".into(),
                )),
                Json(ApiPermissionReplyPayload {
                    reply: "once".into(),
                    message: None,
                }),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(app.permissions.get("per_reponceaaaaaaaaaaaaaaaa").is_none());

        // A second reply for the same ID now returns tagged NotFound.
        let (status, value) = read_json(
            api_session_permission_reply(
                State(app.clone()),
                Path((
                    "ses_replyaaaaaaaaaaaaaaaaaaa".into(),
                    "per_reponceaaaaaaaaaaaaaaaa".into(),
                )),
                Json(ApiPermissionReplyPayload {
                    reply: "once".into(),
                    message: None,
                }),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(value["_tag"], "PermissionNotFoundError");

        // "always" with save[] persists to the saved table.
        let _ = api_session_permission_create(
            State(app.clone()),
            Path("ses_replyaaaaaaaaaaaaaaaaaaa".into()),
            Json(ApiPermissionCreatePayload {
                id: Some("per_repalwaysaaaaaaaaaaaaa".into()),
                action: "read".into(),
                resources: vec!["config/.env".into()],
                save: Some(vec!["config/.env".into()]),
                metadata: None,
                source: None,
                agent: Some("build".into()),
            }),
        )
        .await
        .unwrap();
        let _ = api_session_permission_reply(
            State(app.clone()),
            Path((
                "ses_replyaaaaaaaaaaaaaaaaaaa".into(),
                "per_repalwaysaaaaaaaaaaaaa".into(),
            )),
            Json(ApiPermissionReplyPayload {
                reply: "always".into(),
                message: None,
            }),
        )
        .await
        .unwrap();
        let saved = permission_v2::saved_list(&app.pool.get().unwrap(), Some("prj_x")).unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].resource, "config/.env");
        assert_eq!(saved[0].action, "read");
    }

    #[tokio::test]
    async fn permission_saved_list_and_remove_round_trip() {
        let app = test_app();
        permission_v2::saved_add(&app.pool.get().unwrap(), "prj_x", "read", &[".env"]).unwrap();
        let (status, listed) = read_json(
            api_permission_saved_list(
                State(app.clone()),
                Query(ApiPermissionSavedQuery { project_id: None }),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let items = listed["data"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        let id = items[0]["id"].as_str().unwrap().to_string();
        assert_eq!(items[0]["projectID"], "prj_x");
        assert_eq!(items[0]["action"], "read");
        assert_eq!(items[0]["resource"], ".env");

        let (status, _) = read_json(
            api_permission_saved_remove(State(app.clone()), Path(id))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(
            permission_v2::saved_list(&app.pool.get().unwrap(), Some("prj_x"))
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn session_question_reply_reject_and_ownership_are_enforced() {
        let app = test_app();
        insert_session(&app.pool.get().unwrap(), "ses_qownerownerownerowneraaaa");
        insert_session(&app.pool.get().unwrap(), "ses_qothersothersothersothea");

        // Enqueue directly through the core registry (no ask endpoint exposed).
        let outcome = question_v2::ask(
            &question_v2::Env {
                bus: &app.bus,
                registry: &app.questions,
            },
            &question_v2::AskInput {
                session_id: "ses_qownerownerownerowneraaaa".into(),
                questions: vec![question_v2::Info {
                    question: "Continue?".into(),
                    header: "confirm".into(),
                    options: vec![],
                    multiple: None,
                    custom: None,
                }],
                tool: None,
                timeout: Some(30),
            },
        );

        // Cross-session request retrieval returns tagged NotFound.
        let (status, value) = read_json(
            api_session_question_reply(
                State(app.clone()),
                Path(("ses_qothersothersothersothea".into(), outcome.id.clone())),
                Json(ApiQuestionReplyPayload {
                    answers: vec![vec!["Yes".into()]],
                }),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(value["_tag"], "QuestionNotFoundError");
        assert_eq!(value["requestID"], outcome.id);

        // Session list only returns items owned by the session.
        let (_, listed) = read_json(
            api_session_question_list(
                State(app.clone()),
                Path("ses_qownerownerownerowneraaaa".into()),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(listed["data"][0]["id"], outcome.id);
        let (_, empty) = read_json(
            api_session_question_list(
                State(app.clone()),
                Path("ses_qothersothersothersothea".into()),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(empty["data"], json!([]));

        // Owner rejects — 204 and the ask fiber sees Rejected.
        let (status, _) = read_json(
            api_session_question_reject(
                State(app.clone()),
                Path(("ses_qownerownerownerowneraaaa".into(), outcome.id.clone())),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(
            outcome.wait.recv().unwrap(),
            question_v2::Resolution::Rejected
        );
    }

    #[tokio::test]
    async fn question_request_list_wraps_location_response() {
        let app = test_app();
        insert_session(&app.pool.get().unwrap(), "ses_qlistlistlistlistlistlist");
        // Empty first.
        let (status, value) = read_json(
            api_question_request_list(State(app.clone()), HeaderMap::new(), location_query()).await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["data"], json!([]));

        let outcome = question_v2::ask(
            &question_v2::Env {
                bus: &app.bus,
                registry: &app.questions,
            },
            &question_v2::AskInput {
                session_id: "ses_qlistlistlistlistlistlist".into(),
                questions: vec![question_v2::Info {
                    question: "?".into(),
                    header: "h".into(),
                    options: vec![],
                    multiple: None,
                    custom: None,
                }],
                tool: None,
                timeout: Some(30),
            },
        );

        let (_, listed) = read_json(
            api_question_request_list(State(app.clone()), HeaderMap::new(), location_query()).await,
        )
        .await;
        assert_eq!(listed["data"][0]["id"], outcome.id);
        assert_eq!(
            listed["data"][0]["sessionID"],
            "ses_qlistlistlistlistlistlist"
        );
        assert_eq!(listed["data"][0]["questions"][0]["question"], "?");
    }
}
