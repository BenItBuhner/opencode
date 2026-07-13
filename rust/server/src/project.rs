//! Wire-compatible port of the project read path from
//! packages/opencode/src/project/project.ts (`fromRow`, `list`, `get`).

use rusqlite::{Connection, Row};
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
pub struct Icon {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(rename = "override", skip_serializing_if = "Option::is_none")]
    pub override_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

#[derive(Serialize)]
pub struct Time {
    pub created: i64,
    pub updated: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initialized: Option<i64>,
}

#[derive(Serialize)]
pub struct Info {
    pub id: String,
    pub worktree: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vcs: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<Icon>,
    pub time: Time,
    pub sandboxes: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commands: Option<Value>,
}

const COLUMNS: &str = "id, worktree, vcs, name, icon_url, icon_url_override, icon_color, \
     time_created, time_updated, time_initialized, sandboxes, commands";

fn from_row(row: &Row) -> rusqlite::Result<Info> {
    let icon_url: Option<String> = row.get(4)?;
    let icon_override: Option<String> = row.get(5)?;
    let icon_color: Option<String> = row.get(6)?;
    let icon = if icon_url.is_some() || icon_override.is_some() || icon_color.is_some() {
        Some(Icon {
            url: icon_url,
            override_url: icon_override,
            color: icon_color,
        })
    } else {
        None
    };
    let sandboxes: String = row.get(10)?;
    let commands: Option<String> = row.get(11)?;
    Ok(Info {
        id: row.get(0)?,
        worktree: row.get(1)?,
        vcs: row.get(2)?,
        name: row.get(3)?,
        icon,
        time: Time {
            created: row.get(7)?,
            updated: row.get(8)?,
            initialized: row.get(9)?,
        },
        sandboxes: serde_json::from_str(&sandboxes).unwrap_or(Value::Array(vec![])),
        commands: commands.and_then(|text| serde_json::from_str(&text).ok()),
    })
}

pub fn list(conn: &Connection) -> rusqlite::Result<Vec<Info>> {
    let mut statement = conn.prepare_cached(&format!("SELECT {COLUMNS} FROM project"))?;
    let rows = statement.query_map([], from_row)?;
    rows.collect()
}

pub fn get(conn: &Connection, id: &str) -> rusqlite::Result<Option<Info>> {
    let mut statement =
        conn.prepare_cached(&format!("SELECT {COLUMNS} FROM project WHERE id = ?"))?;
    let mut rows = statement.query_map([id], from_row)?;
    rows.next().transpose()
}
