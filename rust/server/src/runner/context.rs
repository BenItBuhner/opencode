//! System Context Epoch handling for the Rust runner.
//!
//! Port of packages/core/src/session/context-epoch.ts plus the built-in
//! Context Sources (core/environment, core/date, core/instructions,
//! core/skill-guidance, core/reference-guidance). An existing epoch's
//! persisted baseline is reused verbatim; new Sessions get a freshly
//! generated baseline + snapshot in the same shape Bun writes, so either
//! server can later reconcile the epoch.

use crate::config;
use rusqlite::Connection;
use serde_json::{json, Map, Value};

pub struct Epoch {
    pub baseline: String,
    pub baseline_seq: i64,
}

pub fn ensure(
    conn: &Connection,
    session_id: &str,
    directory: &str,
    worktree: &str,
) -> rusqlite::Result<Epoch> {
    let stored: Option<(String, i64)> = conn
        .query_row(
            "SELECT baseline, baseline_seq FROM session_context_epoch WHERE session_id = ?",
            [session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    if let Some((baseline, baseline_seq)) = stored {
        return Ok(Epoch {
            baseline,
            baseline_seq,
        });
    }
    let generation = generate(directory, worktree);
    let baseline_seq: i64 = conn
        .query_row(
            "SELECT seq FROM event_sequence WHERE aggregate_id = ?",
            [session_id],
            |row| row.get(0),
        )
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(-1),
            other => Err(other),
        })?;
    conn.execute(
        "INSERT INTO session_context_epoch (session_id, baseline, snapshot, baseline_seq) \
         VALUES (?, ?, ?, ?)",
        rusqlite::params![
            session_id,
            generation.0,
            generation.1.to_string(),
            baseline_seq
        ],
    )?;
    Ok(Epoch {
        baseline: generation.0,
        baseline_seq,
    })
}

/// Builds (baseline text, snapshot JSON) exactly like SystemContext.initialize
/// over the built-in sources.
fn generate(directory: &str, worktree: &str) -> (String, Value) {
    let mut parts: Vec<String> = vec![];
    let mut snapshot = Map::new();

    let environment = [
        "<env>".to_string(),
        format!("  Working directory: {directory}"),
        format!("  Workspace root folder: {worktree}"),
        format!(
            "  Is directory a git repo: {}",
            if std::path::Path::new(worktree).join(".git").exists() {
                "yes"
            } else {
                "no"
            }
        ),
        format!("  Platform: {}", platform()),
        "</env>".to_string(),
    ]
    .join("\n");
    parts.push(format!(
        "Here is some useful information about the environment you are running in:\n{environment}"
    ));
    snapshot.insert("core/environment".into(), json!({ "value": environment }));

    let date = js_date_string();
    parts.push(format!("Today's date: {date}"));
    snapshot.insert("core/date".into(), json!({ "value": date }));

    let instructions = instruction_files(directory, worktree);
    if !instructions.is_empty() {
        parts.push(
            instructions
                .iter()
                .map(|(path, content)| format!("Instructions from: {path}\n{content}"))
                .collect::<Vec<_>>()
                .join("\n\n"),
        );
        snapshot.insert(
            "core/instructions".into(),
            json!({
                "value": instructions
                    .iter()
                    .map(|(path, content)| json!({ "path": path, "content": content }))
                    .collect::<Vec<_>>(),
                "removed": "Previously loaded instructions no longer apply.",
            }),
        );
    }

    let skills = skill_summaries(worktree);
    parts.push(render_skills(&skills));
    snapshot.insert(
        "core/skill-guidance".into(),
        json!({
            "value": skills
                .iter()
                .map(|(name, description)| json!({ "name": name, "description": description }))
                .collect::<Vec<_>>(),
            "removed": "Skill guidance is no longer available. Do not use any previously listed skill.",
        }),
    );

    let references = reference_summaries(directory, worktree);
    if !references.is_empty() {
        parts.push(render_references(&references));
        snapshot.insert(
            "core/reference-guidance".into(),
            json!({
                "value": references
                    .iter()
                    .map(|(name, path, description)| json!({ "name": name, "path": path, "description": description }))
                    .collect::<Vec<_>>(),
                "removed": "Project reference guidance is no longer available. Do not use previously listed references.",
            }),
        );
    }

    (parts.join("\n\n"), Value::Object(snapshot))
}

fn platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        _ => "linux",
    }
}

/// Date#toDateString: "Sat Jul 04 2026".
fn js_date_string() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before epoch")
        .as_millis() as i64;
    let days = millis.div_euclid(86_400_000);
    let weekday = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][days.rem_euclid(7) as usize];
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
    let month_name = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][(month - 1) as usize];
    format!("{weekday} {month_name} {day:02} {year}")
}

/// InstructionContext.observe: global config AGENTS.md first, then AGENTS.md
/// discovered walking up from the directory to the worktree (nearest first).
fn instruction_files(directory: &str, worktree: &str) -> Vec<(String, String)> {
    let mut paths: Vec<std::path::PathBuf> =
        vec![std::path::Path::new(&config::paths().config).join("AGENTS.md")];
    let mut current = std::path::PathBuf::from(directory);
    let stop = std::path::PathBuf::from(worktree);
    loop {
        paths.push(current.join("AGENTS.md"));
        if current == stop || !current.pop() {
            break;
        }
        if !current.starts_with(&stop) {
            break;
        }
    }
    paths
        .into_iter()
        .filter_map(|path| {
            let content = std::fs::read_to_string(&path).ok()?;
            Some((path.to_string_lossy().into_owned(), content))
        })
        .collect()
}

fn skill_summaries(worktree: &str) -> Vec<(String, String)> {
    let mut skills: Vec<(String, String)> = vec![(
        "customize-opencode".into(),
        "Use ONLY when the user is editing or creating opencode's own configuration: opencode.json, opencode.jsonc, files under .opencode/, or files under ~/.config/opencode/. Also use when creating or fixing opencode agents, subagents, commands, skills, plugins, MCP servers, or permission rules. Do not use for the user's own application code, or for any project that is not configuring opencode itself.".into(),
    )];
    for skill in crate::markdown_skills(worktree) {
        let name = skill
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let description = skill
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !name.is_empty() && !description.is_empty() {
            skills.push((name.to_string(), description.to_string()));
        }
    }
    skills.sort_by(|a, b| a.0.cmp(&b.0));
    skills.dedup_by(|a, b| a.0 == b.0);
    skills
}

fn render_skills(skills: &[(String, String)]) -> String {
    let mut lines = vec![
        "Skills provide specialized instructions and workflows for specific tasks.".to_string(),
        "Use the skill tool to load a skill when a task matches its description.".to_string(),
    ];
    if skills.is_empty() {
        lines.push("No skills are currently available.".into());
        return lines.join("\n");
    }
    lines.push("<available_skills>".into());
    for (name, description) in skills {
        lines.push("  <skill>".into());
        lines.push(format!("    <name>{name}</name>"));
        lines.push(format!("    <description>{description}</description>"));
        lines.push("  </skill>".into());
    }
    lines.push("</available_skills>".into());
    lines.join("\n")
}

/// Reference.Service materialization: local paths expand `~`; repositories
/// resolve to the shared repos cache directory.
fn reference_summaries(directory: &str, worktree: &str) -> Vec<(String, String, String)> {
    let merged = config::instance(directory, worktree);
    let Some(references) = merged.get("references").and_then(Value::as_object) else {
        return vec![];
    };
    let home = std::env::var("HOME").unwrap_or_default();
    let mut out: Vec<(String, String, String)> = references
        .iter()
        .filter_map(|(name, source)| {
            let description = source.get("description")?.as_str()?.to_string();
            if source
                .get("hidden")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return None;
            }
            let path = match (
                source.get("path").and_then(Value::as_str),
                source.get("repository").and_then(Value::as_str),
            ) {
                (Some(path), _) => path.replace('~', &home),
                (None, Some(repository)) => format!(
                    "{home}/.local/share/opencode/repos/{}",
                    repository.trim_end_matches(".git")
                ),
                _ => return None,
            };
            Some((name.clone(), path, description))
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn render_references(references: &[(String, String, String)]) -> String {
    let mut lines = vec![
        "Project references provide additional directories that can be accessed when relevant."
            .to_string(),
        "<available_references>".to_string(),
    ];
    for (name, path, description) in references {
        lines.push("  <reference>".into());
        lines.push(format!("    <name>{name}</name>"));
        lines.push(format!("    <path>{path}</path>"));
        lines.push(format!("    <description>{description}</description>"));
        lines.push("  </reference>".into());
    }
    lines.push("</available_references>".into());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_matches_bun_epoch_for_same_directory() {
        // The Bun server persisted an epoch for /workspace/packages/opencode;
        // the Rust generator must produce the same structure (allowing for the
        // date changing between runs).
        let (baseline, snapshot) = generate("/workspace/packages/opencode", "/workspace");
        assert!(baseline.starts_with(
            "Here is some useful information about the environment you are running in:\n<env>\n  Working directory: /workspace/packages/opencode"
        ));
        assert!(baseline.contains("Instructions from: /workspace/packages/opencode/AGENTS.md"));
        assert!(baseline.contains("Instructions from: /workspace/AGENTS.md"));
        assert!(baseline.contains("<available_skills>"));
        assert!(baseline.contains("<name>customize-opencode</name>"));
        assert!(baseline.contains("<name>effect</name>"));
        assert!(baseline.contains("<available_references>"));
        assert!(snapshot["core/environment"]["value"].is_string());
        assert!(snapshot["core/instructions"]["removed"]
            .as_str()
            .unwrap()
            .starts_with("Previously loaded instructions"));
        let skills = snapshot["core/skill-guidance"]["value"].as_array().unwrap();
        assert!(skills.iter().any(|s| s["name"] == "customize-opencode"));
        let references = snapshot["core/reference-guidance"]["value"]
            .as_array()
            .unwrap();
        assert!(references.iter().any(|r| r["name"] == "opencode-local"));
    }

    #[test]
    fn date_matches_to_date_string_format() {
        let date = js_date_string();
        let parts: Vec<&str> = date.split(' ').collect();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[2].len(), 2);
    }
}
