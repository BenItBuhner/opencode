//! Wire-compatible port of the session row projection from
//! packages/opencode/src/session/session.ts (`fromRow`) and the list query
//! (`listByProject`). Field names and optionality match the TS `Session.Info`
//! schema exactly; `None` fields are omitted from JSON the same way
//! `JSON.stringify` drops `undefined`.

use rusqlite::{Connection, Row};
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
pub struct Tokens {
    pub input: i64,
    pub output: i64,
    pub reasoning: i64,
    pub cache: TokensCache,
}

#[derive(Serialize)]
pub struct TokensCache {
    pub read: i64,
    pub write: i64,
}

#[derive(Serialize)]
pub struct Summary {
    pub additions: i64,
    pub deletions: i64,
    pub files: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diffs: Option<Value>,
}

#[derive(Serialize)]
pub struct Share {
    pub url: String,
}

#[derive(Serialize)]
pub struct Time {
    pub created: i64,
    pub updated: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compacting: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived: Option<i64>,
}

#[derive(Serialize)]
pub struct Info {
    pub id: String,
    pub slug: String,
    #[serde(rename = "projectID")]
    pub project_id: String,
    #[serde(rename = "workspaceID", skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub directory: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(rename = "parentID", skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<Value>,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<Summary>,
    pub cost: f64,
    pub tokens: Tokens,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share: Option<Share>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revert: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission: Option<Value>,
    pub time: Time,
}

pub const COLUMNS: &str = "id, project_id, workspace_id, parent_id, slug, directory, path, title, \
     version, share_url, summary_additions, summary_deletions, summary_files, summary_diffs, \
     metadata, cost, tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, \
     tokens_cache_write, revert, permission, agent, model, time_created, time_updated, \
     time_compacting, time_archived";

fn json_column(row: &Row, index: usize) -> rusqlite::Result<Option<Value>> {
    let raw: Option<String> = row.get(index)?;
    Ok(raw.and_then(|text| serde_json::from_str(&text).ok()))
}

pub fn from_row(row: &Row) -> rusqlite::Result<Info> {
    let summary_additions: Option<i64> = row.get(10)?;
    let summary_deletions: Option<i64> = row.get(11)?;
    let summary_files: Option<i64> = row.get(12)?;
    let summary =
        if summary_additions.is_some() || summary_deletions.is_some() || summary_files.is_some() {
            Some(Summary {
                additions: summary_additions.unwrap_or(0),
                deletions: summary_deletions.unwrap_or(0),
                files: summary_files.unwrap_or(0),
                diffs: json_column(row, 13)?,
            })
        } else {
            None
        };
    let share_url: Option<String> = row.get(9)?;

    Ok(Info {
        id: row.get(0)?,
        project_id: row.get(1)?,
        workspace_id: row.get(2)?,
        parent_id: row.get(3)?,
        slug: row.get(4)?,
        directory: row.get(5)?,
        path: row.get(6)?,
        title: row.get(7)?,
        version: row.get(8)?,
        share: share_url.map(|url| Share { url }),
        summary,
        metadata: json_column(row, 14)?,
        cost: row.get(15)?,
        tokens: Tokens {
            input: row.get(16)?,
            output: row.get(17)?,
            reasoning: row.get(18)?,
            cache: TokensCache {
                read: row.get(19)?,
                write: row.get(20)?,
            },
        },
        revert: json_column(row, 21)?,
        permission: json_column(row, 22)?,
        agent: row.get(23)?,
        model: json_column(row, 24)?,
        time: Time {
            created: row.get(25)?,
            updated: row.get(26)?,
            compacting: row.get(27)?,
            archived: row.get(28)?,
        },
    })
}

#[derive(Default)]
pub struct ListFilter {
    pub directory: Option<String>,
    pub path: Option<String>,
    pub roots: bool,
    pub start: Option<i64>,
    pub search: Option<String>,
    pub limit: Option<i64>,
}

pub fn list(
    conn: &Connection,
    project_id: &str,
    filter: &ListFilter,
) -> rusqlite::Result<Vec<Info>> {
    let mut sql = format!("SELECT {COLUMNS} FROM session WHERE project_id = ?");
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(project_id.to_string())];

    if let Some(path) = filter.path.as_ref().filter(|value| !value.is_empty()) {
        // Mirrors listByProject: match the path or any nested path, optionally
        // falling back to the exact directory for legacy rows without a path.
        match filter.directory.as_ref() {
            Some(directory) => {
                sql.push_str(" AND (path = ? OR path LIKE ? OR (path IS NULL AND directory = ?))");
                params.push(Box::new(path.clone()));
                params.push(Box::new(format!("{path}/%")));
                params.push(Box::new(directory.clone()));
            }
            None => {
                sql.push_str(" AND (path = ? OR path LIKE ?)");
                params.push(Box::new(path.clone()));
                params.push(Box::new(format!("{path}/%")));
            }
        }
    } else if filter.path.is_none() {
        if let Some(directory) = filter.directory.as_ref() {
            sql.push_str(" AND directory = ?");
            params.push(Box::new(directory.clone()));
        }
    }
    if filter.roots {
        sql.push_str(" AND parent_id IS NULL");
    }
    if let Some(start) = filter.start {
        sql.push_str(" AND time_updated >= ?");
        params.push(Box::new(start));
    }
    if let Some(search) = filter.search.as_ref() {
        sql.push_str(" AND title LIKE ?");
        params.push(Box::new(format!("%{search}%")));
    }
    sql.push_str(" ORDER BY time_updated DESC LIMIT ?");
    params.push(Box::new(filter.limit.unwrap_or(100)));

    let mut statement = conn.prepare_cached(&sql)?;
    let rows = statement.query_map(
        rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
        from_row,
    )?;
    rows.collect()
}

pub fn get(conn: &Connection, id: &str) -> rusqlite::Result<Option<Info>> {
    let sql = format!("SELECT {COLUMNS} FROM session WHERE id = ?");
    let mut statement = conn.prepare_cached(&sql)?;
    let mut rows = statement.query_map([id], from_row)?;
    rows.next().transpose()
}
