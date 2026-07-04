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

mod identifier;
mod session;
mod slug;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use r2d2_sqlite::SqliteConnectionManager;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

type Pool = r2d2::Pool<SqliteConnectionManager>;

#[derive(Clone)]
struct App {
    pool: Pool,
    project_id: String,
    directory: String,
    path: String,
    version: String,
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
    Ok(Json(serde_json::to_value(created).expect("serializable")))
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
    Ok(Json(serde_json::to_value(updated).expect("serializable")))
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
        project_id: project_id.clone(),
        directory: directory.clone(),
        path,
        version: std::env::var("OPENCODE_VERSION").unwrap_or_else(|_| "local".into()),
    };

    let router = Router::new()
        .route("/session", get(list_sessions).post(create_session))
        .route("/session/{id}", get(get_session).patch(update_session))
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
